//! The private QEMU installation shared by Hydroxl's VM and image workflows.
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

const INSTALL_ROOT: &str = "/usr/lib/hydroxl/neptune";
const MISSING: &str = "Install Hydroxl's bundled Neptune QEMU before using live machines.";
const BUNDLED_LABEL: &str = "Bundled Neptune QEMU";
const SYSTEM_LABEL: &str = "System QEMU";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QemuBackend {
    Bundled,
    System,
    Custom,
    Unknown,
}

impl QemuBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bundled => BUNDLED_LABEL,
            Self::System => SYSTEM_LABEL,
            Self::Custom => "Custom QEMU",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QemuBuild {
    #[default]
    Bundled,
    System,
}

impl QemuBuild {
    pub const OPTIONS: &[(&str, &str)] = &[("bundled", BUNDLED_LABEL), ("system", SYSTEM_LABEL)];

    pub fn from_choice(value: &str) -> Result<Self> {
        match value {
            "bundled" => Ok(Self::Bundled),
            "system" => Ok(Self::System),
            _ => anyhow::bail!("Choose a QEMU backend."),
        }
    }

    pub fn emulator(self, arch: &str) -> Result<PathBuf> {
        let target = target(arch).context("Choose a supported guest architecture.")?;
        match self {
            Self::Bundled => binary(&format!("qemu-system-{target}")),
            Self::System => system_emulator(arch, target).with_context(|| {
                format!("Install System QEMU for {arch} or choose the bundled build.")
            }),
        }
    }

    pub(crate) fn configure_xml(self, xml: &str) -> Result<Option<String>> {
        let doc = roxmltree::Document::parse(xml)?;
        let domain = doc.root_element();
        let arch = domain_arch(domain).unwrap_or(std::env::consts::ARCH);
        with_emulator(xml, domain, &self.emulator(arch)?)
    }
}

/// Whether the Neptune bundle Hydroxl looks for is present.
pub fn bundle_installed() -> bool {
    root().is_ok_and(|root| root.join("bin").is_dir())
}

fn root() -> Result<PathBuf> {
    let root = std::env::var_os("HYDROXL_QEMU_ROOT").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(option_env!("HYDROXL_BUILT_QEMU_ROOT").unwrap_or(INSTALL_ROOT))
    });
    ensure!(root.is_absolute(), "HYDROXL_QEMU_ROOT must be an absolute path.");
    Ok(root)
}

fn binary(name: &str) -> Result<PathBuf> {
    let path = root()?.join("bin").join(name);
    ensure!(path.is_file(), "{MISSING} Missing {}.", path.display());
    Ok(path)
}

#[derive(serde::Deserialize)]
pub(crate) struct DiskInfo {
    pub(crate) format: String,
    #[serde(rename = "virtual-size")]
    pub(crate) virtual_size: u64,
}

pub(crate) fn disk_info(path: &str) -> Result<DiskInfo> {
    let output = super::run("qemu-img", &["info", "--output=json", path]).with_context(|| {
        format!("Could not read disk image {path}. Check that it is accessible.")
    })?;
    serde_json::from_str(&output).context("QEMU returned invalid disk image information.")
}

pub fn image_tool() -> Result<PathBuf> {
    binary("qemu-img")
}

pub fn triton_iso(arch: &str) -> Result<PathBuf> {
    ensure!(super::triton_arch(arch), super::TRITON_ARCH_UNSUPPORTED);
    let iso = root()?.join("share/hydroxl").join(format!("triton-{arch}.iso"));
    ensure!(iso.is_file(), "Reinstall Hydroxl's Neptune bundle to get the Triton driver disc.");
    Ok(iso)
}

fn target(arch: &str) -> Option<&str> {
    Some(match arch {
        "i386" | "i486" | "i586" | "i686" | "x86_64" => "x86_64",
        "armv7l" | "armv6l" | "arm" => "arm",
        "ppc64le" => "ppc64",
        "aarch64" | "riscv64" | "ppc" | "ppc64" => arch,
        _ => return None,
    })
}

fn system_emulator(arch: &str, target: &str) -> Option<PathBuf> {
    let target32 = if target == "x86_64" && arch != target {
        "i386"
    } else {
        target
    };
    let names = [format!("qemu-system-{target32}"), format!("qemu-system-{target}")];
    ["/usr/bin", "/usr/libexec"]
        .into_iter()
        .flat_map(|directory| names.iter().map(move |name| Path::new(directory).join(name)))
        .chain((arch == std::env::consts::ARCH).then(|| PathBuf::from("/usr/libexec/qemu-kvm")))
        .find(|path| path.is_file())
}

/// virt-install's comma-separated suboptions escape a literal comma as two commas.
pub fn installer_boot(emulator: &Path) -> Result<String> {
    let path = emulator.to_str().context("Use a valid UTF-8 path for bundled QEMU.")?;
    Ok(format!("emulator={}", path.replace(',', ",,")))
}

pub(crate) fn domain_emulator<'a>(root: roxmltree::Node<'a, '_>) -> Option<&'a str> {
    root.children()
        .find(|n| n.has_tag_name("devices"))?
        .children()
        .find(|n| n.has_tag_name("emulator"))?
        .text()
}

pub(crate) fn domain_arch<'a>(domain: roxmltree::Node<'a, '_>) -> Option<&'a str> {
    domain
        .children()
        .find(|n| n.has_tag_name("os"))?
        .children()
        .find(|n| n.has_tag_name("type"))?
        .attribute("arch")
}

pub(crate) fn supports_vdagent(connection: super::Connection, xml: &str) -> Result<bool> {
    let doc = roxmltree::Document::parse(xml)?;
    let domain = doc.root_element();
    let mut args = vec!["domcapabilities"];
    if let Some(emulator) = domain_emulator(domain) {
        args.extend(["--emulatorbin", emulator]);
    } else if let Some(arch) = domain_arch(domain) {
        args.extend(["--arch", arch]);
    }
    if let Some(kind) = domain.attribute("type") {
        args.extend(["--virttype", kind]);
    }
    let capabilities = super::virsh(connection, &args)?;
    let doc = roxmltree::Document::parse(&capabilities)?;
    let kinds = doc.descendants().find(|node| node.has_tag_name("console")).and_then(|node| {
        node.children()
            .find(|node| node.has_tag_name("enum") && node.attribute("name") == Some("type"))
    });
    if let Some(kinds) = kinds {
        return Ok(kinds
            .children()
            .any(|node| node.has_tag_name("value") && node.text() == Some("qemu-vdagent")));
    }
    // Older libvirt versions do not expose the chardev enum in domain capabilities.
    let binary = doc
        .root_element()
        .children()
        .find(|node| node.has_tag_name("path"))
        .and_then(|node| node.text())
        .context("Libvirt did not report the QEMU emulator path.")?;
    Ok(super::run(binary, &["-machine", "none", "-chardev", "help"])?
        .lines()
        .any(|line| line.trim() == "qemu-vdagent"))
}

pub(crate) fn domain_backend(domain: roxmltree::Node<'_, '_>, arch: &str) -> QemuBackend {
    let Some(path) = domain_emulator(domain).map(Path::new) else {
        return QemuBackend::Unknown;
    };
    if path.starts_with(Path::new(INSTALL_ROOT).join("bin"))
        || root().is_ok_and(|root| path.starts_with(root.join("bin")))
        || QemuBuild::Bundled.emulator(arch).is_ok_and(|bundle| same_file(&bundle, path))
    {
        return QemuBackend::Bundled;
    }
    if path.starts_with("/usr/bin") || path.starts_with("/usr/libexec") {
        QemuBackend::System
    } else {
        QemuBackend::Custom
    }
}

pub(super) fn same_file(left: &Path, right: &Path) -> bool {
    left == right
        || left.canonicalize().ok().is_some_and(|path| Some(path) == right.canonicalize().ok())
}

/// Auto-detect the build only for executables discovered from trusted install paths.
pub(super) fn probe_emulator(arch: &str, path: Option<&str>) -> Option<PathBuf> {
    let path = Path::new(path?);
    [QemuBuild::Bundled, QemuBuild::System]
        .into_iter()
        .filter_map(|build| build.emulator(arch).ok())
        .chain(target(arch).map(|target| {
            Path::new(INSTALL_ROOT).join("bin").join(format!("qemu-system-{target}"))
        }))
        .find(|candidate| same_file(candidate, path))
}

fn with_emulator(
    xml: &str,
    domain: roxmltree::Node<'_, '_>,
    path: &Path,
) -> Result<Option<String>> {
    ensure!(domain.has_tag_name("domain"), "Choose a libvirt domain XML file.");
    ensure!(
        matches!(domain.attribute("type"), Some("kvm" | "qemu")),
        "Choose a QEMU or KVM domain."
    );
    let path = path.to_str().context("Use a valid UTF-8 path for bundled QEMU.")?;
    if domain_emulator(domain) == Some(path) {
        return Ok(None);
    }
    let path = crate::xml::escape(path);
    let element = format!("<emulator>{path}</emulator>");
    let mut updated = xml.to_owned();
    let Some(devices) = domain.children().find(|n| n.has_tag_name("devices")) else {
        let (range, replacement) =
            crate::xml::append_child(xml, domain, &format!("<devices>{element}</devices>"))?;
        updated.replace_range(range, &replacement);
        return Ok(Some(updated));
    };
    if let Some(emulator) = devices.children().find(|n| n.has_tag_name("emulator")) {
        updated.replace_range(emulator.range(), &element);
    } else if xml[devices.range()].ends_with("/>") {
        updated.replace_range(
            devices.range().end - 2..devices.range().end,
            &format!(">{element}</devices>"),
        );
    } else {
        let at = xml[devices.range()].find('>').context("Invalid devices section.")? + 1;
        updated.insert_str(devices.range().start + at, &element);
    }
    Ok(Some(updated))
}

pub(crate) fn switch_to_bundled(connection: super::Connection, id: &str) -> Result<()> {
    super::ensure_stopped(connection, id, true)?;
    let xml = super::virsh(connection, &["dumpxml", id, "--inactive"])?;
    if let Some(updated) = QemuBuild::Bundled.configure_xml(&xml)? {
        super::define_xml(connection, &updated).context(
            "Could not switch this machine to bundled QEMU. Check the machine type and emulator permissions.",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_the_emulator_without_changing_machine_type_or_devices() {
        let xml = "<domain type='kvm'><os><type arch='x86_64' machine='pc-q35-11.1'>hvm</type></os><devices><emulator>/usr/bin/qemu-system-x86_64</emulator><disk/></devices></domain>";
        let path = Path::new("/test bundle/a&b/qemu-system-x86_64");
        let updated =
            with_emulator(xml, roxmltree::Document::parse(xml).unwrap().root_element(), path)
                .unwrap()
                .unwrap();
        let doc = roxmltree::Document::parse(&updated).unwrap();
        assert_eq!(domain_emulator(doc.root_element()), path.to_str());
        assert!(updated.contains("machine='pc-q35-11.1'>hvm</type>"));
        assert!(updated.contains("<disk/>"));
        assert!(with_emulator(&updated, doc.root_element(), path).unwrap().is_none());
        assert_eq!(target("i686"), Some("x86_64"));
        assert_eq!(target("armv7l"), Some("arm"));
    }
}
