use crate::backend::{self, Connection};
pub(super) use crate::{files::path_text, xml::escape as esc};
use anyhow::{Context, Result, bail};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn stamp() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default()
}

pub(super) fn stopped(c: Connection, id: &str) -> Result<()> {
    backend::ensure_stopped(c, id, false)
}

pub(super) fn temporary_xml<T>(xml: &str, f: impl FnOnce(&str) -> Result<T>) -> Result<T> {
    let file = crate::xml::temporary(xml)?;
    f(path_text(file.path())?)
}

pub(super) fn attach(c: Connection, id: &str, xml: &str) -> Result<()> {
    editable(c, id)?;
    temporary_xml(xml, |path| {
        backend::virsh(c, &["attach-device", id, path, "--config"]).map(|_| ())
    })
}

pub(super) fn valid_target(target: &str) -> Result<()> {
    if target.is_empty() || target.len() > 12 || !target.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        bail!("Use a disk target such as vdb or sdb.");
    }
    Ok(())
}

pub(super) fn valid_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        bail!("Use a name between 1 and 64 characters.");
    }
    Ok(())
}

pub(super) fn replace_child(
    xml: &str,
    parent: &str,
    tag: &str,
    replacement: &str,
) -> Result<String> {
    let doc =
        roxmltree::Document::parse(xml).context("Could not read the machine configuration XML.")?;
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name(parent))
        .with_context(|| format!("The machine configuration is missing its {parent} section."))?;
    if let Some(child) = node.children().find(|n| n.has_tag_name(tag)) {
        let mut result = xml.to_owned();
        result.replace_range(child.range(), replacement);
        Ok(result)
    } else if !replacement.is_empty() {
        append_child(xml, parent, replacement)
    } else {
        Ok(xml.to_owned())
    }
}

pub(super) fn append_child(xml: &str, parent: &str, child: &str) -> Result<String> {
    let doc =
        roxmltree::Document::parse(xml).context("Could not read the machine configuration XML.")?;
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name(parent))
        .with_context(|| format!("The machine configuration is missing its {parent} section."))?;
    let mut result = xml.to_owned();
    let (range, replacement) = crate::xml::append_child(xml, node, child)?;
    result.replace_range(range, &replacement);
    Ok(result)
}

pub(super) fn editable(c: Connection, id: &str) -> Result<()> {
    backend::ensure_stopped(c, id, true)
}

/// Attaches a device to the saved configuration of a stopped machine.
pub(super) fn attach_xml(c: Connection, id: &str, xml: &str) -> Result<()> {
    attach(c, id, xml)
}
