use super::ShareDriver;
use super::support::{append_child, editable, esc, path_text, replace_child, temporary_xml};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
pub enum ShareOwnership {
    Default,
    Preserve,
    Guest {
        uid: u32,
        gid: u32,
    },
}

#[derive(Clone, Debug)]
pub struct ShareConfig {
    pub path: PathBuf,
    pub tag: String,
    pub readonly: bool,
    pub driver: ShareDriver,
    pub ownership: ShareOwnership,
    /// Current mount tag when replacing an existing device.
    pub replace: Option<String>,
}

pub(super) fn apply_share(c: Connection, machine: &Machine, share: ShareConfig) -> Result<()> {
    let id = &machine.id;
    ensure!(share.path.is_absolute() && share.path.is_dir(), "Choose an existing shared folder.");
    ensure!(
        !share.tag.is_empty()
            && !share.tag.starts_with('-')
            && share.tag.len() <= 36
            && share.tag.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'),
        "Use a mount tag of up to 36 letters, numbers, hyphens, or underscores."
    );
    editable(c, id)?;
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let mapping = match share.ownership {
        ShareOwnership::Guest {
            uid,
            gid,
        } if share.driver == ShareDriver::Virtiofs => {
            ensure!(
                c == Connection::Session,
                "Guest UID/GID mapping requires a user-session machine."
            );
            Some(guest_idmap(uid, gid)?)
        }
        ShareOwnership::Preserve => None,
        _ => Some(String::new()),
    };
    if share.driver == ShareDriver::Virtiofs && c == Connection::Session {
        check_host_access(&share.path, share.readonly)?;
    }
    let updated = share_xml(&xml, &share, mapping.as_deref())?;
    if share.driver == ShareDriver::Virtiofs
        && !ShareDriver::virtiofs_available()
        && !has_custom_daemon(&updated, &share.tag)
    {
        bail!("Install virtiofsd on this computer to share folders with virtio-fs.");
    }
    if updated != xml {
        // Define the complete configuration in one operation. Failed edits leave the old
        // share and its memory backing in place.
        temporary_xml(&updated, |path| backend::virsh(c, &["define", path, "--validate"]).map(|_| ()))
            .context("Libvirt could not confirm the shared folder configuration. Refresh the library before retrying.")?;
    }
    Ok(())
}

fn check_host_access(path: &Path, readonly: bool) -> Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mode = libc::R_OK
        | libc::X_OK
        | if readonly {
            0
        } else {
            libc::W_OK
        };
    if unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) } != 0 {
        let error = std::io::Error::last_os_error();
        let message = if readonly {
            "The host account needs read and search access to the shared folder."
        } else {
            "The host account needs read, write, and search access to the shared folder. Choose Read only if the folder is read-only."
        };
        return Err(error).context(message);
    }
    Ok(())
}

fn guest_idmap(uid: u32, gid: u32) -> Result<String> {
    ensure!(uid != 0 && gid != 0, "Enter the Linux guest user's nonzero UID and GID.");
    let (host_uid, host_gid) = unsafe { (libc::getuid(), libc::getgid()) };
    ensure!(host_uid != 0, "Run Hydroxl as the host account that owns the shared files.");
    let owner = host_uid.to_string();
    let (uids, gids) = crate::backend::with_command_deadline(
        std::time::Instant::now() + std::time::Duration::from_secs(4),
        || -> Result<_> {
            Ok((backend::run("getsubids", &[&owner])?, backend::run("getsubids", &["-g", &owner])?))
        },
    ).context("Guest UID/GID mapping needs subordinate IDs and the getsubids tool. Set File ownership to Libvirt default to use the default mapping.")?;
    Ok(format!(
        "<idmap>{}{}</idmap>",
        map_id("uid", host_uid, uid, &uids)?,
        map_id("gid", host_gid, gid, &gids)?,
    ))
}

fn map_id(kind: &str, host: u32, guest: u32, ranges: &str) -> Result<String> {
    ensure!(guest != 0, "Enter a nonzero guest ID.");
    let (start, count) = ranges.lines().filter_map(|line| {
        let mut fields = line.split_whitespace().skip(2);
        let start: u32 = fields.next()?.parse().ok()?;
        let count: u32 = fields.next()?.parse().ok()?;
        let end = start.checked_add(count)?;
        (start > 0 && count >= guest && !(start..end).contains(&host)).then_some((start, count))
    }).next().with_context(|| format!("The host account needs a subordinate {kind} range large enough for guest ID {guest}."))?;

    let entry = |id: u32, host: u32, count: u32| {
        format!("<{kind} start='{id}' target='{host}' count='{count}'/>")
    };
    // Swap the guest IDs mapped to the host user and to the guest user's
    // former subordinate ID. The other guest IDs keep their default mapping.
    let mut xml = entry(0, start + guest - 1, 1);
    if guest > 1 {
        xml.push_str(&entry(1, start, guest - 1));
    }
    xml.push_str(&entry(guest, host, 1));
    if count > guest {
        xml.push_str(&entry(guest + 1, start + guest, count - guest));
    }
    Ok(xml)
}

fn share_xml(xml: &str, share: &ShareConfig, mapping: Option<&str>) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let devices = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("devices"))
        .context("The machine configuration is missing its devices section.")?;
    let mut old = None;
    for fs in devices.children().filter(|node| node.has_tag_name("filesystem")) {
        let tag = fs.children().find(|n| n.has_tag_name("target")).and_then(|n| n.attribute("dir"));
        if tag.is_some() && tag == share.replace.as_deref() {
            ensure!(old.is_none(), "The machine has multiple shares with the original mount tag.");
            old = Some(fs);
        } else {
            ensure!(tag != Some(&share.tag), "Another shared folder already uses this mount tag.");
        }
    }
    if share.replace.is_some() {
        ensure!(
            old.is_some(),
            "The original share has changed. Refresh the machine and try again."
        );
    }
    let source = format!("<source dir='{}'/>", esc(path_text(&share.path)?));
    let target = format!("<target dir='{}'/>", esc(&share.tag));
    let readonly = if share.readonly {
        "<readonly/>"
    } else {
        ""
    };
    let device = match old {
        Some(node) if ShareDriver::from_xml(&xml[node.range()]) == share.driver => {
            ensure!(
                !node
                    .children()
                    .any(|n| n.has_tag_name("source") && n.attribute("socket").is_some()),
                "Edit an externally managed virtio-fs share in the machine XML."
            );
            let mut device = xml[node.range()].to_owned();
            for (name, value) in
                [("source", source.as_str()), ("target", target.as_str()), ("readonly", readonly)]
            {
                device = replace_child(&device, "filesystem", name, value)?;
            }
            if let Some(mapping) = mapping {
                device = replace_child(&device, "filesystem", "idmap", mapping)?;
            }
            device
        }
        _ => match share.driver {
            ShareDriver::NineP => format!(
                "<filesystem type='mount' accessmode='mapped'>{source}{target}{readonly}</filesystem>"
            ),
            ShareDriver::Virtiofs => format!(
                "<filesystem type='mount' accessmode='passthrough'><driver type='virtiofs'/>{source}{target}{}{readonly}</filesystem>",
                mapping.unwrap_or("")
            ),
        },
    };
    let mut updated = xml.to_owned();
    let (range, replacement) = if let Some(old) = old {
        (old.range(), device)
    } else {
        crate::xml::append_child(xml, devices, &device)?
    };
    updated.replace_range(range, &replacement);
    if share.driver == ShareDriver::Virtiofs
        && let Some(backed) = with_shared_memory(&updated)?
    {
        updated = backed;
    }
    Ok(updated)
}

fn has_custom_daemon(xml: &str, tag: &str) -> bool {
    roxmltree::Document::parse(xml).is_ok_and(|doc| {
        doc.descendants()
            .filter(|node| node.has_tag_name("filesystem"))
            .find(|node| {
                node.children().any(|child| {
                    child.has_tag_name("target") && child.attribute("dir") == Some(tag)
                })
            })
            .and_then(|node| node.children().find(|child| child.has_tag_name("binary")))
            .and_then(|node| node.attribute("path"))
            .is_some_and(|path| Path::new(path).is_file())
    })
}

/// The domain with shareable RAM, or `None` when it already has it.
pub(super) fn with_shared_memory(xml: &str) -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let backing = doc.root_element().children().find(|n| n.has_tag_name("memoryBacking"));
    let shared = backing.is_some_and(|b| {
        b.children().any(|n| n.has_tag_name("access") && n.attribute("mode") == Some("shared"))
    });
    if shared {
        return Ok(None);
    }
    let updated = match backing {
        Some(node) => {
            // Keep hugepages or other settings; add the source and access mode.
            let mut inner = String::new();
            if !node.children().any(|n| n.has_tag_name("source")) {
                inner.push_str("<source type='memfd'/>");
            }
            let access = node.children().find(|n| n.has_tag_name("access"));
            if access.is_none() {
                inner.push_str("<access mode='shared'/>");
            }
            let text = &xml[node.range()];
            let mut result = xml.to_owned();
            if text.ends_with("/>") {
                result.replace_range(
                    node.range(),
                    &format!("<memoryBacking>{inner}</memoryBacking>"),
                );
            } else {
                let at = node.range().end - "</memoryBacking>".len();
                result.insert_str(at, &inner);
                if let Some(access) = access {
                    result.replace_range(access.range(), "<access mode='shared'/>");
                }
            }
            result
        }
        None => append_child(
            xml,
            "domain",
            "<memoryBacking><source type='memfd'/><access mode='shared'/></memoryBacking>",
        )?,
    };
    Ok(Some(updated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_guest_account_to_host_and_keeps_root_in_the_subordinate_range() -> Result<()> {
        let ranges = "0: 1000 100000 65536\n";
        assert_eq!(
            map_id("uid", 1000, 1000, ranges)?,
            "<uid start='0' target='100999' count='1'/>\
             <uid start='1' target='100000' count='999'/>\
             <uid start='1000' target='1000' count='1'/>\
             <uid start='1001' target='101000' count='64536'/>"
        );
        assert_eq!(
            map_id("gid", 200, 1, "0: 1000 300000 1")?,
            "<gid start='0' target='300000' count='1'/>\
             <gid start='1' target='200' count='1'/>"
        );
        assert!(map_id("uid", 1000, 1000, "0: 1000 900 65536").is_err());
        assert!(map_id("uid", 1000, 1000, "").is_err());
        Ok(())
    }

    #[test]
    fn edits_a_share_without_replacing_its_device_settings() -> Result<()> {
        let xml = "<domain type='qemu'><devices>\
            <filesystem type='mount' accessmode='passthrough'>\
            <driver type='virtiofs' queue='1024'/>\
            <source dir='/old'/><target dir='share'/>\
            <idmap><uid start='0' target='9000' count='1'/></idmap>\
            <address type='pci' slot='0x09'/></filesystem>\
            <filesystem type='mount'><source dir='/other'/><target dir='other'/></filesystem>\
            </devices></domain>";
        let mut share = ShareConfig {
            path: "/new & owned".into(),
            tag: "renamed".into(),
            readonly: true,
            driver: ShareDriver::Virtiofs,
            ownership: ShareOwnership::Preserve,
            replace: Some("share".into()),
        };
        let result = share_xml(xml, &share, None)?;
        assert!(result.contains("dir='/new &amp; owned'"));
        assert!(result.contains("<target dir='renamed'/>"));
        assert!(!result.contains("<target dir='share'/>"));
        assert!(result.contains("queue='1024'"));
        assert!(result.contains("slot='0x09'"));
        assert!(result.contains("target='9000'"));
        assert!(result.contains("<readonly/>"));
        assert!(result.contains("<access mode='shared'/>"));
        assert_eq!(
            roxmltree::Document::parse(&result)?
                .descendants()
                .filter(|n| n.has_tag_name("filesystem"))
                .count(),
            2
        );
        assert!(!share_xml(xml, &share, Some(""))?.contains("<idmap>"));

        share.tag = "other".into();
        assert!(share_xml(xml, &share, None).is_err());
        Ok(())
    }
}
