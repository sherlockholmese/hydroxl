use super::manifest::hash_file;
use crate::backend::{self, Connection};
use crate::{files::path_text, xml::escape};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
pub fn export_ova(
    c: Connection,
    machine: &crate::backend::Machine,
    destination: &Path,
) -> Result<()> {
    backend::ensure_stopped(c, &machine.id, true)?;
    if destination.try_exists()? {
        bail!("Choose a new OVA file name.");
    }
    let work = tempfile::Builder::new()
        .prefix("export-")
        .tempdir_in(backend::data_dir()?)
        .context("Could not create a temporary folder for the appliance.")?;
    let folder = work.path();

    let xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let mut files = String::new();
    let mut disks = String::new();
    let mut hardware = String::new();
    let mut names = Vec::new();
    let disk_nodes: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
        .collect();
    if disk_nodes.is_empty() {
        bail!("This machine has no disks to export.");
    }
    for (index, disk) in disk_nodes.iter().enumerate() {
        let source = disk
            .children()
            .find(|n| n.has_tag_name("source"))
            .and_then(|n| n.attribute("file"))
            .context("OVA export requires file-backed disks.")?;
        let capacity = backend::qemu::disk_info(source)?.virtual_size;
        let i = index + 1;
        let filename = format!("disk-{i}.vmdk");
        let path = folder.join(&filename);
        backend::run(
            "qemu-img",
            &[
                "convert",
                "-O",
                "vmdk",
                "-o",
                "subformat=streamOptimized",
                source,
                path_text(&path)?,
            ],
        )?;
        let size = fs::metadata(&path)?.len();
        files
            .push_str(&format!("<File ovf:id='file{i}' ovf:href='{filename}' ovf:size='{size}'/>"));
        disks.push_str(&format!("<Disk ovf:diskId='disk{i}' ovf:fileRef='file{i}' ovf:capacity='{capacity}' ovf:capacityAllocationUnits='byte' ovf:format='http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized'/>"));
        hardware.push_str(&format!("<Item><rasd:AddressOnParent>{index}</rasd:AddressOnParent><rasd:ElementName>Disk {i}</rasd:ElementName><rasd:HostResource>ovf:/disk/disk{i}</rasd:HostResource><rasd:InstanceID>{}</rasd:InstanceID><rasd:Parent>3</rasd:Parent><rasd:ResourceType>17</rasd:ResourceType></Item>", 10 + index));
        names.push(filename);
    }
    let count = doc.descendants().filter(|n| n.has_tag_name("interface")).count();
    let mut networks = String::new();
    for i in 0..count {
        networks.push_str(&format!(
            "<Network ovf:name='Network {}'><Description>Shared network</Description></Network>",
            i + 1
        ));
        hardware.push_str(&format!("<Item><rasd:Connection>Network {}</rasd:Connection><rasd:ElementName>Network adapter {}</rasd:ElementName><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceSubType>E1000</rasd:ResourceSubType><rasd:ResourceType>10</rasd:ResourceType></Item>", i+1, i+1, 100+i));
    }
    let firmware = if doc.descendants().any(|n| {
        n.has_tag_name("loader") || (n.has_tag_name("os") && n.attribute("firmware") == Some("efi"))
    }) {
        "efi"
    } else {
        "bios"
    };
    let descriptor = format!(
        r#"<?xml version='1.0' encoding='UTF-8'?>
<Envelope xmlns='http://schemas.dmtf.org/ovf/envelope/1' xmlns:ovf='http://schemas.dmtf.org/ovf/envelope/1' xmlns:rasd='http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData' xmlns:vssd='http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData' xmlns:vmw='http://www.vmware.com/schema/ovf'>
<References>{files}</References>
<DiskSection><Info>Virtual disks</Info>{disks}</DiskSection>
<NetworkSection><Info>Virtual networks</Info>{networks}</NetworkSection>
<VirtualSystem ovf:id='{name}'><Info>Virtual machine</Info><Name>{name}</Name>
<OperatingSystemSection ovf:id='1'><Info>Guest operating system</Info></OperatingSystemSection>
<VirtualHardwareSection><Info>Virtual hardware</Info>
<System><vssd:ElementName>Virtual hardware</vssd:ElementName><vssd:InstanceID>0</vssd:InstanceID><vssd:VirtualSystemIdentifier>{name}</vssd:VirtualSystemIdentifier><vssd:VirtualSystemType>vmx-13</vssd:VirtualSystemType></System>
<Item><rasd:ElementName>CPU</rasd:ElementName><rasd:InstanceID>1</rasd:InstanceID><rasd:ResourceType>3</rasd:ResourceType><rasd:VirtualQuantity>{cpus}</rasd:VirtualQuantity></Item>
<Item><rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits><rasd:ElementName>Memory</rasd:ElementName><rasd:InstanceID>2</rasd:InstanceID><rasd:ResourceType>4</rasd:ResourceType><rasd:VirtualQuantity>{memory}</rasd:VirtualQuantity></Item>
<Item><rasd:Address>0</rasd:Address><rasd:ElementName>SCSI controller</rasd:ElementName><rasd:InstanceID>3</rasd:InstanceID><rasd:ResourceSubType>lsilogic</rasd:ResourceSubType><rasd:ResourceType>6</rasd:ResourceType></Item>
{hardware}<vmw:Config ovf:required='false' vmw:key='firmware' vmw:value='{firmware}'/><vmw:Config ovf:required='false' vmw:key='architecture' vmw:value='{arch}'/>
</VirtualHardwareSection></VirtualSystem></Envelope>"#,
        name = escape(&machine.name),
        cpus = machine.cpus,
        memory = machine.memory_mib,
        arch = escape(&machine.arch)
    );
    let descriptor_path = folder.join("machine.ovf");
    fs::write(&descriptor_path, descriptor)?;
    names.insert(0, "machine.ovf".into());
    let mut checksums = String::new();
    for name in &names {
        let mut hash = Sha256::new();
        hash_file(&folder.join(name), &mut hash)?;
        checksums.push_str(&format!("SHA256({name})= {:x}\n", hash.finalize()));
    }
    fs::write(folder.join("machine.mf"), checksums)?;
    let mut staged = crate::files::stage(destination)
        .context("Could not create an OVA file in the output folder.")?;
    (|| -> Result<()> {
        let mut archive = tar::Builder::new(staged.as_file_mut());
        archive.append_path_with_name(&descriptor_path, "machine.ovf")?;
        archive.append_path_with_name(folder.join("machine.mf"), "machine.mf")?;
        for name in names.iter().skip(1) {
            archive.append_path_with_name(folder.join(name), name)?;
        }
        archive.finish()?;
        Ok(())
    })()
    .context("Could not write the appliance archive. Check available disk space.")?;
    staged.as_file().sync_all().context("Could not finish writing the appliance archive.")?;
    staged.persist_noclobber(destination)
        .context("Could not save the appliance archive. Check the output folder and choose an unused file name.")?;
    Ok(())
}
