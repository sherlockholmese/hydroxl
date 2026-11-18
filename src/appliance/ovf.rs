use super::{Definition, archive::contained, vmdk::validate_vmdk};
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, fs, path::Path};
fn is(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == name).map(|a| a.value())
}

fn child_text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children().find(|n| is(*n, name)).and_then(|n| n.text())
}

fn memory_mib(quantity: u64, unit: &str) -> Result<u64> {
    let unit = unit.to_lowercase().replace(' ', "");
    let bytes_per_unit = match unit.as_str() {
        "" | "megabytes" | "megabyte" | "mb" | "byte*2^20" | "bytes*2^20" => 1u64 << 20,
        "gigabytes" | "gigabyte" | "gb" | "byte*2^30" | "bytes*2^30" => 1u64 << 30,
        "kilobytes" | "kb" | "byte*2^10" | "bytes*2^10" => 1u64 << 10,
        "byte" | "bytes" => 1,
        "byte*10^6" | "bytes*10^6" => 1_000_000,
        "byte*10^9" | "bytes*10^9" => 1_000_000_000,
        "byte*10^3" | "bytes*10^3" => 1_000,
        _ => bail!("The appliance uses an unsupported memory unit."),
    };
    Ok(quantity
        .checked_mul(bytes_per_unit)
        .context("The appliance memory size is too large.")?
        .div_ceil(1 << 20))
}

pub(crate) fn parse(descriptor: &Path) -> Result<Definition> {
    let xml = fs::read_to_string(descriptor).context("Could not read the OVF descriptor.")?;
    let doc = roxmltree::Document::parse(&xml).context("The OVF descriptor is invalid.")?;
    if !is(doc.root_element(), "Envelope") {
        bail!("Choose an OVF descriptor or an OVA appliance.");
    }
    let systems: Vec<_> = doc.descendants().filter(|n| is(*n, "VirtualSystem")).collect();
    if systems.len() != 1 {
        bail!("Import an appliance containing exactly one virtual machine.");
    }
    let system = systems[0];
    let name = child_text(system, "Name")
        .or_else(|| attr(system, "id"))
        .unwrap_or("Imported machine")
        .to_owned();
    let mut cpus = 1;
    let mut memory = 2048;
    let mut networks = 0;
    let mut attached = Vec::new();
    for item in system.descendants().filter(|n| is(*n, "Item")) {
        match child_text(item, "ResourceType").unwrap_or("").trim() {
            "3" => {
                cpus = child_text(item, "VirtualQuantity")
                    .unwrap_or("1")
                    .trim()
                    .parse()
                    .context("The appliance CPU count is invalid.")?
            }
            "4" => {
                memory = memory_mib(
                    child_text(item, "VirtualQuantity")
                        .unwrap_or("2048")
                        .trim()
                        .parse()
                        .context("The appliance memory size is invalid.")?,
                    child_text(item, "AllocationUnits").unwrap_or("byte * 2^20"),
                )?
            }
            "10" => networks += 1,
            "17" => {
                let resource = child_text(item, "HostResource")
                    .context("An appliance disk has no reference.")?
                    .trim();
                let id = resource
                    .strip_prefix("ovf:/disk/")
                    .or_else(|| resource.strip_prefix("/disk/"))
                    .context("The appliance uses an unsupported disk reference.")?;
                attached.push(id.to_owned());
            }
            _ => {}
        }
    }
    if cpus == 0 || cpus > 256 || !(128..=33_554_432).contains(&memory) {
        bail!("The appliance hardware allocation is outside supported limits.");
    }
    let references: BTreeMap<_, _> = doc
        .descendants()
        .filter(|n| is(*n, "File"))
        .filter_map(|n| Some((attr(n, "id")?.to_owned(), attr(n, "href")?.to_owned())))
        .collect();
    let definitions: BTreeMap<_, _> = doc
        .descendants()
        .filter(|n| is(*n, "Disk"))
        .filter_map(|n| Some((attr(n, "diskId")?.to_owned(), attr(n, "fileRef")?.to_owned())))
        .collect();
    // Respect virtual hardware attachment order. Do not import unused disks as boot disks.
    if attached.is_empty() {
        attached.extend(definitions.keys().cloned());
    }
    if attached.is_empty() {
        bail!("The appliance contains no disk images.");
    }
    let parent = descriptor.parent().context("The descriptor folder is missing.")?;
    let mut disks = Vec::new();
    for id in attached {
        let reference =
            definitions.get(&id).context("A disk is missing from the OVF disk section.")?;
        let href = references
            .get(reference)
            .context("A disk image is missing from the OVF references.")?;
        let path = contained(parent, href)?;
        validate_vmdk(&path, parent)?;
        disks.push(path);
    }
    let uefi = system.descendants().any(|n| {
        (is(n, "Config")
            && attr(n, "key").is_some_and(|v| v.eq_ignore_ascii_case("firmware"))
            && attr(n, "value").is_some_and(|v| v.eq_ignore_ascii_case("efi")))
            || (is(n, "Firmware")
                && attr(n, "type").is_some_and(|v| v.to_ascii_lowercase().contains("efi")))
    });
    let hint = system
        .descendants()
        .filter_map(|n| attr(n, "value").or_else(|| n.text()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let architecture = if hint.contains("aarch64") || hint.contains("arm64") {
        "aarch64"
    } else if hint.contains("i386") || hint.contains("i686") {
        "i686"
    } else {
        "x86_64"
    }
    .to_owned();
    Ok(Definition {
        name,
        cpus,
        memory_mib: memory,
        disks,
        networks,
        uefi,
        architecture,
    })
}
