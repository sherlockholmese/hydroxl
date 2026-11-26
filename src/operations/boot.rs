use super::support::{editable, esc, replace_child};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};

pub(super) fn apply_firmware(c: Connection, machine: &Machine, uefi: bool) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    xml = replace_child(&xml, "os", "loader", "")?;
    xml = replace_child(&xml, "os", "nvram", "")?;
    xml = replace_child(&xml, "os", "firmware", "")?;
    let doc = roxmltree::Document::parse(&xml)?;
    let os = doc
        .descendants()
        .find(|n| n.has_tag_name("os"))
        .context("The boot configuration is missing.")?;
    let start = os.range().start;
    let end = start + xml[start..].find('>').context("The boot configuration is invalid.")? + 1;
    xml.replace_range(
        start..end,
        if uefi {
            "<os firmware='efi'>"
        } else {
            "<os>"
        },
    );
    backend::define_xml(c, &xml)?;
    Ok(())
}

pub(super) fn apply_system(
    c: Connection,
    machine: &Machine,
    cpu: Option<String>,
    machine_type: String,
    boot: String,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    if let Some(cpu) = cpu {
        let doc = roxmltree::Document::parse(&xml)?;
        let cpu_node = doc.root_element().children().find(|n| n.has_tag_name("cpu"));
        let topology = cpu_node
            .and_then(|cpu| cpu.children().find(|n| n.has_tag_name("topology")))
            .map(|node| &xml[node.range()])
            .unwrap_or("");
        let cpu_xml = match cpu.as_str() {
            "host-passthrough" => format!("<cpu mode='host-passthrough'>{topology}</cpu>"),
            "host-model" => format!("<cpu mode='host-model'>{topology}</cpu>"),
            "default" if !topology.is_empty() => format!("<cpu>{topology}</cpu>"),
            "default" => String::new(),
            _ if !cpu.is_empty()
                && cpu.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
            {
                format!(
                    "<cpu mode='custom' match='exact'><model fallback='forbid'>{}</model>{topology}</cpu>",
                    esc(&cpu)
                )
            }
            _ => bail!("Choose a valid CPU model."),
        };
        xml = replace_child(&xml, "domain", "cpu", &cpu_xml)?;
    }
    let doc = roxmltree::Document::parse(&xml)?;
    let os = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("os"))
        .context("Boot configuration is missing.")?;
    let mut changes = Vec::new();
    if !machine_type.trim().is_empty() {
        if !machine_type.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            bail!("Use a valid QEMU machine type.");
        }
        let kind = os
            .children()
            .find(|n| n.has_tag_name("type"))
            .context("The machine type is missing.")?;
        let arch = kind.attribute("arch").unwrap_or(&machine.arch);
        changes.push((
            kind.range(),
            format!("<type arch='{}' machine='{}'>hvm</type>", esc(arch), esc(machine_type.trim())),
        ));
    }
    // An empty boot device leaves boot order untouched; per-device order is set separately.
    if boot.is_empty() {
        changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in changes {
            xml.replace_range(range, &replacement);
        }
        backend::define_xml(c, &xml)?;
        return Ok(());
    }
    if !["hd", "cdrom", "network"].contains(&boot.as_str()) {
        bail!("Choose a boot device.");
    }
    let boot_nodes: Vec<_> = os.children().filter(|n| n.has_tag_name("boot")).collect();
    if let Some(first) = boot_nodes.first() {
        changes.push((first.range(), format!("<boot dev='{}'/>", boot)));
        for n in boot_nodes.iter().skip(1) {
            changes.push((n.range(), String::new()));
        }
    } else {
        let end = os.range().end - "</os>".len();
        changes.push((end..end, format!("<boot dev='{}'/>", boot)));
    }
    // Per-device boot order cannot coexist with global OS boot order.
    for n in doc.descendants().filter(|n| n.has_tag_name("boot") && n.attribute("order").is_some())
    {
        changes.push((n.range(), String::new()));
    }
    changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in changes {
        xml.replace_range(range, &replacement);
    }
    backend::define_xml(c, &xml)?;
    Ok(())
}

/// Per-device boot order. Entries are `disk:<target>` or `net:<mac>`, first boots first;
/// devices not listed are not tried. Replaces any global `<os><boot dev>` order.
pub(super) fn apply_boot_order(c: Connection, machine: &Machine, order: Vec<String>) -> Result<()> {
    if order.is_empty() {
        bail!("Turn on at least one boot device.");
    }
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let mut changes: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for n in doc.descendants().filter(|n| n.has_tag_name("boot")) {
        let in_os = n.parent().is_some_and(|p| p.has_tag_name("os"));
        if in_os || n.attribute("order").is_some() {
            changes.push((n.range(), String::new()));
        }
    }
    for (position, entry) in order.iter().enumerate() {
        let node = if let Some(target) = entry.strip_prefix("disk:") {
            doc.descendants().find(|n| {
                n.has_tag_name("disk")
                    && n.children()
                        .any(|c| c.has_tag_name("target") && c.attribute("dev") == Some(target))
            })
        } else if let Some(mac) = entry.strip_prefix("net:") {
            doc.descendants().find(|n| {
                n.has_tag_name("interface")
                    && n.children()
                        .any(|c| c.has_tag_name("mac") && c.attribute("address") == Some(mac))
            })
        } else {
            None
        }
        .with_context(|| format!("The boot device {entry} no longer exists."))?;
        let close = format!("</{}>", node.tag_name().name());
        let end = node.range().end;
        if !xml[..end].ends_with(&close) {
            bail!("The boot device {entry} cannot take a boot order.");
        }
        let at = end - close.len();
        changes.push((at..at, format!("<boot order='{}'/>", position + 1)));
    }
    changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in changes {
        xml.replace_range(range, &replacement);
    }
    backend::define_xml(c, &xml)?;
    Ok(())
}
