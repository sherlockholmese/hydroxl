use super::{Connection, NewMachine, run, virsh};
use anyhow::{Context, Result, bail};
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
    // Resolve host prerequisites before allocating a folder or a new disk.
    let tpm = if spec.os_variant == "win11" {
        Some(super::tpm::emulator_model(connection, &spec.architecture, virt_type, None)?)
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
    let xml_path = folder.join("machine.xml");
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
                    args.extend(["--boot", "cdrom,hd"]);
                }
            }
        }
        // Host passthrough exposes nesting on its own. Windows 11 then turns on
        // virtualization-based security and runs itself nested, which makes it crawl.
        let cpu_arg = (virt_type == "kvm" && matches!(spec.architecture.as_str(), "x86_64" | "i686"))
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
            args.extend(["--boot", "uefi"]);
        }
        let xml = run("virt-install", &args)?;
        fs::write(&xml_path, xml)?;
        registration_attempted = true;
        virsh(connection, &["define", xml_path.to_str().context("Invalid configuration path.")?])?;
        Ok(())
    })();
    // Only newly allocated files are removed on failure. Existing disks are never touched.
    if result.is_err() && !registration_attempted {
        let _ = fs::remove_dir_all(&folder);
    }
    result
}

pub fn import(connection: Connection, path: &Path) -> Result<()> {
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
    virsh(connection, &["define", path.to_str().context("Invalid file path.")?])?;
    Ok(())
}
