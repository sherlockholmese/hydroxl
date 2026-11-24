use super::*;
pub fn confirm(title: &str, label: &'static str, note: &str, m: Machine, op: Op) -> FormSpec {
    FormSpec::new(title.to_owned(), label, vec![], move |_| Ok(operation(m.clone(), op.clone())))
        .note(note)
        .danger()
}

/// The guest operating system, chosen as family then version from the libosinfo catalog.
/// Drives the machine's mark and the setup commands Hydroxl offers.
pub fn operating_system(m: &Machine) -> FormSpec {
    let machine = m.clone();
    let current = crate::backend::os_by_uri(&m.os).map_or("generic", |e| e.id.as_str());
    let family = crate::backend::family_of(current);
    let families: Vec<(&str, &str)> =
        crate::backend::os_families().iter().map(|(k, l)| (k.as_str(), l.as_str())).collect();
    let versions = crate::backend::os_versions(&family);
    let versions: Vec<(&str, &str)> =
        versions.iter().map(|(k, l)| (k.as_str(), l.as_str())).collect();
    FormSpec::new(
        "Operating system",
        "Save",
        vec![F::choice("Family", &families, &family), F::choice("Version", &versions, current)],
        move |v| {
            let uri = if v[1] == "generic" {
                String::new()
            } else {
                crate::backend::os_catalog()
                    .iter()
                    .find(|e| e.id == v[1])
                    .map(|e| e.uri.clone())
                    .filter(|uri| !uri.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Choose a version."))?
            };
            Ok(operation(machine.clone(), Op::OperatingSystem(uri)))
        },
    )
    .options(|index, v| (index == 1).then(|| crate::backend::os_versions(&v[0])))
}

/// Confirms retargeting a stopped machine at another CPU architecture.
pub fn architecture(m: &Machine, arch: &str) -> FormSpec {
    let label = super::label(ARCHITECTURES, arch);
    let host = std::env::consts::ARCH;
    let native = arch == host || (host == "x86_64" && arch == "i686");
    let mut note = format!("The machine type, firmware and controllers are reset for {label}.");
    if !native {
        note.push_str(" It runs under emulation, which is much slower.");
    }
    FormSpec::new(format!("Change to {label}"), "Change", vec![], {
        let machine = m.clone();
        let arch = arch.to_owned();
        move |_| Ok(operation(machine.clone(), Op::Architecture(arch.clone())))
    })
    .note(note)
}
