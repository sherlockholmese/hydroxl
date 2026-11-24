use super::{Action, Connection, Machine, State, run};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
pub fn virsh(connection: Connection, args: &[&str]) -> Result<String> {
    let mut all = vec!["--connect", connection.uri()];
    all.extend_from_slice(args);
    run("virsh", &all)
}

pub fn machines(connection: Connection) -> Result<Vec<Machine>> {
    let ids = machine_ids(connection)?;
    let mut result = Vec::new();
    for id in &ids {
        match machine(connection, id) {
            Ok(machine) => result.push(machine),
            // A transient VM can vanish between enumeration and inspection.
            Err(_) if machine_ids(connection).is_ok_and(|current| !current.contains(id)) => {}
            Err(error) => return Err(error),
        }
    }
    result.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(result)
}

fn machine_ids(connection: Connection) -> Result<Vec<String>> {
    Ok(virsh(connection, &["list", "--all", "--uuid"])?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

fn machine(connection: Connection, id: &str) -> Result<Machine> {
    let xml = virsh(connection, &["dumpxml", id])?;
    let info = virsh(connection, &["dominfo", id])?;
    let field = |name| {
        info.lines()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(key, value)| (key == name).then(|| value.trim()))
    };
    let state = field("State").context("Libvirt did not return the machine state.")?;
    let mut machine = parse_machine(id, &xml, State::from_virsh(state))?;
    machine.saved_state = field("Managed save") == Some("yes");
    machine.autostart = field("Autostart") == Some("enable");
    Ok(machine)
}

pub(crate) struct MachineStatuses {
    states: HashMap<String, State>,
    saved: HashSet<String>,
}

impl MachineStatuses {
    pub(crate) fn read(connection: Connection) -> Result<Self> {
        let saved = virsh(connection, &["list", "--all", "--with-managed-save", "--uuid"])?;
        let states = virsh(connection, &["domstats", "--state", "--raw", "--nowait"])?;
        Ok(Self {
            states: parse_domain_states(&states),
            saved: saved.split_whitespace().map(str::to_owned).collect(),
        })
    }

    pub(crate) fn apply(&self, machine: &mut Machine) -> bool {
        let Some(&state) = self.states.get(&machine.name) else {
            return false;
        };
        let saved = state == State::Stopped && self.saved.contains(&machine.id);
        let changed = (machine.state, machine.saved_state) != (state, saved);
        machine.state = state;
        machine.saved_state = saved;
        changed
    }
}

fn parse_domain_states(output: &str) -> HashMap<String, State> {
    // `domstats --raw` returns virDomainState values grouped by domain name.
    let mut states = HashMap::new();
    let mut name = None;
    for line in output.lines() {
        if let Some(domain) = line.strip_prefix("Domain: '").and_then(|s| s.strip_suffix('\'')) {
            name = Some(domain);
        } else if let Some(value) = line.trim_start().strip_prefix("state.state=")
            && let Some(name) = name.take()
        {
            let state = match value {
                "1" | "2" | "4" => State::Running,
                "3" | "7" => State::Paused,
                "5" | "6" => State::Stopped,
                _ => State::Other,
            };
            states.insert(name.to_owned(), state);
        }
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_state_snapshot_updates_power_and_clears_managed_save_when_running() {
        let states = parse_domain_states(
            "Domain: 'Ubuntu 24.04'\n  state.state=3\n  state.reason=2\n\n\
             Domain: 'Debian Server'\n  state.state=5\n\n\
             Domain: 'Windows 11'\n  state.state=2\n",
        );
        let statuses = MachineStatuses {
            states,
            saved: ["ubuntu".into(), "debian".into()].into(),
        };
        let mut machines = demo_machines();
        assert!(statuses.apply(&mut machines[0]));
        assert_eq!(machines[0].state, State::Paused);
        assert!(!machines[0].saved_state);
        assert!(statuses.apply(&mut machines[2]));
        assert_eq!(machines[2].status(), "Suspended");
        assert!(statuses.apply(&mut machines[4]));
        assert_eq!(machines[4].state, State::Running);
        assert!(!statuses.apply(&mut machines[1]));
        assert!(!statuses.apply(&mut machines[4]));
    }
}

fn parse_machine(id: &str, xml: &str, state: State) -> Result<Machine> {
    let doc =
        roxmltree::Document::parse(xml).context("Could not read the machine configuration.")?;
    let root = doc.root_element();
    let text = |tag: &str| {
        root.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).unwrap_or("")
    };
    let memory_node = root.children().find(|n| n.has_tag_name("memory"));
    let memory: u64 = text("memory").parse().unwrap_or(0);
    let memory_mib = match memory_node.and_then(|n| n.attribute("unit")).unwrap_or("KiB") {
        "GiB" | "G" => memory * 1024,
        "MiB" | "M" => memory,
        "bytes" | "B" => memory / (1024 * 1024),
        _ => memory / 1024,
    };
    let disk = root
        .descendants()
        .find(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
        .and_then(|n| n.children().find(|c| c.has_tag_name("source")))
        .and_then(|n| n.attribute("file").or_else(|| n.attribute("dev")))
        .map(str::to_owned);
    let network = root
        .descendants()
        .find(|n| n.has_tag_name("interface"))
        .map(|n| match n.attribute("type").unwrap_or("") {
            "user" => "Shared network",
            "network" => "Virtual network",
            "bridge" => "Bridged network",
            _ => "Connected",
        })
        .unwrap_or("None")
        .to_owned();
    let display = root
        .descendants()
        .find(|&n| is_peer_dbus(n))
        .or_else(|| root.descendants().find(|n| n.has_tag_name("graphics")))
        .map(|n| match n.attribute("type").unwrap_or("") {
            "spice" => "SPICE",
            "vnc" => "VNC",
            "dbus" => "D-Bus",
            _ => "Graphics",
        })
        .unwrap_or("None")
        .to_owned();
    let os = root
        .descendants()
        .find(|n| n.has_tag_name("os") && n.attribute("id").is_some())
        .and_then(|n| n.attribute("id"))
        .unwrap_or("")
        .to_owned();
    let arch = super::qemu::domain_arch(root).unwrap_or("Unknown");
    Ok(Machine {
        id: id.to_owned(),
        name: text("name").to_owned(),
        state,
        cpus: text("vcpu").parse().unwrap_or(1),
        memory_mib,
        arch: arch.to_owned(),
        disk,
        network,
        display,
        acceleration: if root.attribute("type") == Some("kvm") {
            "KVM"
        } else {
            "QEMU"
        }
        .to_owned(),
        qemu_backend: super::qemu::domain_backend(root, arch),
        autostart: false,
        saved_state: false,
        notes: text("description").to_owned(),
        os,
        icon: String::new(),
        spice_viewer: true,
    })
}

/// Prepare the selected viewer's local display when starting a guest.
pub fn action(connection: Connection, id: &str, action: Action, prefer_spice: bool) -> Result<()> {
    match action {
        Action::Display => bail!("Open the display from the machine library."),
        Action::Start => {
            // Best effort: a missing in-app display must never block starting the guest.
            let saved = virsh(connection, &["dominfo", id])?
                .lines()
                .any(|line| line.starts_with("Managed save:") && line.contains("yes"));
            if !saved {
                let display_setup = if prefer_spice {
                    super::spice::prepare(connection, id)
                } else {
                    ensure_display(connection, id)
                };
                if let Err(error) = display_setup {
                    eprintln!("Could not prepare the guest display: {error:#}");
                }
            }
            virsh(connection, &["start", id])?;
        }
        other => {
            let command = match other {
                Action::Start => unreachable!(),
                Action::Shutdown => "shutdown",
                Action::Pause => "suspend",
                Action::Resume => "resume",
                Action::Display => unreachable!(),
            };
            virsh(connection, &[command, id])?;
        }
    }
    Ok(())
}

pub fn demo_machines() -> Vec<Machine> {
    [
        (
            "ubuntu",
            "Ubuntu 24.04",
            State::Stopped,
            4,
            8192,
            "x86_64",
            "http://ubuntu.com/ubuntu/24.04",
        ),
        (
            "fedora",
            "Fedora Workstation",
            State::Running,
            6,
            12288,
            "x86_64",
            "http://fedoraproject.org/fedora/41",
        ),
        (
            "debian",
            "Debian Server",
            State::Paused,
            2,
            4096,
            "x86_64",
            "http://debian.org/debian/12",
        ),
        (
            "arch",
            "Arch Linux",
            State::Stopped,
            4,
            4096,
            "x86_64",
            "http://archlinux.org/archlinux/rolling",
        ),
        ("windows", "Windows 11", State::Stopped, 4, 8192, "x86_64", "http://microsoft.com/win/11"),
        (
            "freebsd",
            "FreeBSD 14",
            State::Stopped,
            2,
            2048,
            "x86_64",
            "http://freebsd.org/freebsd/14.1",
        ),
    ]
    .into_iter()
    .map(|(id, name, state, cpus, memory_mib, arch, os)| Machine {
        id: id.into(),
        name: name.into(),
        state,
        cpus,
        memory_mib,
        arch: arch.into(),
        disk: Some(format!("/demo/{id}/disk.qcow2")),
        network: "Shared network".into(),
        display: "SPICE".into(),
        acceleration: "KVM".into(),
        qemu_backend: if id == "ubuntu" {
            super::QemuBackend::System
        } else {
            super::QemuBackend::Bundled
        },
        autostart: false,
        saved_state: false,
        notes: String::new(),
        os: os.into(),
        icon: String::new(),
        spice_viewer: true,
    })
    .collect()
}

pub(crate) fn ensure_stopped(
    connection: Connection,
    id: &str,
    changing_hardware: bool,
) -> Result<()> {
    if State::from_virsh(&virsh(connection, &["domstate", id])?) != State::Stopped {
        bail!("Power off the machine first.");
    }
    if changing_hardware {
        let info = virsh(connection, &["dominfo", id])?;
        if info.lines().any(|line| line.starts_with("Managed save:") && line.contains("yes")) {
            bail!("Discard the saved state before changing hardware.");
        }
    }
    Ok(())
}

/// Adds a local, socket-only VNC display (plus a USB tablet for absolute pointing) to a
/// stopped machine that lacks one, so Hydroxl can show the guest in its own window.
pub fn ensure_display(connection: Connection, id: &str) -> Result<()> {
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
    let has = |pick: &dyn Fn(roxmltree::Node) -> bool| doc.descendants().any(|n| pick(n));
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
    let devices = doc.descendants().find(|n| n.has_tag_name("devices")).context("No devices.")?;
    let at = devices.range().end - "</devices>".len();
    let mut updated = xml.clone();
    updated.insert_str(at, &add);
    let mut file = tempfile::Builder::new().suffix(".xml").tempfile()?;
    std::io::Write::write_all(&mut file, updated.as_bytes())?;
    let path = file.path().to_str().context("Invalid display configuration path.")?;
    virsh(connection, &["define", path]).map(|_| ())
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
    let mut file = tempfile::Builder::new().suffix(".xml").tempfile()?;
    std::io::Write::write_all(&mut file, b"<input type='tablet' bus='usb'/>")?;
    let path = file.path().to_str().context("Invalid tablet configuration path.")?;
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
