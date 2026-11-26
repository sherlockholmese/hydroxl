use super::support::{attach, editable, esc, path_text, stamp, stopped, valid_target};
use super::{Operation, execute};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub(crate) fn target_prefix(bus: &str) -> Option<&'static str> {
    match bus {
        "virtio" => Some("vd"),
        "sata" | "scsi" | "usb" => Some("sd"),
        "ide" => Some("hd"),
        _ => None,
    }
}

pub(crate) fn validate_disk_target(bus: &str, target: &str) -> Result<()> {
    valid_target(target)?;
    if let Some(prefix) = target_prefix(bus)
        && !target.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_lowercase())
        })
    {
        bail!("Use a target such as {prefix}a or {prefix}b for this bus.");
    }
    Ok(())
}

pub(super) fn apply_disk_interface(
    c: Connection,
    machine: &Machine,
    target: &str,
    new_target: &str,
    bus: &str,
) -> Result<()> {
    let id = &machine.id;
    editable(c, id)?;
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    if let Some(updated) = update_disk_interface(xml, target, new_target, bus)? {
        backend::define_xml(c, &updated)?;
    }
    Ok(())
}

fn update_disk_interface(
    mut xml: String,
    target: &str,
    new_target: &str,
    bus: &str,
) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();
    let devices =
        root.children().find(|n| n.has_tag_name("devices")).context("No drives found.")?;
    let mut targets = devices
        .children()
        .filter(|n| n.has_tag_name("disk"))
        .filter_map(|disk| disk.children().find(|n| n.has_tag_name("target")));
    let old = targets
        .clone()
        .find(|n| n.attribute("dev") == Some(target))
        .context("The drive no longer exists. Refresh the machine and try again.")?;
    let disk = old.parent().context("The drive target has no disk configuration.")?;
    let old_bus = old.attribute("bus").unwrap_or("");
    if bus == old_bus && new_target == target {
        return Ok(None);
    }
    validate_disk_target(bus, new_target)?;
    if bus != old_bus
        && (target_prefix(bus).is_none()
            || (disk.attribute("device") == Some("cdrom") && bus == "virtio"))
    {
        bail!("Choose a supported bus for this drive.");
    }
    if new_target != target && targets.any(|n| n.attribute("dev") == Some(new_target)) {
        bail!("Target {new_target} is already assigned to another drive.");
    }

    let mut replacement = format!("<target dev='{}'", esc(new_target));
    if !bus.is_empty() {
        replacement.push_str(&format!(" bus='{}'", esc(bus)));
    }
    for attr in old.attributes().filter(|attr| !matches!(attr.name(), "dev" | "bus")) {
        replacement.push_str(&format!(" {}='{}'", attr.name(), esc(attr.value())));
    }
    replacement.push_str("/>");
    let mut edits = vec![(old.range(), replacement)];
    if let Some(address) = disk.children().find(|n| n.has_tag_name("address")) {
        edits.push((address.range(), String::new()));
    }
    crate::xml::apply_edits(&mut xml, edits)?;
    Ok(Some(xml))
}

pub(super) fn apply_attach_disk(
    c: Connection,
    machine: &Machine,
    path: PathBuf,
    target: String,
    bus: String,
) -> Result<()> {
    let id = machine.id.as_str();
    valid_target(&target)?;
    if !path.is_file() {
        bail!("Choose an existing disk image.");
    }
    if !["virtio", "sata", "scsi", "ide", "usb"].contains(&bus.as_str()) {
        bail!("Choose a supported disk interface.");
    }
    let info = backend::qemu::disk_info(path_text(&path)?)?;
    let xml = format!(
        "<disk type='file' device='disk'><driver name='qemu' type='{}'/><source file='{}'/><target dev='{}' bus='{}'/></disk>",
        esc(&info.format),
        esc(path_text(&path)?),
        esc(&target),
        esc(&bus)
    );
    attach(c, id, &xml)?;
    Ok(())
}

pub(super) fn apply_new_disk(
    c: Connection,
    machine: &Machine,
    size_gib: u64,
    target: String,
    bus: String,
) -> Result<()> {
    let id = machine.id.as_str();
    super::support::editable(c, id)?;
    valid_target(&target)?;
    validate_disk_target(&bus, &target)?;
    if !(1..=16384).contains(&size_gib) {
        bail!("Use a disk size between 1 and 16384 GiB.");
    }
    let folder = machine
        .disk
        .as_ref()
        .and_then(|p| Path::new(p).parent())
        .context("Import a disk before adding storage.")?;
    let path = folder.join(format!("disk-{}.qcow2", stamp()));
    backend::run(
        "qemu-img",
        &["create", "-f", "qcow2", path_text(&path)?, &format!("{size_gib}G")],
    )?;
    let result = execute(
        c,
        machine,
        Operation::AttachDisk {
            path: path.clone(),
            target,
            bus,
        },
        &Default::default(),
    );
    // A failed connection does not prove that attachment failed; keep the new disk.
    result.with_context(|| format!(
        "The new disk is at {}. Refresh the machine and check whether it was attached before retrying.",
        path.display()
    ))
}

pub(super) fn apply_detach_disk(c: Connection, machine: &Machine, target: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_target(&target)?;
    stopped(c, id)?;
    backend::virsh(c, &["detach-disk", id, &target, "--config"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_the_target_in_place_and_leaves_boot_and_storage_intact() -> Result<()> {
        let xml = "<domain><devices>
          <disk type='file' device='disk'>
            <source file='/guest.qcow2'/><target dev='vda' bus='virtio'/><boot order='1'/>
            <address type='pci'/>
          </disk>
          <disk type='file' device='cdrom'>
            <target dev='sda' bus='sata' tray='open'/>
          </disk>
        </devices></domain>";
        let renamed = update_disk_interface(xml.into(), "vda", "vdc", "virtio")?.unwrap();
        assert!(renamed.contains("<target dev='vdc' bus='virtio'/>"));
        assert!(renamed.contains("<source file='/guest.qcow2'/>"));
        assert!(renamed.contains("<boot order='1'/>"));
        assert!(!renamed.contains("<address"));
        let cdrom = update_disk_interface(renamed, "sda", "sdb", "scsi")?.unwrap();
        assert!(cdrom.contains("<target dev='sdb' bus='scsi' tray='open'/>"));
        assert!(update_disk_interface(xml.into(), "sda", "sda", "sata")?.is_none());
        assert!(update_disk_interface(xml.into(), "vda", "sda", "sata").is_err());
        assert!(update_disk_interface(xml.into(), "vda", "sdb", "virtio").is_err());
        Ok(())
    }
}
