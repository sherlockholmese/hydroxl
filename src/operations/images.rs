use super::support::{editable, path_text, stamp, valid_target};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::{fs, path::Path};

pub(super) fn apply_compact_disk(c: Connection, machine: &Machine, target: String) -> Result<()> {
    compact_disk(c, machine, &target)?;
    Ok(())
}

pub(super) fn apply_resize_disk(
    c: Connection,
    machine: &Machine,
    target: String,
    size_gib: u64,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    valid_target(&target)?;
    if !(1..=16384).contains(&size_gib) {
        bail!("Use a disk size between 1 and 16384 GB.");
    }
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let disk = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("disk")
                && n.children().any(|c| {
                    c.has_tag_name("target") && c.attribute("dev") == Some(target.as_str())
                })
        })
        .context("Disk not found. Refresh and try again.")?;
    let path = disk
        .children()
        .find(|n| n.has_tag_name("source"))
        .and_then(|n| n.attribute("file"))
        .context("Only file-backed disks can be resized here.")?;
    let info: serde_json::Value =
        serde_json::from_str(&backend::run("qemu-img", &["info", "--output=json", path])?)?;
    if size_gib * 1024 * 1024 * 1024 <= info["virtual-size"].as_u64().unwrap_or(u64::MAX) {
        bail!("Choose a size larger than the current disk.");
    }
    backend::run("qemu-img", &["resize", path, &format!("{size_gib}G")])?;
    Ok(())
}

pub(super) fn compact_disk(c: Connection, machine: &Machine, target: &str) -> Result<()> {
    editable(c, &machine.id)?;
    valid_target(target)?;
    if !backend::virsh(c, &["snapshot-list", &machine.id, "--name"])?.trim().is_empty() {
        bail!("Remove snapshots before compacting a disk.");
    }
    let xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let disk = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("disk")
                && n.children()
                    .any(|n| n.has_tag_name("target") && n.attribute("dev") == Some(target))
        })
        .context("Disk not found. Refresh and try again.")?;
    let source = disk
        .children()
        .find(|n| n.has_tag_name("source"))
        .and_then(|n| n.attribute("file"))
        .context("Compaction requires a file-backed disk.")?;
    let info: serde_json::Value =
        serde_json::from_str(&backend::run("qemu-img", &["info", "--output=json", source])?)?;
    if info["format"] != "qcow2" {
        bail!("Compaction requires a QCOW2 disk.");
    }
    let original = Path::new(source);
    if fs::symlink_metadata(original)?.file_type().is_symlink() {
        bail!("Use the disk's actual file path before compacting it.");
    }
    let temporary = original.with_file_name(format!(".hydroxl-compact-{}.qcow2", stamp()));
    let result = (|| -> Result<()> {
        backend::run(
            "qemu-img",
            &["convert", "-c", "-O", "qcow2", source, path_text(&temporary)?],
        )?;
        // A runtime integrity check precedes replacing a guest's only disk copy.
        backend::run(
            "qemu-img",
            &["compare", "-f", "qcow2", "-F", "qcow2", source, path_text(&temporary)?],
        )?;
        fs::set_permissions(&temporary, fs::metadata(original)?.permissions())?;
        fs::rename(&temporary, original)
            .context("Could not replace the disk. The original was kept.")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
