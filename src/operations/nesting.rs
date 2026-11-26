//! Expose the host's virtualization extensions to an x86 KVM guest.
use super::support::{append_child, editable};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, ensure};

pub(super) fn host_feature(machine: &Machine) -> Result<&'static str> {
    ensure!(
        matches!(machine.arch.as_str(), "x86_64" | "i686") && machine.acceleration == "KVM",
        "Nested virtualization requires an x86 KVM guest."
    );
    backend::nested_host_feature().context(
        "Nested virtualization is disabled on the host. Enable nesting in the host KVM module and retry.",
    )
}

pub(super) fn apply(c: Connection, machine: &Machine, enabled: bool) -> Result<()> {
    editable(c, &machine.id)?;
    let xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let cpu = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("cpu"))
        .context("Choose Host passthrough as the CPU model first.")?;
    if enabled {
        ensure!(
            doc.root_element().attribute("type") == Some("kvm")
                && cpu.attribute("mode") == Some("host-passthrough"),
            "Nested virtualization requires KVM and Host passthrough."
        );
    }
    let nested = enabled.then(|| host_feature(machine)).transpose()?;
    let mut result = xml.clone();
    let mut features = Vec::new();
    for feature in cpu
        .children()
        .filter(|n| n.has_tag_name("feature") && matches!(n.attribute("name"), Some("vmx" | "svm")))
    {
        features.push(feature);
    }
    let names: Vec<_> = features.iter().filter_map(|n| n.attribute("name")).collect();
    for feature in features.iter().rev() {
        result.replace_range(feature.range(), "");
    }
    if let Some(feature) = nested {
        result =
            append_child(&result, "cpu", &format!("<feature policy='require' name='{feature}'/>"))?;
    } else {
        // An explicitly selected host-passthrough CPU can expose nesting by
        // default. Keep the selected passthrough model and mask its extension.
        let feature = names.first().copied().or_else(|| host_feature(machine).ok());
        if let Some(feature) = feature {
            result = append_child(
                &result,
                "cpu",
                &format!("<feature policy='disable' name='{feature}'/>"),
            )?;
        }
    }
    if result != xml {
        backend::define_xml(c, &result)?;
    }
    Ok(())
}
