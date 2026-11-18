use super::support::{editable, esc, path_text, stamp, stopped, temporary_xml, valid_name};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::fs;

pub(super) fn apply_clone_configuration(
    c: Connection,
    machine: &Machine,
    name: String,
) -> Result<()> {
    clone_configuration(c, machine, &name)?;
    Ok(())
}

pub(super) fn apply_clone(c: Connection, machine: &Machine, name: String) -> Result<()> {
    let id = machine.id.as_str();
    valid_name(&name)?;
    stopped(c, id)?;
    // virt-clone copies storage and regenerates the UUID and network MACs.
    backend::run(
        "virt-clone",
        &["--connect", c.uri(), "--original", &machine.name, "--name", name.trim(), "--auto-clone"],
    )?;
    Ok(())
}

pub(super) fn clone_configuration(c: Connection, machine: &Machine, name: &str) -> Result<()> {
    editable(c, &machine.id)?;
    valid_name(name)?;
    let folder = backend::data_dir()?.join(format!("template-{}", stamp()));
    fs::create_dir(&folder)?;
    let mut registration_attempted = false;
    let result = (|| -> Result<()> {
        let mut xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
        let doc = roxmltree::Document::parse(&xml)?;
        let mut edits = Vec::new();
        for node in doc.root_element().children() {
            if node.has_tag_name("name") {
                edits.push((node.range(), format!("<name>{}</name>", esc(name.trim()))));
            }
            if node.has_tag_name("uuid") || node.has_tag_name("genid") {
                edits.push((node.range(), String::new()));
            }
        }
        for node in doc.descendants().filter(|n| n.has_tag_name("mac") || n.has_tag_name("nvram")) {
            edits.push((node.range(), String::new()));
        }
        if doc
            .descendants()
            .any(|n| n.has_tag_name("tpm") && n.descendants().any(|c| c.has_tag_name("source")))
        {
            bail!("Remove custom TPM state paths before cloning the configuration.");
        }
        for (i, disk) in doc
            .descendants()
            .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
            .enumerate()
        {
            let source = disk
                .children()
                .find(|n| n.has_tag_name("source"))
                .context("A disk has no source.")?;
            let file = source
                .attribute("file")
                .context("Configuration cloning requires file-backed disks.")?;
            let info: serde_json::Value =
                serde_json::from_str(&backend::run("qemu-img", &["info", "--output=json", file])?)?;
            let size =
                info["virtual-size"].as_u64().context("Could not read a disk size.")?.to_string();
            let new = folder.join(format!("disk-{i}.qcow2"));
            backend::run("qemu-img", &["create", "-f", "qcow2", path_text(&new)?, &size])?;
            edits.push((source.range(), format!("<source file='{}'/>", esc(path_text(&new)?))));
            if let Some(driver) = disk.children().find(|n| n.has_tag_name("driver")) {
                edits.push((driver.range(), "<driver name='qemu' type='qcow2'/>".into()));
            }
        }
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in edits {
            xml.replace_range(range, &replacement);
        }
        registration_attempted = true;
        temporary_xml(&xml, |path| backend::virsh(c, &["define", path]).map(|_| ()))?;
        Ok(())
    })();
    if result.is_err() && !registration_attempted {
        let _ = fs::remove_dir_all(folder);
    }
    result
}
