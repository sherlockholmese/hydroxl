use super::support::{editable, esc, path_text, stopped};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn apply_move_storage(c: Connection, machine: &Machine, path: PathBuf) -> Result<()> {
    move_storage(c, machine, &path)?;
    Ok(())
}

pub(super) fn apply_export(c: Connection, machine: &Machine, destination: PathBuf) -> Result<()> {
    export(c, machine, &destination)?;
    Ok(())
}

pub(super) fn apply_export_ova(
    c: Connection,
    machine: &Machine,
    destination: PathBuf,
) -> Result<()> {
    crate::appliance::export_ova(c, machine, &destination)?;
    Ok(())
}

pub(super) fn export(c: Connection, machine: &Machine, destination: &Path) -> Result<()> {
    copy_storage(c, machine, destination, |xml| {
        fs::write(destination.join("machine.xml"), backend::add_hardware_review(xml)?)?;
        Ok(())
    })
}

fn copy_storage<T>(
    c: Connection,
    machine: &Machine,
    destination: &Path,
    finish: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    stopped(c, &machine.id)?;
    if destination.exists() {
        bail!("Choose a new folder for the export.");
    }
    fs::create_dir_all(destination).context("Could not create the export folder.")?;
    // Export is self-contained for file-backed disks. Never silently omit block storage.
    let result = (|| -> Result<T> {
        let mut xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
        let doc = roxmltree::Document::parse(&xml)?;
        let mut edits = Vec::new();
        for (i, disk) in doc
            .descendants()
            .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
            .enumerate()
        {
            let node = disk
                .children()
                .find(|n| n.has_tag_name("source"))
                .context("A disk has no source.")?;
            let source =
                node.attribute("file").context("Export currently requires file-backed storage.")?;
            let new = destination.join(format!("disk-{i}.qcow2"));
            backend::run("qemu-img", &["convert", "-O", "qcow2", source, path_text(&new)?])?;
            edits.push((node.range(), format!("<source file='{}'/>", esc(path_text(&new)?))));
            if let Some(driver) = disk.children().find(|n| n.has_tag_name("driver")) {
                edits.push((driver.range(), "<driver name='qemu' type='qcow2'/>".into()));
            }
        }
        if let Some(nvram) = doc.descendants().find(|n| n.has_tag_name("nvram")) {
            if let Some(source) = nvram.text().filter(|s| !s.trim().is_empty()) {
                let target = destination.join("nvram.fd");
                fs::copy(source.trim(), &target)
                    .context("Could not copy the UEFI variable store.")?;
                if let Some(value) = nvram.children().find(|n| n.is_text()) {
                    edits.push((value.range(), esc(path_text(&target)?)));
                }
            }
        }
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in edits {
            xml.replace_range(range, &replacement);
        }
        finish(&xml)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

pub(super) fn move_storage(c: Connection, machine: &Machine, destination: &Path) -> Result<()> {
    editable(c, &machine.id)?;
    if !backend::virsh(c, &["snapshot-list", &machine.id, "--name"])?.trim().is_empty() {
        bail!("Remove snapshots before moving storage.");
    }
    // Folder export flattens backing chains into independent QCOW2 files.
    let xml = copy_storage(c, machine, destination, |xml| {
        backend::record_owned_storage(destination, xml)?;
        Ok(xml.to_owned())
    })?;
    // Registration can outlive a failed connection. Keep both copies for recovery.
    backend::define_xml(c, &xml)?;
    // Keep original files as a recovery copy. They may also be used by other VMs.
    // The VM now uses only the destination disks and NVRAM.
    Ok(())
}
