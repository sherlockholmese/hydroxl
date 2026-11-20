use super::*;
use crate::backend::QemuBuild;
const EXECUTION: &[(&str, &str)] = &[("virtualize", "Virtualize"), ("emulate", "Emulate")];
const FIRMWARE: &[(&str, &str)] = &[("default", "Automatic"), ("uefi", "UEFI")];
const BOOT_FROM: &[(&str, &str)] = &[("media", "Installation media"), ("network", "Network (PXE)")];
// Field positions in the creation wizard.
const ISO: usize = 0;
const FAMILY: usize = 1;
const OS: usize = 2;
const SOURCE: usize = 3;
const NAME: usize = 4;
const CPUS: usize = 5;
const MEMORY: usize = 6;
const DISK_MODE: usize = 7;
const DISK: usize = 8;
const DISK_PATH: usize = 9;
const ARCH: usize = 10;
const EXEC: usize = 11;
const BOOT: usize = 12;
const LOCATION: usize = 13;
const QEMU_BUILD: usize = 14;
const DISK_MODES: &[(&str, &str)] = &[("new", "New disk"), ("existing", "Existing disk")];

/// Guided creation, VMware-style: media, machine, review. The system is chosen as a
/// family, then a version, from the libosinfo catalog. Hardware defaults follow the
/// system; less common choices stay under Advanced. Media is optional.
pub fn create(location: PathBuf) -> FormSpec {
    let host = std::env::consts::ARCH;
    let families: Vec<(&str, &str)> =
        crate::backend::os_families().iter().map(|(k, l)| (k.as_str(), l.as_str())).collect();
    let location = location.to_string_lossy().into_owned();
    let mut location_field = F::path("Location", Picker::Folder).step(1);
    location_field.value = location;
    FormSpec::new(
        "New machine",
        "Create",
        vec![
            F::path("Installation ISO", Picker::Iso),
            F::choice("Operating system", &families, "generic").row(1),
            F::choice("Version", &[("generic", "Generic")], "generic").row(1),
            F::choice("Boot from", BOOT_FROM, "media").advanced(),
            F::text("Name", "").step(1),
            F::text("CPU cores", "2").step(1).row(2),
            F::text("Memory", "4").suffix("GB").step(1).row(2),
            F::choice("Hard disk", DISK_MODES, "new").step(1).row(4),
            F::text("Disk size", "32").suffix("GB").step(1).row(4),
            F::path("Disk image", Picker::Disk).step(1),
            F::choice("Architecture", ARCHITECTURES, host).step(1).advanced().row(3),
            F::choice("Execution", EXECUTION, "virtualize").step(1).advanced().row(3),
            F::choice("Firmware", FIRMWARE, "default").step(1).advanced().row(3),
            location_field,
            F::choice("QEMU backend", QemuBuild::OPTIONS, "bundled").step(1),
        ],
        new_machine_job,
    )
    .steps(&["Media", "Machine"])
    .visible(|index, v| match index {
        DISK => v[DISK_MODE] == "new",
        DISK_PATH => v[DISK_MODE] == "existing",
        _ => true,
    })
    .options(|index, v| (index == OS).then(|| crate::backend::os_versions(&v[FAMILY])))
    .derive(move |v| {
        let mut out = Vec::new();
        let iso = v[ISO].trim();
        if !iso.is_empty() {
            let stem = std::path::Path::new(iso)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(os) = crate::backend::infer_os(&stem) {
                out.push((FAMILY, crate::backend::family_of(os)));
                out.push((OS, os.to_owned()));
            }
            out.push((NAME, suggested_name(&stem, &v[OS])));
        } else if v[OS] != "generic" {
            out.push((NAME, suggested_name("", &v[OS])));
        }
        let os = v[OS].as_str();
        let (cpus, memory, disk, firmware) = if os == "win11" || os.starts_with("win2k2") {
            ("4", "8", "80", "uefi")
        } else if os.starts_with("win") {
            ("4", "8", "64", "default")
        } else {
            ("2", "4", "32", "default")
        };
        out.extend([
            (CPUS, cpus.to_owned()),
            (MEMORY, memory.to_owned()),
            (DISK, disk.to_owned()),
            (BOOT, firmware.to_owned()),
            (
                EXEC,
                if v[ARCH] == host {
                    "virtualize"
                } else {
                    "emulate"
                }
                .to_owned(),
            ),
        ]);
        out
    })
    .check(|step, v| {
        match step {
            0 => {
                let iso = v[ISO].trim();
                if !iso.is_empty() && !PathBuf::from(iso).is_file() {
                    bail!("Choose an existing ISO, or leave it empty to install later.")
                }
            }
            1 => {
                new_machine(v)?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn new_machine(v: &[String]) -> Result<NewMachine> {
    let iso = v[ISO].trim();
    let spec = NewMachine {
        name: name(&v[NAME])?,
        os_variant: v[OS].clone(),
        architecture: v[ARCH].clone(),
        qemu_build: QemuBuild::from_choice(&v[QEMU_BUILD])?,
        emulate: v[EXEC] == "emulate",
        uefi: v[BOOT] == "uefi",
        cpus: number(v, CPUS, "CPU count")?,
        memory_gib: number(v, MEMORY, "memory size")?,
        disk_gib: if v[DISK_MODE] == "new" {
            number(v, DISK, "disk size")?
        } else {
            0
        },
        existing_disk: (v[DISK_MODE] == "existing").then(|| PathBuf::from(v[DISK_PATH].trim())),
        iso: (!iso.is_empty()).then(|| PathBuf::from(iso)),
        network_boot: v[SOURCE] == "network",
        location: PathBuf::from(v[LOCATION].trim()),
    };
    spec.validate()?;
    Ok(spec)
}
fn new_machine_job(v: &[String]) -> Result<Job> {
    Ok(Job::Create(new_machine(v)?))
}

/// Readable machine name: the catalog name when the system is known
/// ("Ubuntu 24.04 LTS"), otherwise the image name without build tokens.
fn suggested_name(stem: &str, os: &str) -> String {
    if (stem.is_empty() || os.starts_with("win")) && os != "generic" && !os.is_empty() {
        if let Some(entry) = crate::backend::os_catalog().iter().find(|e| e.id == os) {
            return entry.name.trim_start_matches("Microsoft ").to_owned();
        }
    }
    const STOP: &[&str] = &[
        "desktop", "live", "amd64", "x86", "x64", "arm64", "aarch64", "i386", "i686", "dvd",
        "dvd1", "netinst", "install", "boot", "iso", "english", "release", "minimal", "64bit",
    ];
    let words: Vec<String> = stem
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .take_while(|w| {
            let w = w.to_ascii_lowercase();
            !STOP.contains(&w.as_str()) && w != "x86_64"
        })
        .take(3)
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect();
    let joined = words.join(" ");
    if joined.is_empty() {
        "New machine".into()
    } else {
        joined.chars().take(64).collect()
    }
}

pub fn import_xml() -> FormSpec {
    use crate::backend::{ImportHardware, import_hardware};
    use std::{cell::RefCell, path::Path, rc::Rc};
    let selection = Rc::new(RefCell::new((String::new(), None::<ImportHardware>)));
    let derived = selection.clone();
    FormSpec::new(
        "Import libvirt XML",
        "Import",
        vec![
            F::path("Libvirt configuration", Picker::Xml),
            F::choice("QEMU backend", QemuBuild::OPTIONS, "bundled"),
            F::text("Virtual CPUs", "").row(1),
            F::text("Memory", "").suffix("MB").row(1),
        ],
        |v| {
            let path = PathBuf::from(&v[0]);
            if !path.is_file() {
                bail!("Choose a machine XML file.");
            }
            let hardware = if import_hardware(&path)?.is_some() {
                let vcpus = number(v, 2, "virtual CPU count")?;
                let memory_mib = number(v, 3, "memory size")?;
                if !(1..=256).contains(&vcpus) {
                    bail!("Use 1 to 256 virtual CPUs.");
                }
                if !(64..=1024 * 1024).contains(&memory_mib) {
                    bail!("Use between 64 MB and 1 TB of memory.");
                }
                Some(ImportHardware {
                    vcpus,
                    memory_mib,
                })
            } else {
                None
            };
            Ok(Job::Import(path, QemuBuild::from_choice(&v[1])?, hardware))
        },
    )
    .derive(move |v| {
        let mut selection = derived.borrow_mut();
        let path = v[0].trim();
        if selection.0 != path {
            *selection = (path.into(), import_hardware(Path::new(path)).ok().flatten());
        }
        selection.1.map_or_else(Vec::new, |hardware| {
            vec![(2, hardware.vcpus.to_string()), (3, hardware.memory_mib.to_string())]
        })
    })
    .visible(move |index, _| index < 2 || selection.borrow().1.is_some())
}

pub fn import(location: PathBuf) -> FormSpec {
    let mut location_field = F::path("Location", Picker::Folder);
    location_field.value = location.to_string_lossy().into_owned();
    let mut architectures = vec![("auto", "From appliance")];
    architectures.extend_from_slice(ARCHITECTURES);
    FormSpec::new(
        "Import OVA or OVF",
        "Import",
        vec![
            F::path("OVA or OVF appliance", Picker::Appliance),
            F::text("Name", ""),
            F::choice("Architecture", &architectures, "auto").advanced(),
            F::choice(
                "Execution",
                &[("auto", "Automatic"), ("virtualize", "Virtualize"), ("emulate", "Emulate")],
                "auto",
            )
            .advanced(),
            F::choice(
                "Firmware",
                &[("auto", "From appliance"), ("uefi", "UEFI"), ("bios", "BIOS")],
                "auto",
            )
            .advanced(),
            location_field,
            F::choice("QEMU backend", QemuBuild::OPTIONS, "bundled"),
        ],
        |v| {
            let path = PathBuf::from(&v[0]);
            if !path.is_file() {
                bail!("Choose an existing OVA or OVF file.");
            }
            let location = PathBuf::from(v[5].trim());
            if !location.is_dir() {
                bail!("Choose an existing destination folder.");
            }
            Ok(Job::Appliance(crate::appliance::ImportSpec {
                path,
                location,
                name: v[1].clone(),
                architecture: v[2].clone(),
                qemu_build: QemuBuild::from_choice(&v[6])?,
                execution: v[3].clone(),
                firmware: v[4].clone(),
            }))
        },
    )
    .derive(|v| {
        let stem = std::path::Path::new(v[0].trim())
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem.is_empty() {
            vec![]
        } else {
            vec![(1, suggested_name(&stem, ""))]
        }
    })
}

pub fn import_path(path: PathBuf, location: PathBuf) -> FormSpec {
    let is_xml =
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let mut spec = if is_xml {
        import_xml()
    } else {
        import(location)
    };
    if let Some(field) = spec.fields.first_mut() {
        field.value = path.to_string_lossy().into_owned();
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_registration_forms_default_to_bundled_qemu_and_submit_system() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("machine.xml");
        std::fs::write(&path, "<domain type='kvm'/>")?;
        let location = temp.path().to_path_buf();
        for form in [create(location.clone()), import(location), import_xml()] {
            let index = form.fields.iter().position(|field| field.label == "QEMU backend").unwrap();
            let mut values: Vec<_> = form.fields.iter().map(|field| field.value.clone()).collect();
            assert_eq!(values[index], "bundled");
            values[index] = "system".into();
            if form.steps.len() == 2 {
                values[NAME] = "Test guest".into();
            } else {
                values[0] = path.to_string_lossy().into_owned();
            }
            let build = match (form.submit)(&values)? {
                Job::Create(spec) => spec.qemu_build,
                Job::Import(_, build, _) => build,
                Job::Appliance(spec) => spec.qemu_build,
                _ => unreachable!(),
            };
            assert_eq!(build, QemuBuild::System);
        }
        Ok(())
    }

    #[test]
    fn tagged_xml_reveals_hardware_and_requires_values_before_import() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("machine.xml");
        let xml =
            "<domain><name>Fixture</name><memory unit='GiB'>6</memory><vcpu>8</vcpu></domain>";
        let form = import_path(path.clone(), temp.path().into());
        let mut values: Vec<_> = form.fields.iter().map(|field| field.value.clone()).collect();
        let derive = form.derive.as_ref().unwrap();
        let visible = form.visible.as_ref().unwrap();
        std::fs::write(&path, crate::backend::add_hardware_review(xml)?)?;
        assert!(!visible(2, &values));
        for (index, value) in derive(&values) {
            values[index] = value;
        }
        assert!(visible(2, &values) && visible(3, &values));
        assert_eq!(&values[2..], ["8", "6144"]);
        values[2] = "4".into();
        assert!(matches!(
            (form.submit)(&values)?,
            Job::Import(
                _,
                _,
                Some(crate::backend::ImportHardware {
                    vcpus: 4,
                    memory_mib: 6144
                })
            )
        ));
        values[2] = "0".into();
        assert!((form.submit)(&values).is_err());
        std::fs::write(&path, xml)?;
        assert!(matches!((form.submit)(&values)?, Job::Import(_, _, None)));
        Ok(())
    }
}
