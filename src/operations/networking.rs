use super::support::{append_child, attach, editable, esc, replace_child};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};

pub(super) fn apply_port_forward(
    c: Connection,
    machine: &Machine,
    protocol: String,
    host: u16,
    guest: u16,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    if !["tcp", "udp"].contains(&protocol.as_str()) || host == 0 || guest == 0 {
        bail!("Choose TCP or UDP and ports between 1 and 65535.");
    }
    let has_passt = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join("passt").is_file()));
    if !has_passt {
        bail!("Install passt to use port forwarding.");
    }
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let interface = doc
        .descendants()
        .find(|n| n.has_tag_name("interface") && n.attribute("type") == Some("user"))
        .context("Add a shared network first.")?;
    let range = interface.range();
    let mut network = xml[range.clone()].to_owned();
    network = replace_child(&network, "interface", "backend", "<backend type='passt'/>")?;
    network = append_child(
        &network,
        "interface",
        &format!(
            "<portForward proto='{}' address='127.0.0.1'><range start='{}' to='{}'/></portForward>",
            protocol, host, guest
        ),
    )?;
    xml.replace_range(range, &network);
    backend::define_xml(c, &xml)?;
    Ok(())
}

pub(super) fn apply_network(
    c: Connection,
    machine: &Machine,
    mode: String,
    source: String,
) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    let xml = match mode.as_str() {
        "user" => "<interface type='user'><model type='virtio'/></interface>".into(),
        "network" | "bridge" if !source.trim().is_empty() => format!(
            "<interface type='{}'><source {}='{}'/><model type='virtio'/></interface>",
            mode,
            mode,
            esc(source.trim())
        ),
        _ => bail!("Choose a network and enter its source name."),
    };
    attach(c, id, &xml)?;
    Ok(())
}

pub(super) fn remove_port_forward(c: Connection, machine: &Machine, rule: &str) -> Result<()> {
    editable(c, &machine.id)?;
    let mut xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let range = doc
        .descendants()
        .find(|n| n.has_tag_name("portForward") && &xml[n.range()] == rule)
        .map(|n| n.range())
        .context("The rule changed. Refresh and try again.")?;
    xml.replace_range(range, "");
    backend::define_xml(c, &xml)
}
