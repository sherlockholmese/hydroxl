//! Host networking a guest can attach to: libvirt virtual networks and host bridges.
use super::{Connection, run, run_with_input, virsh};
use anyhow::{Context, Result, bail};
use std::io::ErrorKind;

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
            let existing = virsh(connection, &["net-list", "--all", "--name"])
                .context("Could not check for existing virtual networks.")?;
            if existing.lines().any(|existing| existing == name) {
                bail!("A virtual network with this name already exists. Choose another name.");
            }
            let file = crate::xml::temporary(&xml)?;
            let path = crate::files::path_text(file.path())?;
            virsh(connection, &["net-define", path])
                .context("Could not define the virtual network. Check its name and subnet.")?;
            virsh(connection, &["net-start", name])
                .context("The network was created. Check its state and subnet in Libvirt before trying to start it again.")?;
            virsh(connection, &["net-autostart", name])
                .context("The network was started. Libvirt could not enable automatic startup.")?;
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
            )
            .context("The bridge was created. NetworkManager could not add the wired interface. Check the bridge before retrying.")?;
            run("nmcli", &["connection", "up", name])
                .context("The bridge was created. NetworkManager could not activate it. Check its state before retrying.")?;
            if connection == Connection::Session {
                allow_session_bridge(name).context("The bridge was created. To use it in the user session, authorize it in the QEMU bridge configuration.")?;
            }
            Ok(())
        }
    }
}

/// Session guests reach host bridges through qemu-bridge-helper, which only uses bridges
/// listed in /etc/qemu/bridge.conf. Appends one line through polkit's pkexec.
fn allow_session_bridge(name: &str) -> Result<()> {
    let line = format!("allow {name}");
    let current = match std::fs::read_to_string("/etc/qemu/bridge.conf") {
        Ok(current) => current,
        // A root-only or missing file can still be updated after polkit authorizes tee.
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied) => {
            String::new()
        }
        Err(error) => return Err(error).context("Could not read the QEMU bridge configuration."),
    };
    if current.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    run_with_input("pkexec", &["tee", "-a", "/etc/qemu/bridge.conf"], format!("{line}\n"))
        .context("Could not authorize the bridge through polkit.")?;
    Ok(())
}
