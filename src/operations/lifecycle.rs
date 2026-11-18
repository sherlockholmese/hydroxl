use super::support::{esc, path_text, stamp, stopped, temporary_xml};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    process::{Command, Stdio},
};

/// Asks the guest to restart and returns at once; the guest restarts on its own time.
///
/// The guest agent restarts the system from inside when it is configured. Otherwise
/// libvirt sends the ACPI power request and resets the same QEMU process once the guest
/// powers off, so the display stays connected and nothing here waits on the guest.
pub(super) fn apply_reboot(c: Connection, machine: &Machine) -> Result<()> {
    let id = machine.id.as_str();
    if backend::virsh(c, &["domstate", id])?.trim() == "paused" {
        backend::virsh(c, &["resume", id])?;
    }
    let xml = backend::virsh(c, &["dumpxml", id])?;
    if super::has_agent_channel(&xml)
        && backend::virsh(c, &["reboot", id, "--mode", "agent"]).is_ok()
    {
        return Ok(());
    }
    backend::virsh(c, &["reboot", id, "--mode", "acpi"])
        .or_else(|_| backend::virsh(c, &["reboot", id]))
        .context("The guest did not accept the restart. Use Reset to restart it immediately.")?;
    // Some guests (Windows by default) treat the request as the power button and switch
    // off instead of restarting. Watch in the background and start them again, so the
    // action still returns at once and the app never waits on the guest.
    let (id, in_app) = (id.to_owned(), !machine.spice_viewer);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_secs(2));
            match backend::virsh(c, &["domstate", &id]).as_deref().map(str::trim) {
                Ok("shut off") => {
                    if let Err(error) = backend::action(c, &id, crate::backend::Action::Start, in_app) {
                        eprintln!("Could not start the machine after restart: {error:#}");
                    }
                    return;
                }
                Ok("running" | "paused" | "in shutdown" | "pmsuspended") => {}
                _ => return,
            }
        }
    });
    Ok(())
}

pub(super) fn apply_force_stop(c: Connection, machine: &Machine) -> Result<()> {
    let id = machine.id.as_str();
    backend::virsh(c, &["destroy", id])?;
    Ok(())
}

pub(super) fn apply_save_state(c: Connection, machine: &Machine) -> Result<()> {
    let id = machine.id.as_str();
    backend::virsh(c, &["managedsave", id])?;
    Ok(())
}

pub(super) fn apply_discard_state(c: Connection, machine: &Machine) -> Result<()> {
    let id = machine.id.as_str();
    backend::virsh(c, &["managedsave-remove", id])?;
    Ok(())
}

pub(super) fn apply_disposable(c: Connection, machine: &Machine) -> Result<()> {
    disposable(c, machine)?;
    Ok(())
}

pub(super) fn apply_console(c: Connection, machine: &Machine) -> Result<()> {
    let id = machine.id.as_str();
    let terminal = std::env::var("TERMINAL").unwrap_or_else(|_| "xterm".into());
    Command::new(terminal)
        .args(["-e", "virsh", "--connect", c.uri(), "console", id])
        .stdin(Stdio::null())
        .spawn()
        .context("Set TERMINAL to your terminal executable, or install xterm.")?;
    Ok(())
}

pub(super) fn disposable(c: Connection, machine: &Machine) -> Result<()> {
    stopped(c, &machine.id)?;
    let folder = std::env::temp_dir().join(format!("hydroxl-disposable-{}", stamp()));
    fs::create_dir(&folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
    }
    let result = (|| -> Result<()> {
        let mut xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
        let doc = roxmltree::Document::parse(&xml)?;
        let mut edits = Vec::new();
        for node in doc.root_element().children() {
            if node.has_tag_name("uuid") {
                edits.push((node.range(), String::new()));
            }
            if node.has_tag_name("name") {
                edits.push((
                    node.range(),
                    format!("<name>{} Disposable</name>", esc(&machine.name)),
                ));
            }
        }
        for node in doc.descendants().filter(|n| n.has_tag_name("mac")) {
            edits.push((node.range(), String::new()));
        }
        // UEFI variable stores and passthrough devices are writable outside disk
        // overlays; reject those configurations rather than changing persistent data.
        if doc.descendants().any(|n| {
            n.has_tag_name("nvram")
                || n.has_tag_name("hostdev")
                || n.has_tag_name("filesystem")
                || n.has_tag_name("tpm")
        }) {
            bail!(
                "Disposable mode requires a machine without NVRAM, TPM, passthrough devices or shared folders."
            );
        }
        for (i, disk) in doc
            .descendants()
            .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
            .enumerate()
        {
            if disk.children().any(|n| n.has_tag_name("readonly")) {
                continue;
            }
            let source = disk
                .children()
                .find(|n| n.has_tag_name("source"))
                .context("A disk has no source.")?;
            let file =
                source.attribute("file").context("Disposable mode needs file-backed storage.")?;
            let info: serde_json::Value =
                serde_json::from_str(&backend::run("qemu-img", &["info", "--output=json", file])?)?;
            let format = info["format"].as_str().context("Could not identify a disk format.")?;
            let overlay = folder.join(format!("disk-{i}.qcow2"));
            backend::run(
                "qemu-img",
                &["create", "-f", "qcow2", "-F", format, "-b", file, path_text(&overlay)?],
            )?;
            edits.push((source.range(), format!("<source file='{}'/>", esc(path_text(&overlay)?))));
            if let Some(driver) = disk.children().find(|n| n.has_tag_name("driver")) {
                edits.push((driver.range(), "<driver name='qemu' type='qcow2'/>".into()));
            }
        }
        // Keep the original stopped machine untouched and launch a separate transient guest.
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in edits {
            xml.replace_range(range, &replacement);
        }
        temporary_xml(&xml, |path| backend::virsh(c, &["create", path]).map(|_| ()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&folder);
    }
    // Overlay files are retained under /tmp while QEMU holds them. They can be
    // removed after the transient guest exits; they are never merged into a base image.
    result
}
