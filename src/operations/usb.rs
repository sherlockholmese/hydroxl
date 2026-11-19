use super::support::temporary_xml;
use crate::backend::{self, Connection, Machine, State};
use anyhow::{Context, Result};
use std::fs;

pub(super) fn apply_usb_attach(
    c: Connection,
    machine: &Machine,
    bus: String,
    device: String,
) -> Result<()> {
    usb(c, machine, &bus, &device, true)?;
    Ok(())
}

pub(super) fn apply_usb_detach(
    c: Connection,
    machine: &Machine,
    bus: String,
    device: String,
) -> Result<()> {
    usb(c, machine, &bus, &device, false)?;
    Ok(())
}

pub(super) fn usb(
    c: Connection,
    machine: &Machine,
    bus: &str,
    device: &str,
    attach: bool,
) -> Result<()> {
    let bus: u16 = bus.parse().context("Use a numeric USB bus.")?;
    let device: u16 = device.parse().context("Use a numeric USB device.")?;
    let mode = if machine.state == State::Stopped {
        "--config"
    } else {
        "--live"
    };
    change_usb(c, &machine.id, bus, device, mode, attach)
}

fn change_usb(
    c: Connection,
    id: &str,
    bus: u16,
    device: u16,
    mode: &str,
    attach: bool,
) -> Result<()> {
    let xml = format!(
        "<hostdev mode='subsystem' type='usb' managed='yes'><source><address bus='{bus}' device='{device}'/></source></hostdev>"
    );
    temporary_xml(&xml, |path| {
        backend::virsh(
            c,
            &[
                if attach {
                    "attach-device"
                } else {
                    "detach-device"
                },
                id,
                path,
                mode,
            ],
        )
        .map(|_| ())
    })
}

/// Same-host viewer attachment. The saved domain is never edited.
pub(crate) fn toggle_live_usb(c: Connection, id: &str, bus: u16, device: u16) -> Result<bool> {
    let xml = backend::virsh(c, &["dumpxml", id])?;
    let doc = roxmltree::Document::parse(&xml)?;
    anyhow::ensure!(
        doc.root_element().attribute("id").is_some(),
        "Start the guest to connect USB devices."
    );
    let attached = doc.descendants().any(|node| {
        node.has_tag_name("hostdev")
            && node.attribute("type") == Some("usb")
            && node
                .children()
                .find(|child| child.has_tag_name("source"))
                .and_then(|source| source.children().find(|child| child.has_tag_name("address")))
                .is_some_and(|address| {
                    address.attribute("bus").and_then(|n| n.parse().ok()) == Some(bus)
                        && address.attribute("device").and_then(|n| n.parse().ok()) == Some(device)
                })
    });
    change_usb(c, id, bus, device, "--live", !attached)?;
    Ok(!attached)
}

pub fn host_usb() -> Vec<(String, String)> {
    let mut devices = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return devices;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let read = |name| fs::read_to_string(path.join(name)).ok().map(|s| s.trim().to_owned());
        let (Some(bus), Some(device), Some(class)) =
            (read("busnum"), read("devnum"), read("bDeviceClass"))
        else {
            continue;
        };
        if class == "09" {
            continue;
        } // Root and external USB hubs are not pass-through targets.
        let product = read("product").unwrap_or_else(|| "USB device".into());
        let maker = read("manufacturer").unwrap_or_default();
        let name = if maker.is_empty() || product.starts_with(&maker) {
            product
        } else {
            format!("{maker} {product}")
        };
        devices.push((format!("{bus}:{device}"), format!("{name} (bus {bus}, device {device})")));
    }
    devices.sort_by(|a, b| a.1.cmp(&b.1));
    devices
}
