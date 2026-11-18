use super::support::{editable, esc, replace_child, stopped, temporary_xml, valid_name};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};

pub(super) fn apply_notes(c: Connection, machine: &Machine, notes: String) -> Result<()> {
    let id = machine.id.as_str();
    stopped(c, id)?;
    if notes.chars().count() > 16384 {
        bail!("Keep notes under 16384 characters.");
    }
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let updated = replace_child(
        &xml,
        "domain",
        "description",
        &format!("<description>{}</description>", esc(&notes)),
    )?;
    temporary_xml(&updated, |path| backend::virsh(c, &["define", path]).map(|_| ()))?;
    Ok(())
}

pub(super) fn apply_autostart(c: Connection, machine: &Machine, enabled: bool) -> Result<()> {
    let id = machine.id.as_str();
    let args = if enabled {
        vec!["autostart", id]
    } else {
        vec!["autostart", id, "--disable"]
    };
    backend::virsh(c, &args)?;
    Ok(())
}

pub(super) fn apply_rename(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&name)?;
    stopped(c, id)?;
    backend::virsh(c, &["domrename", id, name.trim()])?;
    Ok(())
}

pub(super) fn apply_save_xml(c: Connection, machine: &Machine, xml: String) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    let doc = roxmltree::Document::parse(&xml).context("Fix the XML before saving.")?;
    let uuid =
        doc.root_element().children().find(|n| n.has_tag_name("uuid")).and_then(|n| n.text());
    if !doc.root_element().has_tag_name("domain") || uuid != Some(id) {
        bail!("Keep the domain type and this machine's UUID.");
    }
    temporary_xml(&xml, |path| backend::virsh(c, &["define", path]).map(|_| ()))?;
    Ok(())
}

const LIBOSINFO: &str = "http://libosinfo.org/xmlns/libvirt/domain/1.0";

/// Records the guest operating system in libvirt's libosinfo metadata, where
/// virt-install and virt-manager read it too. Applies to the running guest as well.
pub(super) fn apply_operating_system(c: Connection, machine: &Machine, uri: String) -> Result<()> {
    let id = machine.id.as_str();
    if uri.chars().any(|ch| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'')) {
        bail!("Choose an operating system from the list.");
    }
    let running =
        matches!(machine.state, crate::backend::State::Running | crate::backend::State::Paused);
    let mut args = vec!["metadata", id, LIBOSINFO, "--key", "libosinfo", "--config"];
    if running {
        args.push("--live");
    }
    let element = format!("<os id='{}'/>", esc(&uri));
    if uri.is_empty() {
        args.push("--remove");
    } else {
        args.extend(["--set", &element]);
    }
    backend::virsh(c, &args)?;
    Ok(())
}
