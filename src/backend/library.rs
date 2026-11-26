use super::{Connection, Machine, State, run};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
pub fn virsh(connection: Connection, args: &[&str]) -> Result<String> {
    let mut all = vec!["--connect", connection.uri()];
    all.extend_from_slice(args);
    run("virsh", &all)
}

pub(crate) struct MachineList {
    pub(crate) machines: Vec<Machine>,
    pub(crate) unavailable: usize,
}

pub(crate) fn machines(connection: Connection, previous: &[Machine]) -> Result<MachineList> {
    let ids = machine_ids(connection)?;
    let mut machines = Vec::with_capacity(ids.len());
    let mut failures = Vec::new();
    for id in ids {
        match machine(connection, &id) {
            Ok(machine) => machines.push(machine),
            Err(error) => failures.push((id, error)),
        }
    }
    // Transient machines can disappear between enumeration and inspection.
    if !failures.is_empty()
        && let Ok(current) = machine_ids(connection)
    {
        let current: HashSet<_> = current.iter().map(String::as_str).collect();
        failures.retain(|(id, _)| current.contains(id.as_str()));
    }
    let unavailable = failures.len();
    for (id, error) in &failures {
        eprintln!("Could not refresh machine {id}: {error:#}");
        if let Some(cached) = previous.iter().find(|machine| machine.id == *id) {
            machines.push(cached.clone());
        }
    }
    if machines.is_empty()
        && let Some((_, error)) = failures.into_iter().next()
    {
        return Err(error.context("Could not read the machine records from Libvirt."));
    }
    machines.sort_by_cached_key(|machine| machine.name.to_lowercase());
    Ok(MachineList {
        machines,
        unavailable,
    })
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

fn parse_machine(id: &str, xml: &str, state: State) -> Result<Machine> {
    let doc =
        roxmltree::Document::parse(xml).context("Could not read the machine configuration.")?;
    let root = doc.root_element();
    let text = |tag: &str| {
        root.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).unwrap_or("")
    };
    let name = text("name");
    anyhow::ensure!(
        root.has_tag_name("domain") && !name.trim().is_empty(),
        "Libvirt returned a machine configuration without a domain or a name."
    );
    let memory_node = root.children().find(|n| n.has_tag_name("memory"));
    let memory: u64 = text("memory").trim().parse().unwrap_or(0);
    let memory_mib = match memory_node.and_then(|n| n.attribute("unit")).unwrap_or("KiB") {
        "GiB" | "G" => {
            memory.checked_mul(1024).context("The configured machine memory is too large.")?
        }
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
        .find(|&n| super::is_peer_dbus(n))
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
        name: name.to_owned(),
        state,
        cpus: text("vcpu").trim().parse().unwrap_or(1),
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
