//! Prepare local SPICE channels for the in-app viewer. Existing endpoints and devices
//! keep their settings. USB devices are redirected only when selected in the viewer.
use super::{Connection, virsh};
use anyhow::{Context, Result};
use std::io::Write;

pub(super) fn prepare(connection: Connection, id: &str) -> Result<()> {
    let xml = virsh(connection, &["dumpxml", id, "--inactive"])?;
    let Some(updated) = prepare_xml(&xml)? else {
        return Ok(());
    };
    let mut file = tempfile::Builder::new().suffix(".xml").tempfile()?;
    file.write_all(updated.as_bytes())?;
    let path = file.path().to_str().context("Invalid configuration path.")?;
    virsh(connection, &["define", path])?;
    Ok(())
}

fn prepare_xml(xml: &str) -> Result<Option<String>> {
    let original = xml;
    let compatible = remove_vnc_for_spice_gl(xml)?;
    let xml = compatible.as_deref().unwrap_or(xml);
    let doc = roxmltree::Document::parse(xml)?;
    if doc.descendants().any(super::library::is_peer_dbus) {
        return Ok(None);
    }
    let devices = doc
        .root_element()
        .children()
        .find(|node| node.has_tag_name("devices"))
        .context("The machine has no device configuration.")?;
    let has = |tag| devices.children().any(|node| node.has_tag_name(tag));
    let first_graphics = devices.children().find(|node| node.has_tag_name("graphics"));
    let spice = devices
        .children()
        .find(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("spice"));
    let usb_disabled = devices.children().any(|node| {
        node.has_tag_name("controller")
            && node.attribute("type") == Some("usb")
            && node.attribute("model") == Some("none")
    });
    let mut additions = String::new();
    if !has("audio") && has("sound") {
        additions.push_str("<audio id='1' type='spice'/>");
    }
    if !devices.children().any(|node| {
        node.has_tag_name("channel")
            && node.children().any(|child| {
                child.has_tag_name("target")
                    && child.attribute("name") == Some("com.redhat.spice.0")
            })
    }) {
        additions.push_str(
            "<channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/></channel>",
        );
    }
    if !usb_disabled
        && !devices
            .children()
            .any(|node| node.has_tag_name("redirdev") && node.attribute("type") == Some("spicevmc"))
    {
        let controller = devices
            .children()
            .any(|node| node.has_tag_name("controller") && node.attribute("type") == Some("usb"));
        if !controller {
            additions.push_str("<controller type='usb' model='qemu-xhci'/>");
        }
        additions
            .push_str("<redirdev bus='usb' type='spicevmc'/><redirdev bus='usb' type='spicevmc'/>");
    }
    let mut edited = xml.to_owned();
    let range = devices.range();
    if xml[range.clone()].ends_with("/>") {
        let spice = "<graphics type='spice'><listen type='none'/></graphics>";
        edited.replace_range(range.end - 2..range.end, &format!(">{spice}{additions}</devices>"));
    } else {
        let end = range.end - "</devices>".len();
        edited.insert_str(end, &additions);
        match (first_graphics, spice) {
            (Some(first), Some(spice)) if first != spice => {
                let definition = &xml[spice.range()];
                edited.replace_range(spice.range(), "");
                edited.insert_str(first.range().start, definition);
            }
            (first, None) => {
                let at = first.map_or(end, |node| node.range().start);
                edited.insert_str(at, "<graphics type='spice'><listen type='none'/></graphics>");
            }
            _ => {}
        }
    }
    // 2D displays use the shared-memory surface. QEMU stops filling the GL scanout of a
    // 2D console for a viewer that stays connected across a resolution change (Windows
    // at boot), which leaves a black screen. HYDROXL_SPICE_GL=1 keeps GL for testing.
    let edited = if std::env::var_os("HYDROXL_SPICE_GL").is_some() && render_node_available() {
        enable_gl_scanout(&edited)?.unwrap_or(edited)
    } else {
        drop_scanout_gl(&edited)?.unwrap_or(edited)
    };
    let edited = local_encoding(&edited)?.unwrap_or(edited);
    let edited = route_audio(&edited, "spice")?.unwrap_or(edited);
    let edited = route_agent(&edited, false)?.unwrap_or(edited);
    Ok((edited != original).then_some(edited))
}

pub(super) fn gl_enabled(doc: &roxmltree::Document) -> bool {
    doc.descendants().any(|node| {
        node.has_tag_name("graphics")
            && node.attribute("type") == Some("spice")
            && node
                .children()
                .any(|child| child.has_tag_name("gl") && child.attribute("enable") == Some("yes"))
    })
}

/// The viewer connects over a local socket, where heavy or lossy compression only costs
/// time. The server's video detection is the worst of it: busy regions are sent as lossy MJPEG
/// streams on their own clock while the rest arrives as images, which tears at their
/// boundary and bands at high resolutions. Every update is sent as an exact image.
const LOCAL_ENCODING: &[(&str, &str)] = &[
    // LZ is cheap to decode and keeps a full high-resolution frame small enough to
    // arrive within one refresh; no compression made each frame tens of megabytes.
    ("image", "<image compression='lz'/>"),
    ("jpeg", "<jpeg compression='never'/>"),
    ("zlib", "<zlib compression='never'/>"),
    ("streaming", "<streaming mode='off'/>"),
];

fn local_encoding(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let Some(spice) = doc
        .descendants()
        .find(|n| n.has_tag_name("graphics") && n.attribute("type") == Some("spice"))
    else {
        return Ok(None);
    };
    // Only local displays: a SPICE server listening on the network keeps its settings.
    let listen = spice.attribute("listen").or_else(|| {
        spice
            .children()
            .find(|c| c.has_tag_name("listen"))
            .and_then(|l| l.attribute("type").filter(|t| *t != "none"))
    });
    if listen.is_some() {
        return Ok(None);
    }
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut missing = String::new();
    for (tag, element) in LOCAL_ENCODING {
        match spice.children().find(|c| c.has_tag_name(*tag)) {
            Some(node) if &xml[node.range()] == *element => {}
            Some(node) => edits.push((node.range(), (*element).to_owned())),
            None => missing.push_str(element),
        }
    }
    if edits.is_empty() && missing.is_empty() {
        return Ok(None);
    }
    let text = &xml[spice.range()];
    if !missing.is_empty() {
        if text.ends_with("/>") {
            edits.push((
                spice.range(),
                format!("{}>{missing}</graphics>", &text[..text.len() - 2]),
            ));
        } else {
            let at = spice.range().end - "</graphics>".len();
            edits.push((at..at, missing));
        }
    }
    let mut edited = xml.to_owned();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, text) in edits {
        edited.replace_range(range, &text);
    }
    Ok(Some(edited))
}

fn render_node_available() -> bool {
    std::fs::read_dir("/dev/dri").is_ok_and(|entries| {
        entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("renderD"))
    })
}

/// Adds GL scanout to the SPICE display of a VirtIO, VGA or Bochs device. QXL draws
/// through its own SPICE commands and keeps the classic path.
fn enable_gl_scanout(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    if gl_enabled(&doc) {
        return Ok(None);
    }
    let model = doc
        .descendants()
        .find(|n| n.has_tag_name("model") && n.parent().is_some_and(|p| p.has_tag_name("video")))
        .and_then(|n| n.attribute("type"))
        .unwrap_or("");
    if !matches!(model, "virtio" | "vga" | "bochs") {
        return Ok(None);
    }
    let Some(spice) = doc
        .descendants()
        .find(|n| n.has_tag_name("graphics") && n.attribute("type") == Some("spice"))
    else {
        return Ok(None);
    };
    let text = &xml[spice.range()];
    let mut edited = xml.to_owned();
    if text.ends_with("/>") {
        edited.replace_range(
            spice.range(),
            &format!("{}><gl enable='yes'/></graphics>", &text[..text.len() - 2]),
        );
    } else if let Some(gl) = spice.children().find(|c| c.has_tag_name("gl")) {
        edited.replace_range(gl.range(), "<gl enable='yes'/>");
    } else {
        edited.insert_str(spice.range().end - "</graphics>".len(), "<gl enable='yes'/>");
    }
    Ok(Some(remove_vnc_for_spice_gl(&edited)?.unwrap_or(edited)))
}

/// Where guest sound plays: through the SPICE viewer, or straight to the host's sound
/// server so a VNC viewer has audio too. Only backends Hydroxl manages are switched;
/// a backend chosen by hand (ALSA, JACK, D-Bus) is left alone.
pub(super) fn route_audio(xml: &str, backend: &str) -> Result<Option<String>> {
    const MANAGED: [&str; 3] = ["spice", "pipewire", "pulseaudio"];
    let doc = roxmltree::Document::parse(xml)?;
    let Some(devices) = doc.root_element().children().find(|n| n.has_tag_name("devices")) else {
        return Ok(None);
    };
    if !devices.children().any(|n| n.has_tag_name("sound")) {
        return Ok(None);
    }
    let element = format!("<audio id='1' type='{backend}'/>");
    let mut edited = xml.to_owned();
    match devices.children().find(|n| n.has_tag_name("audio") && n.attribute("id") == Some("1")) {
        Some(audio) if audio.attribute("type") == Some(backend) => return Ok(None),
        Some(audio) if MANAGED.contains(&audio.attribute("type").unwrap_or("")) => {
            edited.replace_range(audio.range(), &element);
        }
        Some(_) => return Ok(None),
        None => edited.insert_str(devices.range().end - "</devices>".len(), &element),
    }
    Ok(Some(edited))
}

/// The guest agent channel (spice-vdagent's `com.redhat.spice.0` port) connects either to
/// the SPICE server or, for a VNC viewer, to QEMU's built-in agent, which serves the
/// clipboard over VNC. Only one can own the port, so it follows the viewer.
pub(super) fn route_agent(xml: &str, vnc: bool) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let Some(devices) = doc.root_element().children().find(|n| n.has_tag_name("devices")) else {
        return Ok(None);
    };
    let channel = devices.children().find(|n| {
        n.has_tag_name("channel")
            && n.children().any(|t| {
                t.has_tag_name("target") && t.attribute("name") == Some("com.redhat.spice.0")
            })
    });
    let (wanted, other) = if vnc {
        ("qemu-vdagent", "spicevmc")
    } else {
        ("spicevmc", "qemu-vdagent")
    };
    let copypaste = !doc
        .descendants()
        .any(|n| n.has_tag_name("clipboard") && n.attribute("copypaste") == Some("no"));
    let element = if vnc {
        format!(
            "<channel type='qemu-vdagent'><source><clipboard copypaste='{}'/></source>\
             <target type='virtio' name='com.redhat.spice.0'/></channel>",
            if copypaste { "yes" } else { "no" }
        )
    } else {
        "<channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/></channel>"
            .to_owned()
    };
    let mut edited = xml.to_owned();
    match channel {
        Some(node) if node.attribute("type") == Some(wanted) => return Ok(None),
        Some(node) if node.attribute("type") == Some(other) => {
            edited.replace_range(node.range(), &element);
        }
        Some(_) => return Ok(None),
        None if vnc => edited.insert_str(devices.range().end - "</devices>".len(), &element),
        None => return Ok(None),
    }
    Ok(Some(edited))
}

/// The host sound server QEMU can play to directly, for viewers without an audio channel.
pub(super) fn host_audio_backend() -> Option<&'static str> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)?;
    if runtime.join("pipewire-0").exists() {
        Some("pipewire")
    } else if runtime.join("pulse/native").exists() {
        Some("pulseaudio")
    } else {
        None
    }
}

/// Removes SPICE GL when it only serves 2D scanout (no 3D acceleration).
pub(super) fn drop_scanout_gl(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let accel3d = doc
        .descendants()
        .any(|n| n.has_tag_name("acceleration") && n.attribute("accel3d") == Some("yes"));
    if accel3d || !gl_enabled(&doc) {
        return Ok(None);
    }
    let mut edited = xml.to_owned();
    let mut ranges: Vec<_> = doc
        .descendants()
        .filter(|n| {
            n.has_tag_name("gl")
                && n.parent().is_some_and(|p| {
                    p.has_tag_name("graphics") && p.attribute("type") == Some("spice")
                })
        })
        .map(|n| n.range())
        .collect();
    while let Some(range) = ranges.pop() {
        edited.replace_range(range, "");
    }
    Ok(Some(edited))
}

pub(crate) fn remove_vnc_for_spice_gl(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    if !gl_enabled(&doc) {
        return Ok(None);
    }
    let mut vnc: Vec<_> = doc
        .descendants()
        .filter(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("vnc"))
        .map(|node| node.range())
        .collect();
    if vnc.is_empty() {
        return Ok(None);
    }
    let mut edited = xml.to_owned();
    while let Some(range) = vnc.pop() {
        edited.replace_range(range, "");
    }
    Ok(Some(edited))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_local_spice_once_and_honors_disabled_usb() {
        let xml = "<domain><os><type arch='x86_64'>hvm</type></os><devices><sound model='ich9'/></devices></domain>";
        let configured = prepare_xml(xml).unwrap().unwrap();
        assert!(configured.contains("<listen type='none'/>"));
        assert!(configured.contains("<audio id='1' type='spice'/>"));
        assert!(configured.contains("type='spicevmc'"));
        assert!(configured.contains("name='com.redhat.spice.0'"));
        assert!(prepare_xml(&configured).unwrap().is_none());
        let no_usb = xml.replace("<devices>", "<devices><controller type='usb' model='none'/>");
        assert!(!prepare_xml(&no_usb).unwrap().unwrap().contains("redirdev"));
    }

    #[test]
    fn agent_channel_follows_the_viewer() -> Result<()> {
        let spice = "<domain><devices><graphics type='spice'><clipboard copypaste='yes'/></graphics><channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/><address type='virtio-serial' controller='0' bus='0' port='1'/></channel></devices></domain>";
        let vnc = route_agent(spice, true)?.unwrap();
        assert!(vnc.contains("<channel type='qemu-vdagent'><source><clipboard copypaste='yes'/></source>"));
        assert!(!vnc.contains("spicevmc"));
        assert!(route_agent(&vnc, true)?.is_none());
        let back = route_agent(&vnc, false)?.unwrap();
        assert!(back.contains("<channel type='spicevmc'>"));
        let off = spice.replace("copypaste='yes'", "copypaste='no'");
        assert!(route_agent(&off, true)?.unwrap().contains("copypaste='no'"));
        let none = "<domain><devices></devices></domain>";
        assert!(route_agent(none, true)?.unwrap().contains("qemu-vdagent"));
        assert!(route_agent(none, false)?.is_none());
        Ok(())
    }

    #[test]
    fn audio_follows_the_viewer_and_keeps_manual_backends() -> Result<()> {
        let spice = "<domain><devices><sound model='ich9'><audio id='1'/></sound><audio id='1' type='spice'/></devices></domain>";
        let vnc = route_audio(spice, "pipewire")?.unwrap();
        assert!(vnc.contains("<audio id='1' type='pipewire'/>"));
        assert!(!vnc.contains("type='spice'"));
        assert_eq!(route_audio(&vnc, "spice")?.unwrap(), spice);
        assert!(route_audio(spice, "spice")?.is_none());
        let alsa = spice.replace("type='spice'", "type='alsa'");
        assert!(route_audio(&alsa, "pipewire")?.is_none());
        let silent = "<domain><devices><video/></devices></domain>";
        assert!(route_audio(silent, "pipewire")?.is_none());
        Ok(())
    }

    #[test]
    fn local_spice_sends_exact_images_without_video_streams() -> Result<()> {
        let xml = "<domain><devices><graphics type='spice'><listen type='none'/><streaming mode='filter'/></graphics></devices></domain>";
        let edited = local_encoding(xml)?.unwrap();
        for element in ["<image compression='lz'/>", "<jpeg compression='never'/>",
            "<zlib compression='never'/>", "<streaming mode='off'/>"] {
            assert!(edited.contains(element), "{element} missing: {edited}");
        }
        assert!(!edited.contains("filter"));
        roxmltree::Document::parse(&edited)?;
        assert!(local_encoding(&edited)?.is_none());
        let network = "<domain><devices><graphics type='spice' port='5900' listen='0.0.0.0'/></devices></domain>";
        assert!(local_encoding(network)?.is_none());
        Ok(())
    }

    #[test]
    fn two_d_displays_get_gpu_scanout_except_qxl() -> Result<()> {
        let xml = "<domain><devices><video><model type='virtio'/></video><graphics type='spice'><listen type='none'/></graphics><graphics type='vnc'/></devices></domain>";
        let edited = enable_gl_scanout(xml)?.unwrap();
        assert!(edited.contains("<gl enable='yes'/>"));
        assert!(!edited.contains("type='vnc'"));
        assert!(enable_gl_scanout(&edited)?.is_none());
        assert!(enable_gl_scanout(&xml.replace("virtio", "qxl"))?.is_none());
        let dropped = drop_scanout_gl(&edited)?.unwrap();
        assert!(!dropped.contains("<gl"));
        Ok(())
    }

    #[test]
    fn removes_legacy_vnc_only_when_spice_uses_gl() -> Result<()> {
        let xml = "<domain><devices><video><model type='virtio'><acceleration accel3d='yes'/></model></video><graphics type='spice'><listen type='none'/><gl enable='yes'/></graphics><graphics type='vnc'><listen type='socket'/></graphics></devices></domain>";
        let edited = prepare_xml(xml)?.unwrap();
        assert!(edited.contains("<gl enable='yes'/>"));
        assert!(!edited.contains("type='vnc'"));
        assert!(prepare_xml(&edited)?.is_none());
        assert!(remove_vnc_for_spice_gl(&xml.replace("enable='yes'", "enable='no'"))?.is_none());
        Ok(())
    }
}
