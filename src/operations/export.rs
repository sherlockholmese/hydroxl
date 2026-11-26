use super::support::{editable, esc, path_text, stopped};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::io::ErrorKind;
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
    copy_storage(c, machine, destination, |destination, xml| {
        crate::files::write_atomic(
            &destination.join("machine.xml"),
            backend::add_hardware_review(xml)?.as_bytes(),
        )
        .context("Could not save the exported machine configuration.")
    })
}

fn copy_storage<T>(
    c: Connection,
    machine: &Machine,
    destination: &Path,
    finish: impl FnOnce(&Path, &str) -> Result<T>,
) -> Result<T> {
    stopped(c, &machine.id)?;
    let name = destination.file_name().context("Enter a name for the export folder.")?;
    let parent =
        destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).context("Could not create the parent folder for the export.")?;
    let destination =
        parent.canonicalize().context("Could not open the destination folder.")?.join(name);
    if let Err(error) = fs::create_dir(&destination) {
        if error.kind() == ErrorKind::AlreadyExists {
            bail!("Choose a new folder for the export.");
        }
        return Err(error).context("Could not create the export folder.");
    }
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
            backend::run("qemu-img", &["convert", "-O", "qcow2", source, path_text(&new)?])
                .with_context(|| {
                    format!(
                        "Could not copy disk {}. Check available space in the destination.",
                        i + 1
                    )
                })?;
            edits.push((node.range(), format!("<source file='{}'/>", esc(path_text(&new)?))));
            if let Some(driver) = disk.children().find(|n| n.has_tag_name("driver")) {
                edits.push((driver.range(), "<driver name='qemu' type='qcow2'/>".into()));
            }
        }
        if let Some(nvram) = doc.descendants().find(|n| n.has_tag_name("nvram"))
            && let Some(source) = nvram.text().filter(|s| !s.trim().is_empty())
        {
            let target = destination.join("nvram.fd");
            fs::copy(source.trim(), &target).context("Could not copy the UEFI variable store.")?;
            if let Some(value) = nvram.children().find(|n| n.is_text()) {
                edits.push((value.range(), esc(path_text(&target)?)));
            }
        }
        crate::xml::apply_edits(&mut xml, edits)?;
        finish(&destination, &xml)
    })();
    match result {
        Err(error) => match fs::remove_dir_all(&destination) {
            Ok(()) => Err(error),
            Err(cleanup) => {
                eprintln!("Could not clean up {}: {cleanup}", destination.display());
                Err(error.context(format!(
                    "Export failed and cleanup could not remove {}.",
                    destination.display()
                )))
            }
        },
        Ok(value) => Ok(value),
    }
}

pub(super) fn move_storage(c: Connection, machine: &Machine, destination: &Path) -> Result<()> {
    editable(c, &machine.id)?;
    if !backend::virsh(c, &["snapshot-list", &machine.id, "--name"])?.trim().is_empty() {
        bail!("Remove snapshots before moving storage.");
    }
    // Folder export flattens backing chains into independent QCOW2 files.
    let xml = copy_storage(c, machine, destination, |destination, xml| {
        backend::record_owned_storage(destination, xml)?;
        Ok(xml.to_owned())
    })?;
    // Registration can outlive a failed connection. Keep both copies for recovery.
    backend::define_xml(c, &xml).with_context(|| format!(
        "Could not confirm the storage move. Copies remain in {}. Refresh and check the disk paths before retrying.",
        destination.display()
    ))?;
    // Keep original files as a recovery copy. They may also be used by other VMs.
    // The VM now uses only the destination disks and NVRAM.
    Ok(())
}
