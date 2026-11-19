use super::ShareDriver;
use super::support::{append_child, attach, editable, esc, path_text, temporary_xml, valid_name};
use crate::backend::{self, Connection, Machine};
use anyhow::{Result, bail};
use std::path::PathBuf;

pub(super) fn apply_share(
    c: Connection,
    machine: &Machine,
    path: PathBuf,
    tag: String,
    readonly: bool,
    driver: ShareDriver,
) -> Result<()> {
    let id = machine.id.as_str();
    if !path.is_dir() {
        bail!("Choose an existing shared folder.");
    }
    valid_name(&tag)?;
    if tag.chars().count() > 36 {
        bail!("Use a mount tag of 36 characters or fewer.");
    }
    let readonly = if readonly {
        "<readonly/>"
    } else {
        ""
    };
    let xml = match driver {
        ShareDriver::NineP => format!(
            "<filesystem type='mount' accessmode='mapped'><source dir='{}'/><target dir='{}'/>{readonly}</filesystem>",
            esc(path_text(&path)?),
            esc(&tag),
        ),
        ShareDriver::Virtiofs => {
            if !ShareDriver::virtiofs_available() {
                bail!("Install virtiofsd on this computer to share folders with virtio-fs.");
            }
            shared_memory(c, id)?;
            format!(
                "<filesystem type='mount' accessmode='passthrough'><driver type='virtiofs'/>\
                 <source dir='{}'/><target dir='{}'/>{readonly}</filesystem>",
                esc(path_text(&path)?),
                esc(&tag),
            )
        }
    };
    attach(c, id, &xml)?;
    Ok(())
}

/// virtio-fs maps guest memory into virtiofsd, so the guest's RAM must be shareable.
fn shared_memory(c: Connection, id: &str) -> Result<()> {
    editable(c, id)?;
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    match with_shared_memory(&xml)? {
        Some(updated) => {
            temporary_xml(&updated, |path| backend::virsh(c, &["define", path]).map(|_| ()))
        }
        None => Ok(()),
    }
}

/// The domain with shareable RAM, or `None` when it already has it.
pub(super) fn with_shared_memory(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let backing = doc.root_element().children().find(|n| n.has_tag_name("memoryBacking"));
    let shared = backing.is_some_and(|b| {
        b.children().any(|n| n.has_tag_name("access") && n.attribute("mode") == Some("shared"))
    });
    if shared {
        return Ok(None);
    }
    let updated = match backing {
        Some(node) => {
            // Keep hugepages or other settings; add the source and access mode.
            let mut inner = String::new();
            if !node.children().any(|n| n.has_tag_name("source")) {
                inner.push_str("<source type='memfd'/>");
            }
            let access = node.children().find(|n| n.has_tag_name("access"));
            if access.is_none() {
                inner.push_str("<access mode='shared'/>");
            }
            let text = &xml[node.range()];
            let mut result = xml.to_owned();
            if text.ends_with("/>") {
                result.replace_range(
                    node.range(),
                    &format!("<memoryBacking>{inner}</memoryBacking>"),
                );
            } else {
                let at = node.range().end - "</memoryBacking>".len();
                result.insert_str(at, &inner);
                if let Some(access) = access {
                    result.replace_range(access.range(), "<access mode='shared'/>");
                }
            }
            result
        }
        None => append_child(
            xml,
            "domain",
            "<memoryBacking><source type='memfd'/><access mode='shared'/></memoryBacking>",
        )?,
    };
    Ok(Some(updated))
}
