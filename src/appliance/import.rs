use super::{ImportSpec, archive::extract, manifest::manifest, ovf::parse};
use crate::backend::{self, Connection};
use crate::{files::path_text, xml::escape};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub fn import(
    c: Connection,
    spec: &ImportSpec,
    progress: &backend::progress::Reporter,
) -> Result<()> {
    progress.update("Reading appliance", None);
    if c != Connection::Session {
        bail!("Import appliances into User session. System machines can be imported from XML.");
    }
    if !spec.path.is_file() {
        bail!("Choose an existing OVA or OVF file.");
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let parent = spec.location.canonicalize().context("Choose an existing destination folder.")?;
    let folder = parent.join(format!("appliance-{stamp}"));
    fs::create_dir(&folder).context("Could not create a folder for the imported machine.")?;
    backend::disable_cow(&folder);
    let staging = folder.join("source");
    let mut registration_attempted = false;
    let result = (|| -> Result<()> {
        let is_ovf = spec
            .path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ovf"));
        let descriptor = if is_ovf {
            spec.path.canonicalize()?
        } else {
            fs::create_dir(&staging)
                .context("Could not create a folder for the archive contents.")?;
            let files = extract(&spec.path, &staging, progress)?;
            let descriptors: Vec<_> = files
                .iter()
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("ovf"))
                })
                .collect();
            if descriptors.len() != 1 {
                bail!("The OVA must contain exactly one OVF descriptor.");
            }
            for path in files.iter().filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mf"))
            }) {
                manifest(path)?;
            }
            (*descriptors[0]).clone()
        };
        progress.update("Reading machine definition", None);
        if is_ovf {
            let mf = descriptor.with_extension("mf");
            if mf.exists() {
                manifest(&mf)?;
            }
        }
        let definition = parse(&descriptor)?;
        let name = if spec.name.trim().is_empty() {
            definition.name.trim().to_owned()
        } else {
            spec.name.trim().to_owned()
        };
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            bail!("Choose a name between 1 and 64 characters.");
        }
        let names = backend::virsh(c, &["list", "--all", "--name"])?;
        if names.lines().any(|n| n == name) {
            bail!("A machine with that name already exists. Choose another name.");
        }
        let architecture = if spec.architecture == "auto" {
            definition.architecture
        } else {
            spec.architecture.clone()
        };
        if !backend::ARCHITECTURES.iter().any(|(v, _)| *v == architecture) {
            bail!("Choose a supported architecture.");
        }
        let can_virtualize =
            architecture == std::env::consts::ARCH && Path::new("/dev/kvm").exists();
        let virt_type = match spec.execution.as_str() {
            "emulate" => "qemu",
            "virtualize" if can_virtualize => "kvm",
            "virtualize" => {
                bail!("This appliance cannot use KVM on this host. Choose Automatic or Emulate.")
            }
            _ if can_virtualize => "kvm",
            _ => "qemu",
        };
        let mut disks = String::new();
        for (i, source) in definition.disks.iter().enumerate() {
            if i >= 26 {
                bail!("Appliances with more than 26 disks are not supported.");
            }
            let target = folder.join(format!("disk-{i}.qcow2"));
            backend::qemu_convert(
                &["-f", "vmdk", "-O", "qcow2", path_text(source)?, path_text(&target)?],
                progress.clone(),
                format!("Converting disk {} of {}", i + 1, definition.disks.len()),
            )?;
            // SATA retains a broadly supported guest boot device for x86 appliances.
            // ARM guests use VirtIO SCSI, with the controller configured below.
            let bus = if architecture == "aarch64" {
                "scsi"
            } else {
                "sata"
            };
            let device = format!("sd{}", char::from(b'a' + i as u8));
            disks.push_str(&format!("<disk type='file' device='disk'><driver name='qemu' type='qcow2'/><source file='{}'/><target dev='{device}' bus='{bus}'/></disk>", escape(path_text(&target)?)));
        }
        let memory = definition.memory_mib.to_string();
        let cpus = backend::Topology::one_socket(definition.cpus).virt_install();
        let mut args = vec![
            "--connect",
            c.uri(),
            "--name",
            &name,
            "--memory",
            &memory,
            "--vcpus",
            &cpus,
            "--arch",
            &architecture,
            "--virt-type",
            virt_type,
            "--import",
            "--disk",
            "none",
            "--os-variant",
            "generic",
            "--graphics",
            "spice,listen=none",
            "--channel",
            "spicevmc",
            "--noautoconsole",
            "--print-xml=1",
        ];
        let emulator = spec.qemu_build.emulator(&architecture)?;
        let mut boot = backend::qemu::installer_boot(&emulator)?;
        if spec.firmware == "uefi" || (spec.firmware == "auto" && definition.uefi) {
            boot.push_str(",uefi");
        }
        args.extend(["--boot", &boot]);
        if architecture == "aarch64" {
            args.extend(["--controller", "scsi,model=virtio-scsi"]);
        }
        if definition.networks == 0 {
            args.extend(["--network", "none"]);
        } else {
            for _ in 0..definition.networks.min(8) {
                args.extend([
                    "--network",
                    if architecture == "aarch64" {
                        "user,model=virtio"
                    } else {
                        "user,model=e1000"
                    },
                ]);
            }
        }
        progress.update("Registering machine", None);
        let xml = backend::run("virt-install", &args)?;
        let mut xml = spec.qemu_build.configure_xml(&xml)?.unwrap_or(xml);
        let insert =
            xml.rfind("</devices>").context("Could not generate the machine configuration.")?;
        xml.insert_str(insert, &disks);
        backend::record_owned_storage(&folder, &xml)?;
        registration_attempted = true;
        backend::define_xml(c, &xml)
    })();
    if result.is_err() && !registration_attempted {
        let _ = fs::remove_dir_all(&folder);
    } else if result.is_ok() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}
