use super::support::{attach, esc, path_text, stamp, stopped, valid_target};
use super::{Operation, execute};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

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
    let info = backend::run("qemu-img", &["info", "--output=json", path_text(&path)?])?;
    let value: serde_json::Value = serde_json::from_str(&info)?;
    let format = value["format"].as_str().context("Could not identify the disk format.")?;
    let xml = format!(
        "<disk type='file' device='disk'><driver name='qemu' type='{}'/><source file='{}'/><target dev='{}' bus='{}'/></disk>",
        esc(format),
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
    stopped(c, id)?;
    valid_target(&target)?;
    if !(1..=16384).contains(&size_gib) {
        bail!("Use a disk size between 1 and 16384 GB.");
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
    );
    // A failed connection does not prove that attachment failed; keep the new disk.
    result?;
    Ok(())
}

pub(super) fn apply_detach_disk(c: Connection, machine: &Machine, target: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_target(&target)?;
    stopped(c, id)?;
    backend::virsh(c, &["detach-disk", id, &target, "--config"])?;
    Ok(())
}
