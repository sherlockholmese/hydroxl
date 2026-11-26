//! Resolve and prepare the guest display configuration for the local viewer.
use super::{Connection, virsh};
use anyhow::{Context, Result, bail};

/// Adds a local, socket-only VNC display (plus a USB tablet for absolute pointing) to a
/// stopped machine that lacks one, so Hydroxl can show the guest in its own window.
pub(super) fn ensure_display(connection: Connection, id: &str) -> Result<()> {
    let original = virsh(connection, &["dumpxml", id, "--inactive"])?;
    if roxmltree::Document::parse(&original)?.descendants().any(is_peer_dbus) {
        return Ok(());
    }
    // GPU scanout for a 2D display is only a SPICE delivery choice; drop it when the
    // VNC viewer is chosen, since QEMU cannot serve VNC alongside a SPICE GL context.
    let xml = super::spice::drop_scanout_gl(&original)?.unwrap_or_else(|| original.clone());
    // The VNC viewer has no audio channel: play guest sound on the host directly.
    let xml = match super::spice::host_audio_backend() {
        Some(backend) => super::spice::route_audio(&xml, backend)?.unwrap_or(xml),
        None => xml,
    };
    // Clipboard over VNC: the guest agent talks to QEMU's built-in agent.
    let xml = if super::qemu::supports_vdagent(connection, &xml).unwrap_or(false) {
        super::spice::route_agent(&xml, true)?.unwrap_or(xml)
    } else {
        xml
    };
    let doc = roxmltree::Document::parse(&xml)?;
    let has = |pick: &dyn Fn(roxmltree::Node) -> bool| doc.descendants().any(pick);
    // 3D acceleration requires SPICE GL. Remove stale VNC entries.
    if super::spice::gl_enabled(&doc) {
        return super::spice::prepare(connection, id);
    }
    let vnc = has(&|n| n.has_tag_name("graphics") && n.attribute("type") == Some("vnc"));
    let tablet = has(&|n| n.has_tag_name("input") && n.attribute("type") == Some("tablet"));
    let usb = has(&|n| {
        n.has_tag_name("controller")
            && n.attribute("type") == Some("usb")
            && n.attribute("model") != Some("none")
    });
    if vnc && tablet && xml == original {
        return Ok(());
    }
    let mut add = String::new();
    if !vnc {
        add += "<graphics type='vnc' autoport='yes'><listen type='socket'/></graphics>";
    }
    // An absolute tablet keeps the guest pointer exactly under the host pointer.
    if !tablet {
        if !usb {
            add += "<controller type='usb' model='qemu-xhci'/>";
        }
        add += "<input type='tablet' bus='usb'/>";
    }
    let devices = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("devices"))
        .context("The machine configuration is missing its devices section.")?;
    let (range, replacement) = crate::xml::append_child(&xml, devices, &add)?;
    let mut updated = xml;
    updated.replace_range(range, &replacement);
    super::define_xml(connection, &updated)
}

pub(crate) enum DisplayTarget {
    Vnc {
        uri: String,
        clipboard: bool,
    },
    Spice(SpiceOptions),
    Dbus {
        graphics: u32,
        clipboard: bool,
        usb: bool,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct SpiceOptions {
    pub graphics: u32,
    pub clipboard: bool,
    pub usb: bool,
    pub gl: bool,
}

pub(crate) fn display_target(
    connection: Connection,
    id: &str,
    prefer_spice: bool,
) -> Result<DisplayTarget> {
    // Test hook: point the display at a VNC socket directly, without libvirt.
    if let Some(socket) = std::env::var_os("HYDROXL_VNC_SOCKET") {
        return Ok(DisplayTarget::Vnc {
            uri: format!("vnc+unix://{}", socket.to_string_lossy()),
            clipboard: true,
        });
    }
    let xml = virsh(connection, &["dumpxml", id])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let domain = doc.root_element();
    if domain.attribute("id").is_none() {
        bail!("Waiting for guest display");
    }
    let graphics: Vec<_> = domain
        .children()
        .find(|node| node.has_tag_name("devices"))
        .into_iter()
        .flat_map(|devices| devices.children().filter(|node| node.has_tag_name("graphics")))
        .collect();
    let spice = graphics.iter().position(|node| node.attribute("type") == Some("spice"));
    let vnc = graphics.iter().any(|node| node.attribute("type") == Some("vnc"));
    if let Some(index) = graphics.iter().copied().position(is_peer_dbus) {
        let clipboard = super::spice::peer_clipboard(&doc);
        let usb = domain.descendants().any(|node| {
            node.has_tag_name("controller")
                && node.attribute("type") == Some("usb")
                && node.attribute("model") != Some("none")
        });
        return Ok(DisplayTarget::Dbus {
            graphics: index as u32,
            clipboard,
            usb,
        });
    }
    if let Some(index) = spice {
        let gl = super::spice::gl_enabled(&doc);
        if gl || prefer_spice || !vnc {
            let clipboard = !graphics[index].children().any(|node| {
                node.has_tag_name("clipboard") && node.attribute("copypaste") == Some("no")
            });
            let usb = domain.descendants().any(|node| {
                node.has_tag_name("redirdev") && node.attribute("type") == Some("spicevmc")
            });
            return Ok(DisplayTarget::Spice(SpiceOptions {
                graphics: index as u32,
                clipboard,
                usb,
                gl,
            }));
        }
    }
    if vnc {
        let endpoint = virsh(connection, &["domdisplay", "--type", "vnc", id])?;
        if !endpoint.trim().is_empty() {
            let clipboard = !doc.descendants().any(|node| {
                node.has_tag_name("clipboard") && node.attribute("copypaste") == Some("no")
            });
            return Ok(DisplayTarget::Vnc {
                uri: endpoint,
                clipboard,
            });
        }
    }
    bail!("The guest has no SPICE, VNC, or peer D-Bus display.")
}

pub(crate) fn is_peer_dbus(node: roxmltree::Node<'_, '_>) -> bool {
    node.has_tag_name("graphics")
        && node.attribute("type") == Some("dbus")
        && node.attribute("p2p") == Some("yes")
}

/// Hot-plugs a USB tablet into a running guest that has a USB controller but no tablet.
pub fn ensure_live_tablet(connection: Connection, id: &str) -> Result<()> {
    let xml = virsh(connection, &["dumpxml", id])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let tablet =
        doc.descendants().any(|n| n.has_tag_name("input") && n.attribute("type") == Some("tablet"));
    let usb = doc.descendants().any(|n| {
        n.has_tag_name("controller")
            && n.attribute("type") == Some("usb")
            && n.attribute("model") != Some("none")
    });
    if tablet || !usb {
        return Ok(());
    }
    let file = crate::xml::temporary("<input type='tablet' bus='usb'/>")?;
    let path = crate::files::path_text(file.path())?;
    let info = virsh(connection, &["dominfo", id])?;
    let persistent = info.lines().any(|line| {
        line.starts_with("Persistent:") && line.split_whitespace().last() == Some("yes")
    });
    let mut args = vec!["attach-device", id, path, "--live"];
    if persistent {
        args.push("--config");
    }
    virsh(connection, &args).map(|_| ())
}
