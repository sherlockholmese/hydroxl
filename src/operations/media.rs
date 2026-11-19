use super::support::{attach, esc, path_text, stamp, temporary_xml, valid_target};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub(super) fn apply_add_cdrom(
    c: Connection,
    machine: &Machine,
    target: String,
    bus: String,
) -> Result<()> {
    let id = machine.id.as_str();
    valid_target(&target)?;
    if !["sata", "scsi", "ide", "usb"].contains(&bus.as_str()) {
        bail!("Choose an optical drive interface.");
    }
    attach(c, id, &cdrom_xml(&target, &bus, None))?;
    Ok(())
}

fn cdrom_xml(target: &str, bus: &str, source: Option<&str>) -> String {
    let source = source.map_or_else(String::new, |path| format!("<source file='{}'/>", esc(path)));
    format!(
        "<disk type='file' device='cdrom'><driver name='qemu' type='raw'/>{source}<target dev='{}' bus='{}'/><readonly/></disk>",
        esc(target),
        esc(bus),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ToolsDrive<'a> {
    Mounted,
    Insert(&'a str),
    Replace(&'a str),
    Attach {
        target: String,
        bus: &'static str,
        live: bool,
    },
}

fn tools_drive<'a>(
    doc: &'a Document<'_>,
    iso: &str,
    requested: Option<&str>,
) -> Result<ToolsDrive<'a>> {
    let root = doc.root_element();
    let child = |node: Node<'a, '_>, name| node.children().find(|n| n.has_tag_name(name));
    let devices = child(root, "devices").context("The machine has no device configuration.")?;
    let disks: Vec<_> = devices
        .children()
        .filter(|n| n.has_tag_name("disk"))
        .filter_map(|disk| Some((child(disk, "target")?.attribute("dev")?, disk)))
        .collect();
    let optical = disks.iter().filter(|(_, disk)| disk.attribute("device") == Some("cdrom"));
    let mut empty = None;
    for (target, disk) in optical {
        if requested.is_some_and(|requested| requested != *target) {
            continue;
        }
        let source = child(*disk, "source");
        if source.and_then(|node| node.attribute("file")) == Some(iso) {
            return Ok(ToolsDrive::Mounted);
        }
        let occupied = source.is_some_and(|node| {
            node.attributes().any(|attr| !matches!(attr.name(), "startupPolicy" | "index"))
                || node.children().any(|child| child.is_element())
        });
        if requested.is_some() && occupied {
            return Ok(ToolsDrive::Replace(target));
        }
        if !occupied {
            empty.get_or_insert(*target);
        }
    }
    if let Some(target) = empty {
        return Ok(ToolsDrive::Insert(target));
    }
    if requested.is_some() {
        bail!("The selected CD/DVD drive is no longer available.");
    }
    let live = root.attribute("id").is_some();
    let controller = |kind| {
        devices.children().any(|n| {
            n.has_tag_name("controller")
                && n.attribute("type") == Some(kind)
                && n.attribute("model") != Some("none")
        })
    };
    let arch = child(root, "os")
        .and_then(|node| child(node, "type"))
        .and_then(|node| node.attribute("arch"));
    let x86 = matches!(arch, Some("x86_64" | "i686") | None);
    let bus = if (live || !x86) && controller("usb") {
        "usb"
    } else if (live && controller("scsi")) || !x86 {
        "scsi"
    } else {
        "sata"
    };
    let target = ('a'..='z')
        .map(|letter| format!("sd{letter}"))
        .find(|target| !disks.iter().any(|(dev, _)| dev == target))
        .context("No free CD/DVD drive target is available.")?;
    Ok(ToolsDrive::Attach {
        target,
        bus,
        live,
    })
}

/// Mounts the cached VirtIO ISO in the requested drive or finds a drive without replacing media.
pub(super) fn apply_guest_tools(
    c: Connection,
    machine: &Machine,
    target: Option<String>,
) -> Result<()> {
    let iso = guest_tools_iso()?;
    mount_tools(c, machine, &iso, target.as_deref())
}

pub(super) fn apply_triton_driver(c: Connection, machine: &Machine) -> Result<()> {
    if super::GuestOs::of(machine) != super::GuestOs::Windows {
        bail!("Choose a Windows machine for the Triton driver.");
    }
    let iso = backend::qemu::triton_iso(&machine.arch)?;
    mount_tools(c, machine, &iso, None)
}

fn mount_tools(c: Connection, machine: &Machine, iso: &Path, target: Option<&str>) -> Result<()> {
    let iso = path_text(iso)?;
    let id = &machine.id;
    let xml = backend::virsh(c, &["dumpxml", id])?;
    let doc = Document::parse(&xml)?;
    match tools_drive(&doc, iso, target)? {
        ToolsDrive::Mounted => Ok(()),
        ToolsDrive::Insert(target) => change_media(c, id, target, "--insert", Some(iso)),
        ToolsDrive::Replace(target) => change_media(c, id, target, "--update", Some(iso)),
        ToolsDrive::Attach {
            target,
            bus,
            live,
        } => {
            let xml = cdrom_xml(&target, bus, Some(iso));
            if live {
                temporary_xml(&xml, |path| {
                    backend::virsh(c, &["attach-device", id, path, "--live"]).map(|_| ())
                })
                .context("Could not add a CD/DVD drive while the VM is running. Eject a disc and retry, or shut down the VM and retry.")
            } else {
                attach(c, id, &xml)
            }
        }
    }
}

fn guest_tools_iso() -> Result<PathBuf> {
    let folder =
        backend::data_dir()?.parent().context("The data folder is missing.")?.join("tools");
    fs::create_dir_all(&folder)?;
    let iso = folder.join("virtio-win.iso");
    if !valid_iso(&iso) {
        let download = folder.join(format!("virtio-win-{}.part", stamp()));
        let result = (|| -> Result<()> {
            backend::run(
                "curl",
                &[
                    "--fail",
                    "--location",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--silent",
                    "--show-error",
                    "--output",
                    path_text(&download)?,
                    "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso",
                ],
            )?;
            if !valid_iso(&download) {
                bail!("The guest tools download is incomplete. Try again.");
            }
            fs::rename(&download, &iso)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&download);
        }
        result?;
    }
    Ok(iso)
}

fn valid_iso(path: &Path) -> bool {
    fn check(path: &Path) -> std::io::Result<bool> {
        const SECTOR: usize = 2048;
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(16 * SECTOR as u64))?;
        let mut descriptor = [0; SECTOR];
        for _ in 0..16 {
            file.read_exact(&mut descriptor)?;
            if &descriptor[1..7] != b"CD001\x01" || descriptor[0] == 255 {
                break;
            }
            if descriptor[0] == 1 {
                let blocks = u32::from_le_bytes(descriptor[80..84].try_into().unwrap());
                let block_size = u16::from_le_bytes(descriptor[128..130].try_into().unwrap());
                let volume = u64::from(blocks) * u64::from(block_size);
                return Ok((1024 * 1024..=length).contains(&volume));
            }
        }
        Ok(false)
    }
    check(path).unwrap_or(false)
}

pub(super) fn apply_mount_iso(
    c: Connection,
    machine: &Machine,
    path: PathBuf,
    target: String,
) -> Result<()> {
    if !path.is_file() {
        bail!("Choose an existing ISO file.");
    }
    change_media(c, &machine.id, &target, "--update", Some(path_text(&path)?))
}

pub(super) fn apply_eject_iso(c: Connection, machine: &Machine, target: String) -> Result<()> {
    change_media(c, &machine.id, &target, "--eject", None)
}

fn change_media(
    c: Connection,
    id: &str,
    target: &str,
    action: &str,
    source: Option<&str>,
) -> Result<()> {
    valid_target(target)?;
    let mut args = vec!["change-media", "--domain", id, "--path", target, action, "--current"];
    if let Some(path) = source {
        args.extend(["--source", path]);
    }
    backend::virsh(c, &args).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLER: &str =
        "<disk device='cdrom'><source file='/installer.iso'/><target dev='sdb'/></disk>";
    const TOOLS: &str = "/virtio-win.iso";

    #[test]
    fn automatic_guest_tools_mount_preserves_an_installer() -> Result<()> {
        let xml = format!(
            "<domain id='1'><devices><controller type='usb' model='qemu-xhci'/><disk device='disk'><target dev='sda'/></disk>{INSTALLER}</devices></domain>"
        );
        let doc = Document::parse(&xml)?;
        assert_eq!(
            tools_drive(&doc, TOOLS, None)?,
            ToolsDrive::Attach {
                target: "sdc".into(),
                bus: "usb",
                live: true
            }
        );
        assert_eq!(tools_drive(&doc, TOOLS, Some("sdb"))?, ToolsDrive::Replace("sdb"));
        assert!(tools_drive(&doc, TOOLS, Some("sda")).is_err());
        Ok(())
    }

    #[test]
    fn guest_tools_prefers_an_already_mounted_iso_then_an_empty_drive() -> Result<()> {
        let empty = "<disk device='cdrom'><target dev='sdc'/></disk>";
        let mounted =
            "<disk device='cdrom'><source file='/virtio-win.iso'/><target dev='sdd'/></disk>";
        let xml = format!("<domain><devices>{INSTALLER}{empty}</devices></domain>");
        assert_eq!(tools_drive(&Document::parse(&xml)?, TOOLS, None)?, ToolsDrive::Insert("sdc"));
        let xml = format!("<domain><devices>{INSTALLER}{empty}{mounted}</devices></domain>");
        assert_eq!(tools_drive(&Document::parse(&xml)?, TOOLS, None)?, ToolsDrive::Mounted);
        Ok(())
    }
}
