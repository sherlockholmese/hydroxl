use super::{Connection, NewMachine, run, virsh};
use anyhow::{Context, Result, bail};
use std::io::Write as _;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub(crate) fn data_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .context("Set XDG_DATA_HOME to a writable directory.")?;
    let path = base.join("hydroxl/machines");
    fs::create_dir_all(&path).context("Could not create the machine directory.")?;
    Ok(path)
}

/// Turns off copy-on-write for files later created in `folder`. On btrfs, a CoW disk
/// image fragments with every guest write and the guest stalls on I/O. Other file
/// systems reject the flag, which is harmless.
pub fn disable_cow(folder: &Path) {
    if let Some(path) = folder.to_str() {
        let _ = run("chattr", &["+C", path]);
    }
}

/// Default parent folder for new machines; falls back to the home folder when the data
/// folder cannot be created.
pub fn machines_dir() -> PathBuf {
    data_dir().unwrap_or_else(|_| {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    })
}

pub(crate) fn define_xml(connection: Connection, xml: &str) -> Result<()> {
    let mut file = tempfile::Builder::new().suffix(".xml").tempfile()?;
    file.write_all(xml.as_bytes())?;
    virsh(connection, &["define", file.path().to_str().context("Invalid configuration path.")?])?;
    Ok(())
}

pub(crate) const STORAGE_RECORD: &str = ".hydroxl-storage.json";

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct OwnedStorage {
    pub(crate) uuid: String,
    pub(crate) disks: Vec<String>,
}

/// The caller has just created these disks. Record ownership for custom storage folders.
pub(crate) fn record_owned_storage(folder: &Path, xml: &str) -> Result<()> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let uuid = root
        .children()
        .find(|n| n.has_tag_name("uuid"))
        .and_then(|n| n.text())
        .context("The generated configuration is missing its UUID.")?;
    let disks: Vec<_> = root
        .children()
        .filter(|n| n.has_tag_name("devices"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name("source"))
        .filter_map(|n| n.attribute("file"))
        .map(Path::new)
        .filter(|path| path.parent() == Some(folder))
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect();
    if !disks.is_empty() {
        let record = OwnedStorage {
            uuid: uuid.into(),
            disks,
        };
        fs::write(folder.join(STORAGE_RECORD), serde_json::to_vec(&record)?)?;
    }
    Ok(())
}

fn disk_format(path: &Path) -> Result<String> {
    let info =
        run("qemu-img", &["info", "--output=json", path.to_str().context("Invalid disk path.")?])?;
    let json: serde_json::Value =
        serde_json::from_str(&info).context("Could not read the disk image.")?;
    Ok(json["format"].as_str().unwrap_or("raw").to_owned())
}

pub fn create(connection: Connection, spec: &NewMachine) -> Result<()> {
    spec.validate()?;
    if connection == Connection::System {
        bail!("Create machines in User session. System machines can be imported from XML.");
    }
    let virt_type = if spec.emulate {
        "qemu"
    } else {
        "kvm"
    };
    if virt_type == "kvm" && !Path::new("/dev/kvm").exists() {
        bail!("KVM is unavailable. Enable virtualization or choose Emulate.");
    }
    let emulator = spec.qemu_build.emulator(&spec.architecture)?;
    let mut boot = super::qemu::installer_boot(&emulator)?;
    // Resolve host prerequisites before allocating a folder or a new disk.
    let tpm = if spec.os_variant == "win11" {
        Some(super::tpm::emulator_model(
            connection,
            &spec.architecture,
            virt_type,
            None,
            Some(emulator.to_str().context("Invalid QEMU path.")?),
        )?)
    } else {
        None
    };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let slug: String = spec
        .name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut folder = spec.location.join(&slug);
    if folder.exists() {
        folder = spec.location.join(format!("{slug}-{stamp}"));
    }
    fs::create_dir(&folder).context("Could not create the machine folder.")?;
    disable_cow(&folder);
    let disk = spec.existing_disk.clone().unwrap_or_else(|| folder.join("disk.qcow2"));
    let mut registration_attempted = false;
    let result = (|| -> Result<()> {
        let format = if spec.existing_disk.is_some() {
            disk_format(&disk)?
        } else {
            run(
                "qemu-img",
                &[
                    "create",
                    "-f",
                    "qcow2",
                    disk.to_str().context("Invalid disk path.")?,
                    &format!("{}G", spec.disk_gib),
                ],
            )?;
            "qcow2".to_owned()
        };
        let disk_arg = format!("path={},format={format}", disk.display());
        let memory = (spec.memory_gib * 1024).to_string();
        let cpus = super::Topology::one_socket(spec.cpus).virt_install();
        let mut args = vec![
            "--connect",
            connection.uri(),
            "--name",
            spec.name.trim(),
            "--memory",
            &memory,
            "--vcpus",
            &cpus,
            "--arch",
            &spec.architecture,
            "--virt-type",
            virt_type,
            "--disk",
            &disk_arg,
            "--os-variant",
            &spec.os_variant,
            "--network",
            "user",
            "--graphics",
            "spice,listen=none",
            "--channel",
            "spicevmc",
            // Lets Hydroxl read guest addresses and set up shared folders inside the guest.
            "--channel",
            "unix,target.type=virtio,name=org.qemu.guest_agent.0",
            "--noautoconsole",
            "--print-xml=1",
        ];
        let uefi = spec.uefi || spec.os_variant == "win11";
        match &spec.iso {
            Some(iso) => args.extend(["--cdrom", iso.to_str().context("Invalid ISO path.")?]),
            None if spec.network_boot => args.push("--pxe"),
            // No media yet: an empty drive to mount later, tried before the disk.
            None => {
                args.extend(["--import", "--disk", "device=cdrom,bus=sata"]);
                if !uefi {
                    boot.push_str(",cdrom,hd");
                }
            }
        }
        // Host passthrough exposes nesting on its own. Windows 11 then turns on
        // virtualization-based security and runs itself nested, which makes it crawl.
        let cpu_arg = (virt_type == "kvm"
            && matches!(spec.architecture.as_str(), "x86_64" | "i686"))
        .then(super::nested_host_feature)
        .flatten()
        .map(|feature| format!("host-passthrough,disable={feature}"));
        if let Some(cpu_arg) = cpu_arg.as_deref() {
            args.extend(["--cpu", cpu_arg]);
        }
        let tpm_arg = tpm.map(|model| format!("emulator,model={model},version=2.0"));
        if let Some(tpm_arg) = tpm_arg.as_deref() {
            args.extend(["--tpm", tpm_arg]);
        }
        if uefi {
            boot.push_str(",uefi");
        }
        args.extend(["--boot", &boot]);
        let xml = run("virt-install", &args)?;
        let xml = spec.qemu_build.configure_xml(&xml)?.unwrap_or(xml);
        record_owned_storage(&folder, &xml)?;
        registration_attempted = true;
        define_xml(connection, &xml)
    })();
    // Only newly allocated files are removed on failure. Existing disks are never touched.
    if result.is_err() && !registration_attempted {
        let _ = fs::remove_dir_all(&folder);
    }
    result
}

const IMPORT_NAMESPACE: &str = "https://hydroxl.dev/xml/import/1";
const IMPORT_TAG: &str = "review-hardware";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImportHardware {
    pub(crate) vcpus: u32,
    pub(crate) memory_mib: u64,
}

fn review_hardware(doc: &roxmltree::Document) -> Option<ImportHardware> {
    let root = doc.root_element();
    let child = |name| root.children().find(|n| n.has_tag_name(name));
    let vcpus = child("vcpu").and_then(|n| n.text()).and_then(|s| s.trim().parse().ok())?;
    let memory = child("memory").or_else(|| child("currentMemory"))?;
    let size: u64 = memory.text()?.trim().parse().ok()?;
    let unit = memory.attribute("unit").unwrap_or("KiB").to_ascii_lowercase();
    let memory_mib = match unit.as_str() {
        "bytes" | "b" => size.div_ceil(1024 * 1024),
        "kb" => size.checked_mul(1000)?.div_ceil(1024 * 1024),
        "kib" | "k" => size.div_ceil(1024),
        "mb" => size.checked_mul(1000 * 1000)?.div_ceil(1024 * 1024),
        "mib" | "m" => size,
        "gb" => size.checked_mul(1000 * 1000 * 1000)?.div_ceil(1024 * 1024),
        "gib" | "g" => size.checked_mul(1024)?,
        "tb" => size.checked_mul(1000 * 1000 * 1000 * 1000)?.div_ceil(1024 * 1024),
        "tib" | "t" => size.checked_mul(1024 * 1024)?,
        _ => return None,
    };
    Some(ImportHardware {
        vcpus,
        memory_mib,
    })
}

fn has_hardware_review(doc: &roxmltree::Document) -> bool {
    doc.root_element().children().find(|n| n.has_tag_name("metadata")).is_some_and(|metadata| {
        metadata.children().any(|n| n.has_tag_name((IMPORT_NAMESPACE, IMPORT_TAG)))
    })
}

pub(crate) fn import_hardware(path: &Path) -> Result<Option<ImportHardware>> {
    if !path.is_file() {
        bail!("Choose a machine XML file.");
    }
    let xml = fs::read_to_string(path).context("Could not read the XML file.")?;
    let doc = roxmltree::Document::parse(&xml).context("Choose a valid libvirt XML file.")?;
    if has_hardware_review(&doc) {
        return review_hardware(&doc).map(Some).context(
            "Hardware review requires a valid virtual CPU count and memory size in the XML.",
        );
    }
    Ok(None)
}

pub(crate) fn add_hardware_review(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    if has_hardware_review(&doc) {
        return Ok(xml.to_owned());
    }
    let root = doc.root_element();
    let tag = format!("<hydroxl:{IMPORT_TAG} xmlns:hydroxl='{IMPORT_NAMESPACE}'/>");
    let mut updated = xml.to_owned();
    if let Some(metadata) = root.children().find(|node| node.has_tag_name("metadata")) {
        let range = metadata.range();
        let source = &xml[range.clone()];
        if let Some(open) = source.strip_suffix("/>") {
            updated.replace_range(range, &format!("{open}>{tag}</metadata>"));
        } else {
            let closing = source.rfind("</metadata").context("Invalid metadata section.")?;
            updated.insert_str(range.start + closing, &format!("  {tag}\n  "));
        }
    } else {
        let open_end = xml[root.range()].find('>').context("Invalid domain element.")?;
        updated.insert_str(
            root.range().start + open_end + 1,
            &format!("\n  <metadata>{tag}</metadata>"),
        );
    }
    Ok(updated)
}

pub fn import(
    connection: Connection,
    path: &Path,
    qemu_build: super::QemuBuild,
    hardware: Option<ImportHardware>,
) -> Result<()> {
    let xml = fs::read_to_string(path).context("Could not read the XML file.")?;
    let doc = roxmltree::Document::parse(&xml).context("Choose a valid libvirt XML file.")?;
    if !doc.root_element().has_tag_name("domain") {
        bail!("Choose a libvirt domain XML file.");
    }
    let name = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("name"))
        .and_then(|n| n.text())
        .context("The XML needs a machine name.")?;
    // Refuse to redefine an existing machine through the import flow.
    if virsh(connection, &["domuuid", name]).is_ok() {
        bail!("A machine with this name already exists. Rename it in the XML first.");
    }
    if let Some(uuid) =
        doc.root_element().children().find(|n| n.has_tag_name("uuid")).and_then(|n| n.text())
    {
        if virsh(connection, &["domname", uuid]).is_ok() {
            bail!("This machine is already in the library.");
        }
    }
    let xml = if has_hardware_review(&doc) {
        let original = review_hardware(&doc).context(
            "Hardware review requires a valid virtual CPU count and memory size in the XML.",
        )?;
        let hardware =
            hardware.context("Review the hardware settings before importing this machine.")?;
        let topology =
            (hardware.vcpus != original.vcpus).then(|| super::Topology::one_socket(hardware.vcpus));
        let memory = (hardware.memory_mib != original.memory_mib).then_some(hardware.memory_mib);
        super::hardware::configured_xml(xml, topology, memory)?
    } else {
        xml
    };
    let xml = qemu_build.configure_xml(&xml)?.unwrap_or(xml);
    define_xml(connection, &xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_adds_one_review_tag_and_retains_other_metadata() -> Result<()> {
        for metadata in ["", "<metadata/>", "<metadata><os xmlns='urn:other'/></metadata>"] {
            let xml = format!(
                "<domain type='qemu'>{metadata}<name>Test</name><memory unit='KiB'>2097152</memory><vcpu>4</vcpu></domain>"
            );
            let tagged = add_hardware_review(&xml)?;
            assert_eq!(tagged, add_hardware_review(&tagged)?);
            let doc = roxmltree::Document::parse(&tagged)?;
            assert!(has_hardware_review(&doc));
            assert_eq!(
                review_hardware(&doc),
                Some(ImportHardware {
                    vcpus: 4,
                    memory_mib: 2048
                })
            );
            if metadata.contains("urn:other") {
                assert!(tagged.contains("<os xmlns='urn:other'/>"));
            }
        }
        Ok(())
    }
}
