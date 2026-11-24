use super::support::{append_child, editable, esc, replace_child, temporary_xml};
use super::{DisplaySettings, GuestOs, sharing::with_shared_memory};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};

const QEMU_NS: &str = "http://libvirt.org/schemas/domain/qemu/1.0";
const DISPLAY_NS: &str = "https://hydroxl.dev/libvirt/display/1";
/// libvirt's alias for the first video device, which carries the 3D renderer.
const VIDEO_ALIAS: &str = "video0";

/// Replaces the QEMU device properties Hydroxl sets on the primary video device.
/// Other overrides and command-line arguments are left alone.
pub(super) fn set_video_properties(
    xml: &str,
    properties: &[(&str, &str, String)],
) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let prefix = root.lookup_prefix(QEMU_NS).unwrap_or("qemu").to_owned();
    let is = |n: &roxmltree::Node, name: &str| {
        n.is_element() && n.tag_name().namespace() == Some(QEMU_NS) && n.tag_name().name() == name
    };
    let overrides = root.children().find(|n| is(n, "override"));
    let ours = overrides.and_then(|o| {
        o.children().find(|n| is(n, "device") && n.attribute("alias") == Some(VIDEO_ALIAS))
    });
    let device = (!properties.is_empty()).then(|| {
        let props: String = properties
            .iter()
            .map(|(name, kind, value)| {
                format!("<{prefix}:property name='{name}' type='{kind}' value='{value}'/>")
            })
            .collect();
        format!(
            "<{prefix}:device alias='{VIDEO_ALIAS}'><{prefix}:frontend>{props}</{prefix}:frontend></{prefix}:device>"
        )
    });
    let mut result = xml.to_owned();
    match (overrides, ours) {
        (Some(o), Some(d)) => {
            let alone = o.children().filter(|n| n.is_element()).count() == 1;
            match (&device, alone) {
                (Some(device), _) => result.replace_range(d.range(), device),
                (None, true) => result.replace_range(o.range(), ""),
                (None, false) => result.replace_range(d.range(), ""),
            }
        }
        (Some(o), None) => {
            if let Some(device) = &device {
                let close = format!("</{prefix}:override>");
                let at = if xml[o.range()].ends_with(&close) {
                    o.range().end - close.len()
                } else {
                    // Self-closing override.
                    result.replace_range(
                        o.range(),
                        &format!("<{prefix}:override>{device}</{prefix}:override>"),
                    );
                    return Ok(result);
                };
                result.insert_str(at, device);
            }
        }
        (None, _) => {
            let Some(device) = &device else {
                return Ok(result);
            };
            let at = root.range().end - "</domain>".len();
            ensure_ends_with_domain(xml, root.range())?;
            result.insert_str(at, &format!("<{prefix}:override>{device}</{prefix}:override>"));
            if root.lookup_namespace_uri(Some(&prefix)) != Some(QEMU_NS) {
                let open = root.range().start + "<domain".len();
                result.insert_str(open, &format!(" xmlns:{prefix}='{QEMU_NS}'"));
            }
        }
    }
    Ok(result)
}

fn ensure_ends_with_domain(xml: &str, range: std::ops::Range<usize>) -> Result<()> {
    xml[range].ends_with("</domain>").then_some(()).context("Invalid domain.")
}

pub(super) fn apply_display(
    c: Connection,
    machine: &Machine,
    settings: Option<DisplaySettings>,
    local_dbus: Option<bool>,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let was_peer_dbus = doc.descendants().any(backend::is_peer_dbus);
    let settings_were_applied = settings.is_some();
    let clipboard = settings.as_ref().map_or_else(
        || {
            if was_peer_dbus {
                backend::peer_clipboard(&doc)
            } else {
                !doc.descendants().any(|node| {
                    node.has_tag_name("clipboard") && node.attribute("copypaste") == Some("no")
                })
            }
        },
        |settings| settings.clipboard,
    );
    let peer_dbus = local_dbus.unwrap_or(was_peer_dbus);
    let clipboard = if peer_dbus && clipboard && !backend::qemu::supports_vdagent(c, &xml)? {
        if local_dbus != Some(true) {
            bail!(
                "This QEMU build does not support D-Bus clipboard sync. Update the VM's QEMU build to enable it."
            );
        }
        false
    } else {
        clipboard
    };
    if let Some(settings) = settings {
        xml = display_settings(machine, xml, settings, local_dbus)?;
    }
    if let Some(dbus) = local_dbus {
        xml = switch_transport(&xml, dbus, clipboard, backend::host_audio_backend())?;
    }
    if peer_dbus {
        if clipboard {
            if let Some(updated) = backend::route_agent_clipboard(&xml, true, Some(true))? {
                xml = updated;
            }
        } else {
            let doc = roxmltree::Document::parse(&xml)?;
            let agent = doc.descendants().find(|&node| {
                backend::is_agent_channel(node) && node.attribute("type") == Some("qemu-vdagent")
            });
            if let Some(agent) = agent {
                xml.replace_range(agent.range(), "");
            }
        }
    } else if settings_were_applied && local_dbus.is_none() {
        let doc = roxmltree::Document::parse(&xml)?;
        let uses_vdagent = doc.descendants().any(|node| {
            backend::is_agent_channel(node) && node.attribute("type") == Some("qemu-vdagent")
        });
        if uses_vdagent
            && let Some(updated) = backend::route_agent_clipboard(&xml, true, Some(clipboard))?
        {
            xml = updated;
        }
    }
    temporary_xml(&xml, |path| backend::virsh(c, &["define", path]).map(|_| ()))
}

fn display_settings(
    machine: &Machine,
    mut xml: String,
    DisplaySettings {
        model,
        heads,
        accel3d,
        sound,
        clipboard,
        vram,
    }: DisplaySettings,
    local_dbus: Option<bool>,
) -> Result<String> {
    if !["virtio", "qxl", "vga", "bochs", "ramfb"].contains(&model.as_str())
        || !(1..=4).contains(&heads)
    {
        bail!("Choose a supported graphics device and 1–4 displays.");
    }
    if !["none", "ich9", "ac97", "es1370", "sb16"].contains(&sound.as_str()) {
        bail!("Choose a supported sound device.");
    }
    if accel3d && model != "virtio" {
        bail!("Choose VirtIO for 3D acceleration.");
    }
    let windows = GuestOs::of(machine) == GuestOs::Windows;
    let doc = roxmltree::Document::parse(&xml)?;
    let caps = accel3d
        .then(|| {
            backend::gpu_features(&machine.arch, backend::qemu::domain_emulator(doc.root_element()))
        })
        .unwrap_or_default();
    let neptune = accel3d && windows;
    if neptune && !backend::triton_arch(&machine.arch) {
        bail!(backend::TRITON_ARCH_UNSUPPORTED);
    }
    if neptune && !caps.neptune {
        bail!(backend::NEPTUNE_HOST_MISSING);
    }
    let blob = accel3d && (caps.venus || neptune);
    let dbus_node = doc
        .descendants()
        .find(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("dbus"));
    let peer_dbus = dbus_node.is_some_and(backend::is_peer_dbus);
    let dbus = local_dbus.unwrap_or(dbus_node.is_some());
    if neptune && local_dbus.is_none() && dbus && !peer_dbus {
        bail!("Use peer-to-peer D-Bus or local SPICE to open DirectX in Hydroxl.");
    }
    let vram = if vram > 0 && ["qxl", "vga", "bochs"].contains(&model.as_str()) {
        if !(8..=512).contains(&vram) {
            bail!("Use 8 to 512 MiB of video memory.");
        }
        format!(" vram='{}'", vram * 1024)
    } else {
        String::new()
    };
    let video = format!(
        "<video><model type='{}' heads='{}'{vram}{} primary='yes'>{}</model></video>",
        model,
        heads,
        if blob {
            " blob='on'"
        } else {
            ""
        },
        if accel3d {
            "<acceleration accel3d='yes'/>"
        } else {
            ""
        }
    );
    xml = replace_child(&xml, "devices", "video", &video)?;
    let mut properties = Vec::new();
    if blob {
        properties.push(("hostmem", "unsigned", backend::HOSTMEM_BYTES.to_string()));
        if caps.venus {
            properties.push(("venus", "bool", "true".to_owned()));
        }
        if neptune {
            properties.push(("neptune", "bool", "true".to_owned()));
        }
        // Blob resources live in guest RAM that the renderer maps.
        if let Some(shared) = with_shared_memory(&xml)? {
            xml = shared;
        }
    }
    xml = set_video_properties(&xml, &properties)?;
    if local_dbus.is_none() && dbus {
        if peer_dbus {
            xml = dbus_gl(&xml, accel3d)?;
        }
    } else if local_dbus.is_none() {
        let graphics = spice_graphics(clipboard, accel3d, None);
        let doc = roxmltree::Document::parse(&xml)?;
        if let Some(spice) = doc
            .descendants()
            .find(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("spice"))
        {
            xml.replace_range(spice.range(), &graphics);
        } else {
            xml = append_child(&xml, "devices", &graphics)?;
        }
        if let Some(compatible) = backend::remove_vnc_for_spice_gl(&xml)? {
            xml = compatible;
        }
    }
    let sound_xml = if sound == "none" {
        String::new()
    } else if dbus || local_dbus.is_some() {
        let doc = roxmltree::Document::parse(&xml)?;
        let audio = doc
            .descendants()
            .find(|node| {
                node.has_tag_name("audio") && node.parent().is_some_and(|n| n.has_tag_name("sound"))
            })
            .and_then(|node| node.attribute("id"))
            .map(|id| format!("<audio id='{}'/>", esc(id)))
            .unwrap_or_default();
        format!("<sound model='{sound}'>{audio}</sound>")
    } else {
        format!("<sound model='{}'><audio id='1'/></sound>", sound)
    };
    xml = replace_child(&xml, "devices", "sound", &sound_xml)?;
    if !dbus && local_dbus.is_none() {
        xml = replace_child(
            &xml,
            "devices",
            "audio",
            if sound == "none" {
                ""
            } else {
                "<audio id='1' type='spice'/>"
            },
        )?;
        if !xml.contains("com.redhat.spice.0") {
            xml = append_child(
                &xml,
                "devices",
                "<channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/></channel>",
            )?;
        }
    }
    Ok(xml)
}

fn gl_xml(enabled: bool, render_node: Option<&str>) -> String {
    match (enabled, render_node) {
        (true, Some(path)) => format!("<gl enable='yes' rendernode='{}'/>", esc(path)),
        (true, None) => "<gl enable='yes'/>".into(),
        (false, _) => String::new(),
    }
}

fn spice_graphics(clipboard: bool, accelerated: bool, render_node: Option<&str>) -> String {
    let clipboard = if clipboard {
        "yes"
    } else {
        "no"
    };
    let gl = gl_xml(accelerated, render_node);
    format!(
        "<graphics type='spice' autoport='yes'><listen type='none'/><clipboard copypaste='{clipboard}'/>{gl}</graphics>"
    )
}

fn switch_transport(
    xml: &str,
    dbus: bool,
    clipboard: bool,
    host_audio: Option<&str>,
) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let devices = doc
        .root_element()
        .children()
        .find(|node| node.has_tag_name("devices"))
        .context("The machine has no device configuration.")?;
    let accelerated = devices
        .descendants()
        .any(|node| node.has_tag_name("acceleration") && node.attribute("accel3d") == Some("yes"));
    let render_node = devices
        .children()
        .filter(|node| node.has_tag_name("graphics"))
        .flat_map(|node| node.children())
        .filter(|node| node.has_tag_name("gl"))
        .find_map(|node| node.attribute("rendernode"));
    let saved = doc.descendants().find(|node| node.has_tag_name((DISPLAY_NS, "display")));
    let previous = saved.and_then(|node| node.text()).unwrap_or("<devices/>");
    let previous = roxmltree::Document::parse(previous)?;
    let previous = previous.root_element();
    let graphics = if dbus {
        let gl = gl_xml(accelerated, render_node);
        format!("<graphics type='dbus' p2p='yes'>{gl}</graphics>")
    } else {
        restore_graphics(previous, clipboard, accelerated, render_node)?
    };
    let mut edits = Vec::new();
    let mut backup = String::new();
    let mut found_graphics = false;
    for node in devices.children().filter(|node| node.is_element()) {
        let kind = node.attribute("type");
        let replacement = match node.tag_name().name() {
            "graphics" if matches!(kind, Some("dbus" | "spice" | "vnc" | "egl-headless")) => {
                let replacement = if found_graphics {
                    ""
                } else {
                    graphics.as_str()
                };
                found_graphics = true;
                Some(replacement.to_owned())
            }
            "redirdev" if dbus && kind == Some("spicevmc") => Some(String::new()),
            "channel"
                if dbus
                    && kind == Some("spicevmc")
                    && (!clipboard || !backend::is_agent_channel(node)) =>
            {
                Some(String::new())
            }
            "audio" if dbus && kind == Some("spice") => {
                let backend = host_audio.unwrap_or("none");
                let id = esc(node.attribute("id").context("The audio backend has no ID.")?);
                Some(format!("<audio id='{id}' type='{backend}'/>"))
            }
            "audio" if !dbus => previous
                .children()
                .find(|old| {
                    old.has_tag_name("audio") && old.attribute("id") == node.attribute("id")
                })
                .map(|old| old.document().input_text()[old.range()].to_owned())
                .or_else(|| {
                    let id = node.attribute("id").filter(|_| kind == Some("dbus"))?;
                    Some(format!("<audio id='{}' type='spice'/>", esc(id)))
                }),
            _ => None,
        };
        if let Some(replacement) = replacement {
            if dbus && (kind != Some("dbus") || !backend::is_peer_dbus(node)) {
                backup.push_str(&xml[node.range()]);
            }
            edits.push((node.range(), replacement));
        }
    }
    if let Some(saved) = saved {
        edits.push((saved.range(), String::new()));
    }
    let mut updated = xml.to_owned();
    edits.sort_unstable_by_key(|(range, _)| range.start);
    for (range, replacement) in edits.into_iter().rev() {
        updated.replace_range(range, &replacement);
    }
    if !found_graphics {
        updated = append_child(&updated, "devices", &graphics)?;
    }
    if dbus && !backup.is_empty() {
        let entry = format!(
            "<display xmlns='{DISPLAY_NS}'>{}</display>",
            esc(&format!("<devices>{backup}</devices>"))
        );
        updated = if doc.root_element().children().any(|node| node.has_tag_name("metadata")) {
            append_child(&updated, "metadata", &entry)?
        } else {
            append_child(&updated, "domain", &format!("<metadata>{entry}</metadata>"))?
        };
    } else if !dbus {
        let usb = devices.children().any(|node| {
            node.has_tag_name("controller")
                && node.attribute("type") == Some("usb")
                && node.attribute("model") != Some("none")
        });
        for node in previous.children().filter(|node| {
            (usb && node.has_tag_name("redirdev"))
                || (node.has_tag_name("channel") && !backend::is_agent_channel(*node))
        }) {
            updated =
                append_child(&updated, "devices", &node.document().input_text()[node.range()])?;
        }
        let restored = roxmltree::Document::parse(&updated)?;
        let spice = restored
            .descendants()
            .any(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("spice"));
        if spice
            && let Some(agent) = backend::route_agent_clipboard(&updated, false, Some(clipboard))?
        {
            updated = agent;
        }
    }
    Ok(updated)
}

fn restore_graphics(
    previous: roxmltree::Node<'_, '_>,
    clipboard: bool,
    accelerated: bool,
    render_node: Option<&str>,
) -> Result<String> {
    let mut restored = String::new();
    let mut local = false;
    for node in previous.children().filter(|node| node.has_tag_name("graphics")) {
        match node.attribute("type") {
            Some("spice") => {
                let mut graphics = node.document().input_text()[node.range()].to_owned();
                let clipboard = if clipboard {
                    "yes"
                } else {
                    "no"
                };
                graphics = replace_child(
                    &graphics,
                    "graphics",
                    "clipboard",
                    &format!("<clipboard copypaste='{clipboard}'/>"),
                )?;
                let old_gl = node.children().find(|child| child.has_tag_name("gl"));
                let gl = gl_xml(
                    accelerated,
                    render_node.or_else(|| old_gl.and_then(|gl| gl.attribute("rendernode"))),
                );
                restored.push_str(&replace_child(&graphics, "graphics", "gl", &gl)?);
                local = true;
            }
            Some("vnc" | "egl-headless") if accelerated => {}
            kind => {
                local |= kind == Some("vnc");
                restored.push_str(&node.document().input_text()[node.range()]);
            }
        }
    }
    if !local {
        restored.push_str(&spice_graphics(clipboard, accelerated, render_node));
    }
    Ok(restored)
}

fn dbus_gl(xml: &str, enabled: bool) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let graphics = doc
        .descendants()
        .find(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("dbus"))
        .context("The D-Bus display is missing.")?;
    let mut updated = xml.to_owned();
    if let Some(node) = graphics.children().find(|node| node.has_tag_name("gl")) {
        if !enabled {
            updated.replace_range(node.range(), "");
        } else if let Some(value) = node.attribute_node("enable") {
            updated.replace_range(value.range_value(), "yes");
        } else {
            updated.insert_str(node.range().start + "<gl".len(), " enable='yes'");
        }
    } else if enabled {
        let gl = "<gl enable='yes'/>";
        let text = &xml[graphics.range()];
        if text.ends_with("/>") {
            updated.replace_range(
                graphics.range().end - 2..graphics.range().end,
                &format!(">{gl}</graphics>"),
            );
        } else {
            updated.insert_str(graphics.range().end - "</graphics>".len(), gl);
        }
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN: &str = "<domain type='kvm'><name>w</name><devices/></domain>";

    fn props() -> Vec<(&'static str, &'static str, String)> {
        vec![("venus", "bool", "true".into()), ("hostmem", "unsigned", "8589934592".into())]
    }

    #[test]
    fn adds_namespace_and_override() {
        let xml = set_video_properties(DOMAIN, &props()).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(doc.root_element().lookup_namespace_uri(Some("qemu")), Some(QEMU_NS));
        assert!(xml.contains(
            "<qemu:override><qemu:device alias='video0'><qemu:frontend>\
             <qemu:property name='venus' type='bool' value='true'/>"
        ));
        // Applying again replaces instead of duplicating.
        let again = set_video_properties(&xml, &props()).unwrap();
        assert_eq!(again.matches("alias='video0'").count(), 1);
    }

    #[test]
    fn clearing_keeps_foreign_overrides() {
        let xml = set_video_properties(DOMAIN, &props()).unwrap();
        assert!(!set_video_properties(&xml, &[]).unwrap().contains("override"));
        let foreign = xml.replace(
            "</qemu:override>",
            "<qemu:device alias='net0'><qemu:frontend/></qemu:device></qemu:override>",
        );
        let cleared = set_video_properties(&foreign, &[]).unwrap();
        assert!(cleared.contains("alias='net0'") && !cleared.contains("alias='video0'"));
        roxmltree::Document::parse(&cleared).unwrap();
    }

    #[test]
    fn clearing_without_override_is_a_no_op() {
        assert_eq!(set_video_properties(DOMAIN, &[]).unwrap(), DOMAIN);
    }

    #[test]
    fn enables_dbus_gl_without_discarding_the_render_node() -> Result<()> {
        let xml = "<domain><devices><graphics type='dbus' p2p='yes'><gl enable='no' rendernode='/dev/dri/renderD129'/></graphics></devices></domain>";
        let enabled = dbus_gl(xml, true)?;
        assert!(enabled.contains("<gl enable='yes' rendernode='/dev/dri/renderD129'/>"));
        assert!(dbus_gl(&enabled, false)?.contains("<graphics type='dbus' p2p='yes'></graphics>"));
        let self_closing = "<domain><devices><graphics type='dbus' p2p='yes'/></devices></domain>";
        assert!(dbus_gl(self_closing, true)?.contains("<gl enable='yes'/>"));
        Ok(())
    }

    #[test]
    fn switches_to_peer_dbus_without_vdagent_and_restores_spice() -> Result<()> {
        let xml = "<domain type='kvm'><name>test</name><metadata><os xmlns='urn:test'/></metadata><devices>+            <graphics type='spice'><listen type='none'/><gl enable='yes' rendernode='/dev/dri/renderD129'/></graphics>+            <video><model type='virtio' blob='on'><acceleration accel3d='yes'/></model><alias name='video0'/></video>+            <sound model='ich9'><audio id='1'/></sound><audio id='1' type='spice'/>+            <controller type='usb' model='qemu-xhci'/><redirdev bus='usb' type='spicevmc'/>+            <channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/></channel>+            <channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>+            </devices></domain>";
        let dbus = switch_transport(xml, true, false, Some("pipewire"))?;
        let doc = roxmltree::Document::parse(&dbus)?;
        let device = |tag| doc.descendants().find(|node| node.has_tag_name(tag)).unwrap();
        assert!(backend::is_peer_dbus(device("graphics")));
        assert_eq!(device("gl").attribute("rendernode"), Some("/dev/dri/renderD129"));
        assert_eq!(device("model").attribute("blob"), Some("on"));
        assert!(!doc.descendants().any(backend::is_agent_channel));
        assert_eq!(device("channel").attribute("type"), Some("unix"));
        assert!(!doc.descendants().any(|node| node.has_tag_name("redirdev")));
        assert!(dbus.contains("<audio id='1' type='pipewire'/>"));
        let restored = switch_transport(&dbus, false, true, None)?;
        assert!(!restored.contains(DISPLAY_NS));
        assert!(restored.contains("<os xmlns='urn:test'/>"));
        assert!(restored.contains("<listen type='none'/>"));
        assert!(restored.contains("rendernode='/dev/dri/renderD129'"));
        assert!(restored.contains("<redirdev bus='usb' type='spicevmc'/>"));
        assert!(restored.contains("<audio id='1' type='spice'/>"));
        assert!(restored.contains("<channel type='spicevmc'>"));
        Ok(())
    }
}
