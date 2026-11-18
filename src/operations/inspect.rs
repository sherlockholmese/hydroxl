use super::capture::screenshot;
use super::snapshots::snapshot_labels;
use super::support::stamp;
use super::{Details, Device, Snapshot};
use crate::backend::{self, Connection, State};
use anyhow::Result;

pub fn details(c: Connection, id: &str) -> Result<Details> {
    let xml = backend::virsh(c, &["dumpxml", id])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let mut devices = Vec::new();
    for n in doc.descendants().filter(|n| n.has_tag_name("disk")) {
        let kind = n.attribute("device").unwrap_or("disk");
        let target = n
            .children()
            .find(|c| c.has_tag_name("target"))
            .and_then(|c| c.attribute("dev"))
            .unwrap_or("");
        let source = n
            .children()
            .find(|c| c.has_tag_name("source"))
            .and_then(|c| c.attribute("file").or_else(|| c.attribute("dev")))
            .unwrap_or("Empty");
        devices.push(Device {
            label: if kind == "cdrom" {
                "CD/DVD"
            } else {
                "Disk"
            }
            .into(),
            kind: kind.into(),
            target: target.into(),
            source: source.into(),
            xml: xml[n.range()].to_owned(),
        });
    }
    for n in doc.descendants().filter(|n| n.has_tag_name("filesystem")) {
        let tag = n
            .children()
            .find(|c| c.has_tag_name("target"))
            .and_then(|c| c.attribute("dir"))
            .unwrap_or("share");
        let source = n
            .children()
            .find(|c| c.has_tag_name("source"))
            .and_then(|c| c.attribute("dir"))
            .unwrap_or("");
        devices.push(Device {
            label: "Shared folder".into(),
            kind: "share".into(),
            target: tag.into(),
            source: source.into(),
            xml: xml[n.range()].to_owned(),
        });
    }
    for n in doc.descendants().filter(|n| n.has_tag_name("interface")) {
        let mac = n
            .children()
            .find(|n| n.has_tag_name("mac"))
            .and_then(|n| n.attribute("address"))
            .unwrap_or("Network");
        let source = n
            .children()
            .find(|n| n.has_tag_name("source"))
            .and_then(|n| n.attribute("network").or_else(|| n.attribute("bridge")))
            .unwrap_or("Shared network");
        devices.push(Device {
            label: "Network".into(),
            kind: "network".into(),
            target: mac.into(),
            source: source.to_owned(),
            xml: xml[n.range()].to_owned(),
        });
    }
    for n in doc
        .descendants()
        .filter(|n| n.has_tag_name("hostdev") && n.attribute("type") == Some("usb"))
    {
        let address = n.descendants().find(|n| n.has_tag_name("address"));
        let bus = address.and_then(|n| n.attribute("bus")).unwrap_or("?");
        let device = address.and_then(|n| n.attribute("device")).unwrap_or("?");
        devices.push(Device {
            label: "USB".into(),
            kind: "usb".into(),
            target: format!("Bus {bus}"),
            source: format!("Device {device}"),
            xml: xml[n.range()].to_owned(),
        });
    }

    for node in doc.descendants().filter(|n| n.has_tag_name("portForward")) {
        let protocol = node.attribute("proto").unwrap_or("tcp").to_uppercase();
        let host = node.attribute("address").unwrap_or("Any address");
        let ports = node.children().find(|n| n.has_tag_name("range"));
        let start = ports.and_then(|n| n.attribute("start")).unwrap_or("all");
        let guest = ports.and_then(|n| n.attribute("to")).unwrap_or(start);
        devices.push(Device {
            label: "Port forward".into(),
            kind: "forward".into(),
            target: format!("{protocol} {host}:{start}"),
            source: format!("Guest port {guest}"),
            xml: xml[node.range()].into(),
        });
    }
    let snapshot_result = backend::virsh(c, &["snapshot-list", id, "--name"]);
    let snapshot_error = snapshot_result
        .as_ref()
        .err()
        .map(|_| "Snapshots are unavailable for this machine.".to_owned());
    let snapshot_names = snapshot_result.unwrap_or_default();
    let labels = snapshot_labels(id);
    let mut snapshots = Vec::new();
    for name in snapshot_names.lines().filter(|s| !s.is_empty()) {
        let snapshot_xml = match backend::virsh(c, &["snapshot-dumpxml", id, name]) {
            Ok(xml) => xml,
            Err(error) if error.is::<backend::SnapshotNotFound>() => continue,
            Err(error) => return Err(error),
        };
        let doc = roxmltree::Document::parse(&snapshot_xml)?;
        let state = doc
            .descendants()
            .find(|n| n.has_tag_name("state"))
            .and_then(|n| n.text())
            .unwrap_or("unknown");
        let parent = doc
            .descendants()
            .find(|n| n.has_tag_name("parent"))
            .and_then(|n| n.children().find(|c| c.has_tag_name("name")))
            .and_then(|n| n.text())
            .map(str::to_owned);
        let parent = parent.map(|parent| labels.get(&parent).cloned().unwrap_or(parent));
        snapshots.push(Snapshot {
            name: name.into(),
            label: labels.get(name).cloned().unwrap_or_else(|| name.into()),
            state: snapshot_state_label(state).into(),
            parent,
        });
    }
    let running = State::from_virsh(&backend::virsh(c, &["domstate", id])?) == State::Running;
    let disk_sizes = devices
        .iter()
        .filter(|d| d.kind == "disk" && !d.target.is_empty())
        .filter_map(|d| {
            let info = backend::virsh(c, &["domblkinfo", id, &d.target]).ok()?;
            let field = |name: &str| {
                info.lines()
                    .find_map(|l| l.strip_prefix(name))
                    .and_then(|v| v.trim().parse::<u64>().ok())
            };
            Some((
                d.target.clone(),
                field("Capacity:")?,
                field("Physical:").or(field("Allocation:"))?,
            ))
        })
        .collect();
    let agent = has_agent_channel(&xml);
    let leases = doc
        .descendants()
        .any(|n| n.has_tag_name("interface") && n.attribute("type") == Some("network"));
    let addresses = if running {
        guest_addresses(c, id, agent, leases)
    } else {
        Vec::new()
    };
    let mut graphics = doc.descendants().filter(|node| node.has_tag_name("graphics"));
    let can_screendump = graphics.clone().next().is_some()
        && !graphics.any(|node| {
            node.attribute("type") == Some("dbus")
                || node.children().any(|child| {
                    child.has_tag_name("gl") && child.attribute("enable") == Some("yes")
                })
        });
    let preview = if running && can_screendump {
        let path = std::env::temp_dir().join(format!(
            "hydroxl-preview-{}-{}.png",
            std::process::id(),
            stamp()
        ));
        if screenshot(c, id, &path).is_ok() {
            Some(path)
        } else {
            None
        }
    } else {
        None
    };
    Ok(Details {
        snapshots,
        devices,
        xml,
        snapshot_error,
        preview,
        networks: backend::virsh(c, &["net-list", "--all", "--name"])
            .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
            .unwrap_or_default(),
        bridges: host_bridges(),
        disk_sizes,
        addresses,
    })
}

fn snapshot_state_label(state: &str) -> &'static str {
    match state.trim() {
        "shutoff" => State::Stopped.label(),
        "disk-snapshot" => "Disk only",
        "blocked" => "Blocked",
        "shutdown" => "Shutting down",
        _ => State::from_virsh(state).label(),
    }
}

/// Whether the domain has the QEMU guest agent's virtio channel.
pub fn has_agent_channel(xml: &str) -> bool {
    xml.contains("org.qemu.guest_agent.0")
}

/// IPv4 addresses from the guest agent, then from libvirt's DHCP leases. Each source is
/// asked only when the domain is configured for it, so a refresh never logs failures.
fn guest_addresses(c: Connection, id: &str, agent: bool, leases: bool) -> Vec<String> {
    let sources = [("agent", agent), ("lease", leases)];
    for source in sources.into_iter().filter(|(_, on)| *on).map(|(s, _)| s) {
        let Ok(out) = backend::virsh(c, &["domifaddr", id, "--source", source]) else {
            continue;
        };
        let found: Vec<String> = out
            .lines()
            .filter_map(|line| {
                let cols: Vec<&str> = line.split_whitespace().collect();
                let at = cols.iter().position(|c| *c == "ipv4")?;
                let address = cols.get(at + 1)?.split('/').next()?;
                (!address.starts_with("127.")).then(|| address.to_owned())
            })
            .collect();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Host bridge interfaces, from sysfs.
fn host_bridges() -> Vec<String> {
    let mut bridges: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("bridge").is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    bridges.sort();
    bridges
}

#[cfg(test)]
mod tests {
    use super::snapshot_state_label;

    #[test]
    fn labels_libvirt_snapshot_states() {
        assert_eq!(snapshot_state_label("shutoff"), "Powered off");
        assert_eq!(snapshot_state_label("disk-snapshot"), "Disk only");
        assert_eq!(snapshot_state_label("running"), "Running");
    }
}
