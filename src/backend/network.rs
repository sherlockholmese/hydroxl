//! Host networking a guest can attach to: libvirt virtual networks and host bridges.
use super::{Connection, run, virsh};
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub enum HostNetwork {
    /// A libvirt network with DHCP; NAT to the host's uplink, or isolated.
    Virtual {
        name: String,
        nat: bool,
        subnet: String,
    },
    /// A NetworkManager bridge that takes over a wired interface.
    Bridge {
        name: String,
        interface: String,
    },
}

/// Wired interfaces that can join a bridge: physical, not wireless, not already bridged.
pub fn physical_interfaces() -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    let p = e.path();
                    p.join("device").exists()
                        && !p.join("wireless").exists()
                        && !p.join("brport").exists()
                })
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 15
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("Use a name of up to 15 letters, digits, - or _.");
    }
    Ok(())
}

/// Parses `a.b.c.0/24` into the gateway, netmask and DHCP range libvirt needs.
fn subnet(cidr: &str) -> Result<(String, String, String, String)> {
    let (addr, bits) =
        cidr.trim().split_once('/').context("Use a subnet such as 192.168.150.0/24.")?;
    let bits: u32 = bits
        .parse()
        .ok()
        .filter(|b| (16..=29).contains(b))
        .context("Use a prefix from /16 to /29.")?;
    let ip: std::net::Ipv4Addr = addr.parse().context("Use a subnet such as 192.168.150.0/24.")?;
    let mask = u32::MAX << (32 - bits);
    let base = u32::from(ip) & mask;
    let last = base | !mask;
    let v4 = |n: u32| std::net::Ipv4Addr::from(n).to_string();
    Ok((v4(base + 1), v4(mask), v4(base + 2), v4(last - 1)))
}

pub fn create(connection: Connection, network: &HostNetwork) -> Result<()> {
    match network {
        HostNetwork::Virtual {
            name,
            nat,
            subnet: cidr,
        } => {
            if connection == Connection::Session {
                bail!("Virtual networks need the System connection. Switch in Preferences.");
            }
            valid_name(name)?;
            let (gateway, netmask, start, end) = subnet(cidr)?;
            let forward = if *nat {
                "<forward mode='nat'/>"
            } else {
                ""
            };
            let xml = format!(
                "<network><name>{name}</name>{forward}<bridge stp='on' delay='0'/>\
                 <ip address='{gateway}' netmask='{netmask}'><dhcp><range start='{start}' end='{end}'/></dhcp></ip></network>"
            );
            let path = std::env::temp_dir().join(format!("hydroxl-net-{}.xml", std::process::id()));
            std::fs::write(&path, xml)?;
            let defined =
                virsh(connection, &["net-define", path.to_str().context("Invalid path.")?]);
            let _ = std::fs::remove_file(&path);
            defined?;
            virsh(connection, &["net-start", name])?;
            virsh(connection, &["net-autostart", name])?;
            Ok(())
        }
        HostNetwork::Bridge {
            name,
            interface,
        } => {
            valid_name(name)?;
            if !physical_interfaces().contains(interface) {
                bail!("Choose a wired interface that is not already bridged.");
            }
            // NetworkManager authorizes through polkit; Hydroxl never runs sudo.
            run(
                "nmcli",
                &[
                    "connection",
                    "add",
                    "type",
                    "bridge",
                    "ifname",
                    name,
                    "con-name",
                    name,
                    "bridge.stp",
                    "no",
                    "ipv4.method",
                    "auto",
                    "ipv6.method",
                    "auto",
                ],
            )
            .context("NetworkManager could not create the bridge.")?;
            let port = format!("{name}-port-{interface}");
            run(
                "nmcli",
                &[
                    "connection",
                    "add",
                    "type",
                    "bridge-slave",
                    "ifname",
                    interface,
                    "master",
                    name,
                    "con-name",
                    &port,
                ],
            )?;
            run("nmcli", &["connection", "up", name])?;
            if connection == Connection::Session {
                allow_session_bridge(name)?;
            }
            Ok(())
        }
    }
}

/// Session guests reach host bridges through qemu-bridge-helper, which only uses bridges
/// listed in /etc/qemu/bridge.conf. Appends one line through polkit's pkexec.
fn allow_session_bridge(name: &str) -> Result<()> {
    let line = format!("allow {name}");
    let current = std::fs::read_to_string("/etc/qemu/bridge.conf").unwrap_or_default();
    if current.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    let mut child = Command::new("pkexec")
        .args(["tee", "-a", "/etc/qemu/bridge.conf"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("Install polkit to allow the bridge for user-session machines.")?;
    child
        .stdin
        .take()
        .context("pkexec input unavailable.")?
        .write_all(format!("{line}\n").as_bytes())?;
    if !child.wait()?.success() {
        bail!("The bridge was created, but allowing it for user-session machines was cancelled.");
    }
    Ok(())
}
