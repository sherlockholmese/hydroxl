use super::support::{append_child, editable, replace_child, temporary_xml};
use crate::backend::{self, Connection, Machine};
use anyhow::{Result, bail};

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
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let dbus = doc
        .descendants()
        .any(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("dbus"));
    let vram = if vram > 0 && ["qxl", "vga", "bochs"].contains(&model.as_str()) {
        if !(8..=512).contains(&vram) {
            bail!("Use 8 to 512 MB of video memory.");
        }
        format!(" vram='{}'", vram * 1024)
    } else {
        String::new()
    };
    let video = format!(
        "<video><model type='{}' heads='{}'{vram} primary='yes'>{}</model></video>",
        model,
        heads,
        if accel3d {
            "<acceleration accel3d='yes'/>"
        } else {
            ""
        }
    );
    xml = replace_child(&xml, "devices", "video", &video)?;
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
    if !dbus {
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
