//! Machine removal keeps storage by default. Destructive removal is limited to
//! Hydroxl-owned files and refuses sharing, backing dependencies and special devices.
use super::support::{editable, stopped};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn apply_remove(c: Connection, machine: &Machine, delete_disks: bool) -> Result<()> {
    let id = machine.id.as_str();
    stopped(c, id)?;
    editable(c, id)?;
    ensure!(
        backend::virsh(c, &["snapshot-list", id, "--name"])?.trim().is_empty(),
        "Remove snapshots before removing this machine from the library."
    );
    let disks = if delete_disks {
        deletion_plan(c, id)?
    } else {
        Vec::new()
    };
    // Recheck after the potentially slow sharing scan. Undefine can turn an active
    // guest transient, so also confirm it is absent before unlinking any storage.
    stopped(c, id)?;
    backend::virsh(c, &["undefine", id, "--keep-nvram"])?;
    if !disks.is_empty() {
        let remaining = backend::virsh(c, &["list", "--all", "--uuid"])
            .context("The machine definition was removed, but its shutdown state could not be verified. Disk files were kept.")?;
        ensure!(
            !remaining.lines().any(|line| line.trim() == id),
            "The definition was removed, but the machine is still active. Disk files were kept."
        );
    }
    let mut failed = Vec::new();
    for disk in disks {
        if let Err(error) = fs::remove_file(&disk) {
            eprintln!("Could not delete {}: {error}", disk.display());
            failed.push(disk.display().to_string());
        }
    }
    if !failed.is_empty() {
        bail!(
            "The machine was removed, but these disk files could not be deleted: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn deletion_plan(c: Connection, id: &str) -> Result<Vec<PathBuf>> {
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let root = backend::data_dir()?.canonicalize().context("Could not locate Hydroxl storage.")?;
    let disks = owned_disks(&xml, &root)?;
    if disks.is_empty() {
        return Ok(disks);
    }
    // Inspect every other domain, including its live and persistent configurations.
    let ids = backend::virsh(c, &["list", "--all", "--uuid"])?;
    for other in ids.lines().map(str::trim).filter(|other| !other.is_empty() && *other != id) {
        let live = backend::virsh(c, &["dumpxml", other])?;
        check_references(c, &live, &disks)?;
        // Transient domains have no inactive definition. dominfo is authoritative.
        let info = backend::virsh(c, &["dominfo", other])?;
        if info.lines().any(|line| {
            line.starts_with("Persistent:") && line.split_whitespace().last() == Some("yes")
        }) {
            let inactive = backend::virsh(c, &["dumpxml", other, "--inactive"])?;
            check_references(c, &inactive, &disks)?;
        }
    }
    Ok(disks)
}

fn owned_disks(xml: &str, root: &Path) -> Result<Vec<PathBuf>> {
    let doc = roxmltree::Document::parse(xml)?;
    let uuid =
        doc.root_element().children().find(|n| n.has_tag_name("uuid")).and_then(|n| n.text());
    let mut disks = BTreeSet::new();
    for disk in doc
        .descendants()
        .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
    {
        ensure!(
            !disk.children().any(|n| n.has_tag_name("readonly") || n.has_tag_name("shareable")),
            "Shared or read-only disks cannot be deleted. Remove the machine with disk files kept."
        );
        let source = disk.children().find(|n| n.has_tag_name("source"))
            .and_then(|n| n.attribute("file"))
            .context("Only file-backed Hydroxl disks can be deleted. Keep disk files to remove this machine.")?;
        let path = Path::new(source)
            .canonicalize()
            .context("Could not locate a disk file. Keep disk files to remove this machine.")?;
        ensure!(
            (path.starts_with(root) && path != root)
                || uuid.is_some_and(|uuid| owns_machine_folder(&path, uuid)),
            "This disk is outside a Hydroxl-owned machine folder. Keep disk files to remove this machine."
        );
        let metadata = fs::metadata(&path)?;
        ensure!(metadata.is_file(), "Only regular disk files can be deleted.");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.nlink() == 1,
                "A disk has other hard links. Keep disk files to remove this machine."
            );
        }
        disks.insert(path);
    }
    Ok(disks.into_iter().collect())
}

// Custom creation locations have a machine.xml manifest alongside their disks.
// A matching UUID and disk source distinguish those from attached external images.
fn owns_machine_folder(path: &Path, uuid: &str) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(xml) = fs::read_to_string(parent.join("machine.xml")) else {
        return false;
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return false;
    };
    doc.root_element().has_tag_name("domain")
        && doc.root_element().children().any(|n| n.has_tag_name("uuid") && n.text() == Some(uuid))
        && doc
            .descendants()
            .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
            .flat_map(|n| n.children())
            .filter(|n| n.has_tag_name("source"))
            .filter_map(|n| n.attribute("file"))
            .any(|source| Path::new(source).canonicalize().is_ok_and(|source| source == path))
}

fn check_references(c: Connection, xml: &str, deleting: &[PathBuf]) -> Result<()> {
    let doc = roxmltree::Document::parse(xml)?;
    for disk in doc.descendants().filter(|n| n.has_tag_name("disk")) {
        for source in disk.descendants().filter(|n| n.has_tag_name("source")) {
            let path = if let Some(file) = source.attribute("file") {
                Some(PathBuf::from(file))
            } else if let (Some(pool), Some(volume)) =
                (source.attribute("pool"), source.attribute("volume"))
            {
                Some(PathBuf::from(
                    backend::virsh(c, &["vol-path", volume, "--pool", pool])?.trim(),
                ))
            } else {
                None
            };
            let Some(path) = path else {
                continue;
            };
            let path = path
                .canonicalize()
                .context("Could not check another machine's storage; no disks were deleted.")?;
            ensure!(
                !deleting.contains(&path),
                "A disk is attached to another machine. Keep disk files to remove this machine."
            );
            if disk.attribute("device") != Some("disk") {
                continue;
            }
            let chain: serde_json::Value = serde_json::from_str(&backend::run(
                "qemu-img",
                &[
                    "info",
                    "--force-share",
                    "--backing-chain",
                    "--output=json",
                    path.to_str().context("A disk path is not valid UTF-8.")?,
                ],
            )?)?;
            for image in chain.as_array().context("Could not inspect a disk backing chain.")? {
                let filename =
                    image["filename"].as_str().context("A backing image has no filename.")?;
                let filename = Path::new(filename);
                let resolved = if filename.is_absolute() {
                    filename.to_owned()
                } else {
                    path.parent().unwrap_or(Path::new("/")).join(filename)
                };
                let resolved =
                    resolved.canonicalize().context("Could not locate a backing image.")?;
                ensure!(
                    !deleting.contains(&resolved),
                    "Another machine depends on this disk as a backing image. Keep disk files to remove this machine."
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_rejects_external_shared_and_device_disks() {
        let dir = std::env::temp_dir().join(format!("hydroxl-removal-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("disk.qcow2");
        fs::write(&file, b"test").unwrap();
        let xml = |extra: &str, source: &str| {
            format!(
                "<domain><devices><disk device='disk'>{extra}{source}</disk></devices></domain>"
            )
        };
        let source = format!("<source file='{}'/>", file.display());
        assert_eq!(owned_disks(&xml("", &source), &dir).unwrap(), vec![file.clone()]);
        assert!(owned_disks(&xml("<shareable/>", &source), &dir).is_err());
        assert!(owned_disks(&xml("<readonly/>", &source), &dir).is_err());
        assert!(owned_disks(&xml("", &source), &dir.join("other")).is_err());
        assert!(owned_disks(&xml("", "<source dev='/dev/sda'/>"), &dir).is_err());
        let managed = xml("", &source).replace("<domain>", "<domain><uuid>test-machine</uuid>");
        fs::write(dir.join("machine.xml"), &managed).unwrap();
        assert_eq!(owned_disks(&managed, &dir.join("other")).unwrap(), vec![file.clone()]);
        assert!(
            owned_disks(&managed.replace("test-machine", "different-machine"), &dir.join("other"))
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
