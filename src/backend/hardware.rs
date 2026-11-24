use super::{Connection, virsh};
use anyhow::{Context, Result, bail};
/// The virtualization extension a KVM guest can be given ("vmx" or "svm"), when the
/// host module allows nesting.
pub fn nested_host_feature() -> Option<&'static str> {
    [("kvm_intel", "vmx"), ("kvm_amd", "svm")].into_iter().find_map(|(module, feature)| {
        let parameter = format!("/sys/module/{module}/parameters/nested");
        std::fs::read_to_string(parameter)
            .is_ok_and(|value| matches!(value.trim(), "Y" | "1"))
            .then_some(feature)
    })
}

/// Guest CPU layout. The vCPU count is the product of the three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
}
impl Topology {
    pub fn one_socket(vcpus: u32) -> Self {
        Self {
            sockets: 1,
            cores: vcpus,
            threads: 1,
        }
    }

    pub(crate) fn virt_install(self) -> String {
        format!(
            "{},sockets={},cores={},threads={}",
            self.vcpus(),
            self.sockets,
            self.cores,
            self.threads
        )
    }

    pub fn vcpus(self) -> u32 {
        self.sockets.saturating_mul(self.cores).saturating_mul(self.threads)
    }
}

pub fn configure(
    connection: Connection,
    id: &str,
    topology: Option<Topology>,
    memory_mib: u64,
) -> Result<()> {
    super::ensure_stopped(connection, id, true)?;
    let xml = virsh(connection, &["dumpxml", id, "--inactive"])?;
    super::define_xml(connection, &configured_xml(xml, topology, Some(memory_mib))?)
}

pub(super) fn configured_xml(
    mut xml: String,
    topology: Option<Topology>,
    memory_mib: Option<u64>,
) -> Result<String> {
    if topology.is_some_and(|topology| !(1..=256).contains(&topology.vcpus())) {
        bail!("Use 1 to 256 virtual CPUs.");
    }
    if memory_mib.is_some_and(|memory| !(64..=1024 * 1024).contains(&memory)) {
        bail!("Use between 64 MiB and 1 TiB of memory.");
    }
    let doc = roxmltree::Document::parse(&xml)?;
    let topology_xml = topology.map(|topology| {
        format!(
            "<topology sockets='{}' dies='1' cores='{}' threads='{}'/>",
            topology.sockets, topology.cores, topology.threads
        )
    });
    let mut edits = Vec::new();
    let mut has_cpu = false;
    for node in doc.root_element().children().filter(|n| n.is_element()) {
        let replacement = match node.tag_name().name() {
            "vcpu" => topology.map(|topology| format!("<vcpu>{}</vcpu>", topology.vcpus())),
            "memory" | "currentMemory" => memory_mib.map(|memory| {
                let tag = node.tag_name().name();
                format!("<{tag} unit='MiB'>{memory}</{tag}>")
            }),
            "cpu" => {
                let Some(topology_xml) = &topology_xml else {
                    continue;
                };
                has_cpu = true;
                let text = &xml[node.range()];
                let open_end = text.find('>').context("The CPU configuration is invalid.")?;
                let self_closing = text[..open_end].ends_with('/');
                let open = if self_closing {
                    &text[..open_end - 1]
                } else {
                    &text[..open_end]
                };
                let inner: String = node
                    .children()
                    .filter(|c| c.is_element() && !c.has_tag_name("topology"))
                    .map(|c| &xml[c.range()])
                    .collect();
                Some(format!("{}>{topology_xml}{inner}</cpu>", open.trim_end()))
            }
            _ => None,
        };
        if let Some(value) = replacement {
            edits.push((node.range(), value));
        }
    }
    let root = doc.root_element();
    let mut additions = String::new();
    if !has_cpu && let Some(topology_xml) = topology_xml {
        additions.push_str(&format!("<cpu>{topology_xml}</cpu>"));
    }
    if let Some(topology) = topology
        && !root.children().any(|n| n.has_tag_name("vcpu"))
    {
        additions.push_str(&format!("<vcpu>{}</vcpu>", topology.vcpus()));
    }
    if let Some(memory) = memory_mib
        && !root.children().any(|n| n.has_tag_name("memory"))
    {
        additions.push_str(&format!("<memory unit='MiB'>{memory}</memory>"));
    }
    if !additions.is_empty() {
        let closing = xml[root.range()].rfind("</domain").context("Invalid domain element.")?;
        let end = root.range().start + closing;
        edits.push((end..end, additions));
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, value) in edits {
        xml.replace_range(range, &value);
    }
    Ok(xml)
}
