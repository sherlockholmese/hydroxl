use super::support::{editable, stamp, valid_name};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::{fs, path::PathBuf};

pub(super) fn apply_snapshot_create(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&name)?;
    let internal = format!("hydroxl-{}", stamp());
    backend::virsh(c, &["snapshot-create-as", id, "--name", &internal, "--atomic"])?;
    set_snapshot_label(id, &internal, Some(name.trim()))?;
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
    set_snapshot_label(id, &name, None)?;
    Ok(())
}

pub(super) fn apply_snapshot_rename(
    _c: Connection,
    machine: &Machine,
    old: String,
    new: String,
) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&new)?;
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
    let title = snapshot_labels(id).get(&name).cloned().unwrap_or_else(|| name.clone());
    let replacement = format!("hydroxl-{}", stamp());
    backend::virsh(c, &["snapshot-create-as", id, "--name", &replacement, "--atomic"])?;
    set_snapshot_label(id, &replacement, Some(&title))?;
    backend::virsh(c, &["snapshot-delete", id, &name])?;
    set_snapshot_label(id, &name, None)?;
    Ok(())
}

pub(super) fn labels_path(id: &str) -> Result<PathBuf> {
    if !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        bail!("Invalid machine identity.");
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .context("Set XDG_DATA_HOME to save snapshot names.")?;
    let folder = base.join("hydroxl/snapshots");
    fs::create_dir_all(&folder)?;
    Ok(folder.join(format!("{id}.json")))
}

pub(super) fn snapshot_labels(id: &str) -> std::collections::BTreeMap<String, String> {
    labels_path(id)
        .ok()
        .and_then(|p| fs::read(p).ok())
        .and_then(|s| serde_json::from_slice(&s).ok())
        .unwrap_or_default()
}

pub(super) fn set_snapshot_label(id: &str, name: &str, label: Option<&str>) -> Result<()> {
    let mut labels = snapshot_labels(id);
    if let Some(label) = label {
        labels.insert(name.into(), label.into());
    } else {
        labels.remove(name);
    }
    let path = labels_path(id)?;
    let temporary = path.with_extension("json.new");
    fs::write(&temporary, serde_json::to_vec_pretty(&labels)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}
