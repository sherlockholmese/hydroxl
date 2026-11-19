use super::support::{append_child, editable, replace_child, temporary_xml};
use super::{GuestOs, sharing::with_shared_memory};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};

const QEMU_NS: &str = "http://libvirt.org/schemas/domain/qemu/1.0";
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

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_display(
    c: Connection,
    machine: &Machine,
    model: String,
    heads: u32,
    accel3d: bool,
    sound: String,
    clipboard: bool,
    vram: u32,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
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
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
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
    let dbus = dbus_node.is_some();
    if neptune && dbus_node.is_some_and(|node| node.attribute("p2p") != Some("yes")) {
        bail!("Use peer-to-peer D-Bus or local SPICE to open DirectX in Hydroxl.");
    }
    let vram = if vram > 0 && ["qxl", "vga", "bochs"].contains(&model.as_str()) {
        if !(8..=512).contains(&vram) {
            bail!("Use 8 to 512 MB of video memory.");
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
    let graphics = format!(
        "<graphics type='spice' autoport='yes'><listen type='none'/><clipboard copypaste='{}'/>{}</graphics>",
        if clipboard {
            "yes"
        } else {
            "no"
        },
        if accel3d {
            "<gl enable='yes'/>"
        } else {
            ""
        }
    );
    if dbus && windows {
        xml = dbus_gl(&xml, accel3d)?;
    } else if !dbus {
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
    } else if dbus {
        format!("<sound model='{}'/>", sound)
    } else {
        format!("<sound model='{}'><audio id='1'/></sound>", sound)
    };
    xml = replace_child(&xml, "devices", "sound", &sound_xml)?;
    if !dbus {
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
    temporary_xml(&xml, |path| backend::virsh(c, &["define", path]).map(|_| ()))?;
    Ok(())
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
}
