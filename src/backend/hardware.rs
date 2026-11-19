use super::{Connection, virsh};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
/// The virtualization extension a KVM guest can be given ("vmx" or "svm"), when the
/// host module allows nesting.
pub fn nested_host_feature() -> Option<&'static str> {
    [("kvm_intel", "vmx"), ("kvm_amd", "svm")].into_iter().find_map(|(module, feature)| {
        let parameter = format!("/sys/module/{module}/parameters/nested");
        fs::read_to_string(parameter)
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
    if topology.is_some_and(|topology| !(1..=256).contains(&topology.vcpus())) {
        bail!("Use 1 to 256 virtual CPUs.");
    }
    if !(64..=1024 * 1024).contains(&memory_mib) {
        bail!("Use between 64 MB and 1 TB of memory.");
    }
    super::ensure_stopped(connection, id, true)?;
    // Define the complete updated XML once, so CPU and memory are changed together.
    let xml = virsh(connection, &["dumpxml", id, "--inactive"])?;
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
            "memory" => Some(format!("<memory unit='MiB'>{memory_mib}</memory>")),
            "currentMemory" => {
                Some(format!("<currentMemory unit='MiB'>{memory_mib}</currentMemory>"))
            }
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
    if !has_cpu && let Some(topology_xml) = topology_xml {
        let end = doc.root_element().range().end - "</domain>".len();
        edits.push((end..end, format!("<cpu>{topology_xml}</cpu>")));
    }
    let mut updated = xml;
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, value) in edits {
        updated.replace_range(range, &value);
    }
    let path = std::env::temp_dir().join(format!(
        "hydroxl-config-{}-{}.xml",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::write(&path, updated)?;
    let result =
        virsh(connection, &["define", path.to_str().context("Invalid configuration path.")?]);
    let _ = fs::remove_file(path);
    result.map(|_| ())
}
