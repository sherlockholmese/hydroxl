use super::support::{editable, stamp, valid_name};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(super) fn apply_snapshot_create(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&name)?;
    let internal = format!("hydroxl-{}", stamp());
    backend::virsh(c, &["snapshot-create-as", id, "--name", &internal, "--atomic"])?;
    set_snapshot_label(id, &internal, Some(name.trim())).with_context(|| format!(
        "The snapshot was saved as {internal}. Rename it in Snapshots after fixing access to the snapshot names file."
    ))?;
    Ok(())
}

pub(super) fn apply_snapshot_restore(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    backend::virsh(c, &["snapshot-revert", id, "--snapshotname", &name])?;
    Ok(())
}

pub(super) fn apply_snapshot_delete(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    backend::virsh(c, &["snapshot-delete", id, "--snapshotname", &name])?;
    set_snapshot_label(id, &name, None)
        .context("The snapshot was deleted. Hydroxl could not remove its saved label.")?;
    Ok(())
}

pub(super) fn apply_snapshot_rename(
    c: Connection,
    machine: &Machine,
    old: String,
    new: String,
) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&new)?;
    backend::virsh(c, &["snapshot-dumpxml", id, &old])?;
    // Rename only the user-facing title. The libvirt snapshot identity and
    // qcow2 internal name remain stable, preserving restore and parent links.
    set_snapshot_label(id, &old, Some(new.trim()))?;
    Ok(())
}

pub(super) fn apply_snapshot_overwrite(
    c: Connection,
    machine: &Machine,
    name: String,
) -> Result<()> {
    let id = machine.id.as_str();
    backend::virsh(c, &["snapshot-dumpxml", id, &name])?;
    let title = snapshot_labels(id)?.remove(&name).unwrap_or_else(|| name.clone());
    let replacement = format!("hydroxl-{}", stamp());
    backend::virsh(c, &["snapshot-create-as", id, "--name", &replacement, "--atomic"])?;
    set_snapshot_label(id, &replacement, Some(&title)).with_context(|| {
        format!(
            "The replacement was saved as {replacement}. The original snapshot is still available."
        )
    })?;
    backend::virsh(c, &["snapshot-delete", id, "--snapshotname", &name]).context(
        "The replacement was saved. Refresh Snapshots to check whether the original was removed.",
    )?;
    set_snapshot_label(id, &name, None)
        .context("The snapshot was replaced. Hydroxl could not remove the old label.")?;
    Ok(())
}

pub(super) fn labels_path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        bail!("Invalid machine identity.");
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .context("Set XDG_DATA_HOME to save snapshot names.")?;
    let folder = base.join("hydroxl/snapshots");
    Ok(folder.join(format!("{id}.json")))
}

pub(super) fn snapshot_labels(id: &str) -> Result<BTreeMap<String, String>> {
    read_labels(&labels_path(id)?)
}

fn read_labels(path: &Path) -> Result<BTreeMap<String, String>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Could not read snapshot names from {}.", path.display())
            });
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("The snapshot names file at {} is invalid.", path.display()))
}

pub(super) fn set_snapshot_label(id: &str, name: &str, label: Option<&str>) -> Result<()> {
    let path = labels_path(id)?;
    update_label(&path, name, label)
        .with_context(|| format!("Could not save snapshot names to {}.", path.display()))
}

fn update_label(path: &Path, name: &str, label: Option<&str>) -> Result<()> {
    let parent = path.parent().context("The snapshot names folder is missing.")?;
    fs::create_dir_all(parent)?;
    // Lock a separate file because publishing the JSON replaces its inode.
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut labels = read_labels(path)?;
    if let Some(label) = label {
        labels.insert(name.into(), label.into());
    } else if labels.remove(name).is_none() {
        return Ok(());
    }
    crate::files::write_atomic(path, &serde_json::to_vec_pretty(&labels)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_corrupt_snapshot_names_intact() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("labels.json");
        fs::write(&file, "{invalid json")?;
        assert!(update_label(&file, "snapshot-id", Some("Still useful")).is_err());
        assert_eq!(fs::read_to_string(file)?, "{invalid json");
        Ok(())
    }

    #[test]
    fn concurrent_writes_preserve_both_snapshot_names() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("labels.json");
        std::thread::scope(|scope| -> Result<()> {
            let writer = scope.spawn(|| update_label(&file, "a", Some("One")));
            update_label(&file, "b", Some("Two"))?;
            writer.join().expect("snapshot writer did not panic")?;
            Ok(())
        })?;
        let labels = read_labels(&file)?;
        assert_eq!(labels.get("a").map(String::as_str), Some("One"));
        assert_eq!(labels.get("b").map(String::as_str), Some("Two"));
        Ok(())
    }
}
