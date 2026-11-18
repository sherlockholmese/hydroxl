use super::manifest::hash_file;
use crate::backend::{self, Connection};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
fn text(path: &Path) -> Result<&str> {
    path.to_str().context("Use a path with a valid UTF-8 name.")
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}
pub fn export_ova(
    c: Connection,
    machine: &crate::backend::Machine,
    destination: &Path,
) -> Result<()> {
    if crate::backend::State::from_virsh(&backend::virsh(c, &["domstate", &machine.id])?)
        != crate::backend::State::Stopped
        || machine.saved_state
    {
        bail!("Power off the machine and discard its saved state before exporting.");
    }
    if destination.exists() {
        bail!("Choose a new OVA file name.");
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let folder = backend::data_dir()?.join(format!("export-{stamp}"));
    fs::create_dir(&folder)?;
    let result = (|| -> Result<()> {
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
            let info: serde_json::Value = serde_json::from_str(&backend::run(
                "qemu-img",
                &["info", "--output=json", source],
            )?)?;
            let capacity = info["virtual-size"].as_u64().context("Could not read a disk size.")?;
            let i = index + 1;
            let filename = format!("disk-{i}.vmdk");
            let path = folder.join(&filename);
            backend::run(
                "qemu-img",
                &["convert", "-O", "vmdk", "-o", "subformat=streamOptimized", source, text(&path)?],
            )?;
            let size = fs::metadata(&path)?.len();
            files.push_str(&format!(
                "<File ovf:id='file{i}' ovf:href='{filename}' ovf:size='{size}'/>"
            ));
            disks.push_str(&format!("<Disk ovf:diskId='disk{i}' ovf:fileRef='file{i}' ovf:capacity='{capacity}' ovf:capacityAllocationUnits='byte' ovf:format='http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized'/>"));
            hardware.push_str(&format!("<Item><rasd:AddressOnParent>{index}</rasd:AddressOnParent><rasd:ElementName>Disk {i}</rasd:ElementName><rasd:HostResource>ovf:/disk/disk{i}</rasd:HostResource><rasd:InstanceID>{}</rasd:InstanceID><rasd:Parent>3</rasd:Parent><rasd:ResourceType>17</rasd:ResourceType></Item>", 10 + index));
            names.push(filename);
        }
        let count = doc.descendants().filter(|n| n.has_tag_name("interface")).count();
        let mut networks = String::new();
        for i in 0..count {
            networks.push_str(&format!("<Network ovf:name='Network {}'><Description>Shared network</Description></Network>", i+1));
            hardware.push_str(&format!("<Item><rasd:Connection>Network {}</rasd:Connection><rasd:ElementName>Network adapter {}</rasd:ElementName><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceSubType>E1000</rasd:ResourceSubType><rasd:ResourceType>10</rasd:ResourceType></Item>", i+1, i+1, 100+i));
        }
        let firmware = if doc.descendants().any(|n| {
            n.has_tag_name("loader")
                || (n.has_tag_name("os") && n.attribute("firmware") == Some("efi"))
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
        // Commit only a finished archive, beside the requested destination so rename is atomic.
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temporary = parent.join(format!(".hydroxl-export-{stamp}.ova"));
        let archive_result = (|| -> Result<()> {
            let file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            let mut archive = tar::Builder::new(file);
            archive.append_path_with_name(&descriptor_path, "machine.ovf")?;
            archive.append_path_with_name(folder.join("machine.mf"), "machine.mf")?;
            for name in names.iter().skip(1) {
                archive.append_path_with_name(folder.join(name), name)?;
            }
            archive.finish()?;
            let file = archive.into_inner()?;
            file.sync_all()?;
            // hard_link fails if another writer created the destination in the meantime.
            if fs::hard_link(&temporary, destination).is_err() {
                // FAT and some removable volumes do not support hard links.
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(destination)
                    .context("Could not save the OVA. Choose a new file name.")?;
                let copied = (|| -> Result<()> {
                    let mut input = File::open(&temporary)?;
                    std::io::copy(&mut input, &mut output)?;
                    output.sync_all()?;
                    Ok(())
                })();
                if copied.is_err() {
                    let _ = fs::remove_file(destination);
                }
                copied?;
            }
            Ok(())
        })();
        let _ = fs::remove_file(temporary);
        archive_result
    })();
    let _ = fs::remove_dir_all(folder);
    result
}
