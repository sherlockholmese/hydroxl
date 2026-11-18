use super::*;
use crate::backend::network::{HostNetwork, physical_interfaces};

pub fn virtual_network() -> FormSpec {
    FormSpec::new(
        "New virtual network",
        "Create",
        vec![
            F::text("Name", "hydroxl"),
            F::choice("Type", &[("nat", "NAT"), ("isolated", "Isolated")], "nat"),
            F::text("Subnet", "192.168.150.0/24"),
        ],
        |v| {
            Ok(Job::Network(HostNetwork::Virtual {
                name: v[0].trim().into(),
                nat: v[1] == "nat",
                subnet: v[2].trim().into(),
            }))
        },
    )
}

pub fn host_bridge() -> FormSpec {
    let interfaces = physical_interfaces();
    let choices: Vec<(&str, &str)> = interfaces.iter().map(|i| (i.as_str(), i.as_str())).collect();
    if choices.is_empty() {
        return FormSpec::new("New host bridge", "Create", vec![], |_| {
            bail!("No wired interface is available.")
        })
        .note("No wired interface is available.")
        .unavailable();
    }
    FormSpec::new(
        "New host bridge",
        "Create bridge",
        vec![F::text("Name", "br0"), F::choice("Interface", &choices, choices[0].0)],
        |v| {
            Ok(Job::Network(HostNetwork::Bridge {
                name: v[0].trim().into(),
                interface: v[1].clone(),
            }))
        },
    )
    .note("The interface's connection moves to the bridge and may drop briefly.")
    .danger()
}
