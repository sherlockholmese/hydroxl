//! Retargets a stopped machine at another CPU architecture.
//!
//! Disks, network cards, shared folders and the rest of the guest's identity are kept.
//! Everything tied to the old platform is dropped so libvirt fills in the new platform's
//! defaults: the emulator binary, PCI controllers and addresses, x86-only features and
//! timers, firmware image paths, and a CPU model that cannot run under emulation.
use super::support::{editable, esc, temporary_xml};
use crate::backend::{self, ARCHITECTURES, Connection, Machine};
use anyhow::{Context, Result, bail};

/// Architectures this host can run: native ones under KVM, the rest under emulation.
/// Read from libvirt's capabilities, so only installed emulators are offered.
pub fn available(c: Connection) -> Vec<String> {
    let Ok(xml) = backend::virsh(c, &["capabilities"]) else {
        return Vec::new();
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return Vec::new();
    };
    let mut arches: Vec<String> = doc
        .descendants()
        .filter(|n| n.has_tag_name("arch") && n.parent().is_some_and(|p| p.has_tag_name("guest")))
        .filter_map(|n| n.attribute("name"))
        .filter(|name| ARCHITECTURES.iter().any(|(value, _)| value == name))
        .map(str::to_owned)
        .collect();
    arches.sort_by_key(|a| ARCHITECTURES.iter().position(|(value, _)| value == a));
    arches.dedup();
    arches
}

fn x86(arch: &str) -> bool {
    matches!(arch, "x86_64" | "i686")
}

/// Default machine type for an architecture, the one libvirt and virt-install choose.
pub fn default_machine(arch: &str) -> &'static str {
    match arch {
        "x86_64" | "i686" => "q35",
        "ppc64" | "ppc64le" => "pseries",
        "ppc" => "mac99",
        _ => "virt",
    }
}

pub(super) fn apply(c: Connection, machine: &Machine, arch: String) -> Result<()> {
    let id = machine.id.as_str();
    if !ARCHITECTURES.iter().any(|(value, _)| *value == arch) {
        bail!("Choose a supported architecture.");
    }
    editable(c, id)?;
    let xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let updated = retarget(&xml, &arch, kvm_available(&arch))?;
    let build = if machine.qemu_backend == backend::QemuBackend::System {
        backend::QemuBuild::System
    } else {
        backend::QemuBuild::Bundled
    };
    let updated = build.configure_xml(&updated)?.unwrap_or(updated);
    temporary_xml(&updated, |path| backend::virsh(c, &["define", path]).map(|_| ())).context(
        "libvirt could not use this architecture. Check that its QEMU emulator is installed.",
    )
}

fn kvm_available(arch: &str) -> bool {
    let host = std::env::consts::ARCH;
    (arch == host || (host == "x86_64" && arch == "i686"))
        && std::path::Path::new("/dev/kvm").exists()
}

/// The domain XML rewritten for `arch`. Pure, so it can be tested without libvirt.
pub(super) fn retarget(xml: &str, arch: &str, kvm: bool) -> Result<String> {
    let doc = roxmltree::Document::parse(xml).context("Could not read the configuration.")?;
    let root = doc.root_element();
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let remove = |node: roxmltree::Node, edits: &mut Vec<(std::ops::Range<usize>, String)>| {
        edits.push((node.range(), String::new()));
    };

    // <domain type='kvm'>: KVM only for the host's own architecture.
    let open_end = xml[root.range()].find('>').context("The configuration is malformed.")?;
    let open = &xml[root.range().start..root.range().start + open_end];
    let domain_type = if kvm {
        "kvm"
    } else {
        "qemu"
    };
    let new_open = match root.attribute("type") {
        Some(old) => open
            .replacen(&format!("type='{old}'"), &format!("type='{domain_type}'"), 1)
            .replacen(&format!("type=\"{old}\""), &format!("type='{domain_type}'"), 1),
        None => format!("{open} type='{domain_type}'"),
    };
    edits.push((root.range().start..root.range().start + open_end, new_open));

    let os = root
        .children()
        .find(|n| n.has_tag_name("os"))
        .context("The configuration has no OS section.")?;
    let os_type = os
        .children()
        .find(|n| n.has_tag_name("type"))
        .context("The configuration has no OS type.")?;
    edits.push((
        os_type.range(),
        format!(
            "<type arch='{}' machine='{}'>{}</type>",
            esc(arch),
            default_machine(arch),
            os_type.text().unwrap_or("hvm")
        ),
    ));
    // Firmware images are built per architecture. Keep the UEFI choice and let libvirt pick
    // the matching image; ARM and RISC-V `virt` boot through UEFI.
    for node in os.children().filter(|n| n.has_tag_name("loader") || n.has_tag_name("nvram")) {
        remove(node, &mut edits);
    }
    let needs_uefi = matches!(arch, "aarch64" | "armv7l" | "riscv64");
    let has_uefi =
        os.attribute("firmware") == Some("efi") || os.children().any(|n| n.has_tag_name("loader"));
    if needs_uefi || has_uefi {
        let os_open_end = xml[os.range()].find('>').context("The configuration is malformed.")?;
        let os_open = &xml[os.range().start..os.range().start + os_open_end];
        let os_open = os_open.trim_end_matches('/');
        if !os_open.contains("firmware=") {
            edits.push((
                os.range().start..os.range().start + os_open_end,
                format!("{os_open} firmware='efi'"),
            ));
        }
    }

    for node in root.descendants() {
        let name = node.tag_name().name();
        let parent = node.parent().map(|p| p.tag_name().name()).unwrap_or("");
        let drop = match name {
            // libvirt resolves the emulator for the new architecture.
            "emulator" => true,
            // Addresses and PCI/IDE/SATA controllers belong to the old machine type.
            "address" => parent != "source",
            "controller" => matches!(node.attribute("type"), Some("pci" | "ide" | "sata" | "fdc")),
            // PS/2 exists only on PC hardware; libvirt adds it again for x86.
            "input" => node.attribute("bus") == Some("ps2"),
            // Old-platform firmware hints.
            "smbios" | "vmcoreinfo" => true,
            _ => false,
        };
        if drop {
            remove(node, &mut edits);
        }
    }

    if !x86(arch) {
        if let Some(features) = root.children().find(|n| n.has_tag_name("features")) {
            for node in features.children().filter(|n| {
                matches!(
                    n.tag_name().name(),
                    "apic"
                        | "pae"
                        | "hyperv"
                        | "vmport"
                        | "smm"
                        | "kvm"
                        | "ioapic"
                        | "pvspinlock"
                        | "hap"
                        | "vmcoreinfo"
                )
            }) {
                remove(node, &mut edits);
            }
        }
        if let Some(clock) = root.children().find(|n| n.has_tag_name("clock")) {
            for timer in clock.children().filter(|n| n.has_tag_name("timer")) {
                remove(timer, &mut edits);
            }
        }
        // QXL, VGA and Bochs are PC display adapters; VirtIO GPU works everywhere.
        for model in root.descendants().filter(|n| {
            n.has_tag_name("model")
                && n.parent().is_some_and(|p| p.has_tag_name("video"))
                && !matches!(n.attribute("type"), Some("virtio" | "ramfb" | "none"))
        }) {
            edits.push((model.range(), "<model type='virtio' heads='1' primary='yes'/>".into()));
        }
        for tpm in root.descendants().filter(|n| n.has_tag_name("tpm")) {
            if let Some(model) = tpm.attribute("model") {
                if model != "tpm-tis-device" && arch == "aarch64" {
                    let open_end = xml[tpm.range()].find('>').unwrap_or(0);
                    let open = &xml[tpm.range().start..tpm.range().start + open_end];
                    edits.push((
                        tpm.range().start..tpm.range().start + open_end,
                        open.replacen(model, "tpm-tis-device", 1),
                    ));
                }
            }
        }
    }

    // Host CPU passthrough and host-model need KVM on the same architecture.
    if !kvm {
        if let Some(cpu) = root.children().find(|n| n.has_tag_name("cpu")) {
            let topology = cpu
                .children()
                .find(|n| n.has_tag_name("topology"))
                .map(|n| xml[n.range()].to_owned())
                .unwrap_or_default();
            edits.push((cpu.range(), format!("<cpu mode='maximum' check='none'>{topology}</cpu>")));
        }
    }

    apply_edits(xml, edits)
}

/// Applies non-overlapping range edits. A range inside one already being replaced is skipped.
fn apply_edits(xml: &str, mut edits: Vec<(std::ops::Range<usize>, String)>) -> Result<String> {
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut kept: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for edit in edits {
        if kept.last().is_some_and(|last| edit.0.start < last.0.end) {
            continue;
        }
        kept.push(edit);
    }
    let mut out = xml.to_owned();
    for (range, text) in kept.into_iter().rev() {
        out.replace_range(range, &text);
    }
    roxmltree::Document::parse(&out).context("Could not rewrite the configuration.")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::retarget;

    const PC: &str = "<domain type='kvm'><name>t</name><uuid>u</uuid>\
        <os firmware='efi'><type arch='x86_64' machine='pc-q35-9.1'>hvm</type>\
        <loader readonly='yes' type='pflash'>/usr/share/OVMF/OVMF_CODE.fd</loader>\
        <nvram>/var/lib/nvram/t.fd</nvram><boot dev='hd'/></os>\
        <features><acpi/><apic/><vmport state='off'/><hyperv><relaxed state='on'/></hyperv></features>\
        <cpu mode='host-passthrough'><topology sockets='1' cores='4' threads='1'/></cpu>\
        <clock offset='utc'><timer name='rtc' tickpolicy='catchup'/></clock>\
        <devices><emulator>/usr/bin/qemu-system-x86_64</emulator>\
        <disk type='file' device='disk'><source file='/d.qcow2'/><target dev='vda' bus='virtio'/>\
        <address type='pci' domain='0x0000' bus='0x04' slot='0x00' function='0x0'/></disk>\
        <controller type='pci' index='0' model='pcie-root'/><controller type='usb' model='qemu-xhci'/>\
        <input type='mouse' bus='ps2'/><input type='tablet' bus='usb'/>\
        <video><model type='qxl' heads='1'/></video></devices></domain>";

    #[test]
    fn x86_to_arm_drops_pc_platform_and_keeps_the_guest() {
        let out = retarget(PC, "aarch64", false).unwrap();
        assert!(out.contains("<domain type='qemu'>"));
        assert!(out.contains("<type arch='aarch64' machine='virt'>hvm</type>"));
        assert!(out.contains("firmware='efi'"));
        for gone in [
            "OVMF",
            "<nvram",
            "<apic",
            "<vmport",
            "<hyperv",
            "<emulator",
            "bus='ps2'",
            "type='pci'",
            "<timer",
            "qxl",
            "host-passthrough",
        ] {
            assert!(!out.contains(gone), "{gone} should be removed: {out}");
        }
        for kept in
            ["/d.qcow2", "<acpi/>", "qemu-xhci", "type='tablet'", "cores='4'", "type='virtio'"]
        {
            assert!(out.contains(kept), "{kept} should be kept: {out}");
        }
    }

    #[test]
    fn same_family_keeps_host_cpu_under_kvm() {
        let out = retarget(PC, "x86_64", true).unwrap();
        assert!(out.contains("<domain type='kvm'>"));
        assert!(out.contains("host-passthrough"));
        assert!(out.contains("<apic/>"));
        assert!(out.contains("machine='q35'"));
    }
}
