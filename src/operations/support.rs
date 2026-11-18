use crate::backend::{self, Connection};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn stamp() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default()
}

pub(super) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    path.to_str().context("Choose a path with a valid UTF-8 name.")
}

pub(super) fn stopped(c: Connection, id: &str) -> Result<()> {
    backend::ensure_stopped(c, id, false)
}

pub(super) fn temporary_xml<T>(xml: &str, f: impl FnOnce(&str) -> Result<T>) -> Result<T> {
    let path = std::env::temp_dir().join(format!("hydroxl-{}-{}.xml", std::process::id(), stamp()));
    fs::write(&path, xml).context("Could not write the configuration.")?;
    let result = f(path_text(&path)?);
    let _ = fs::remove_file(path);
    result
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
    let doc = roxmltree::Document::parse(xml)?;
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name(parent))
        .context("The configuration section is missing.")?;
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
    let doc = roxmltree::Document::parse(xml)?;
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name(parent))
        .context("The configuration section is missing.")?;
    let text = &xml[node.range()];
    let mut result = xml.to_owned();
    if text.ends_with("/>") {
        let start = node.range().start;
        let end = node.range().end;
        result.replace_range(
            start..end,
            &format!("{}>{}</{}>", &text[..text.len() - 2], child, parent),
        );
    } else {
        let at = node.range().end - parent.len() - 3;
        result.insert_str(at, child);
    }
    Ok(result)
}

pub(super) fn editable(c: Connection, id: &str) -> Result<()> {
    backend::ensure_stopped(c, id, true)
}

/// Attaches a device to the saved configuration of a stopped machine.
pub(super) fn attach_xml(c: Connection, id: &str, xml: &str) -> Result<()> {
    attach(c, id, xml)
}
