use super::*;
pub fn mount(m: &Machine, target: String) -> FormSpec {
    let machine = m.clone();
    FormSpec::new(
        "Mount installation image",
        "Mount",
        vec![F::path("ISO image", Picker::Iso)],
        move |v| {
            if !PathBuf::from(&v[0]).is_file() {
                bail!("Choose an existing ISO image.");
            }
            Ok(operation(
                machine.clone(),
                Op::MountIso {
                    path: v[0].clone().into(),
                    target: target.clone(),
                },
            ))
        },
    )
}

pub fn resize(m: &Machine, target: String) -> FormSpec {
    let machine = m.clone();
    FormSpec::new("Grow disk", "Resize", vec![F::text("New size", "").suffix("GB")], move |v| {
        Ok(operation(
            machine.clone(),
            Op::ResizeDisk {
                target: target.clone(),
                size_gib: number(v, 0, "disk size")?,
            },
        ))
    })
}

pub fn compact(m: &Machine, target: String) -> FormSpec {
    confirm(
        "Compact disk",
        "Compact",
        "Requires free disk space and no snapshots. The guest data is kept.",
        m.clone(),
        Op::CompactDisk(target),
    )
}

pub fn guest_tools(m: &Machine, target: Option<String>) -> FormSpec {
    let machine = m.clone();
    let drive = match &target {
        Some(target) => format!("Replaces any disc in {target} with the VirtIO driver ISO."),
        None => "Uses an empty CD/DVD drive or adds one for the VirtIO driver ISO.".to_owned(),
    };
    FormSpec::new("Mount Windows guest tools", "Download and mount", vec![], move |_| {
        Ok(operation(machine.clone(), Op::GuestTools(target.clone())))
    })
    .note(format!("{drive} Open the disc inside Windows to install the drivers."))
}

pub fn disk_bus(m: &Machine, target: String) -> FormSpec {
    let machine = m.clone();
    FormSpec::new(
        format!("Interface for {target}"),
        "Change",
        vec![F::choice("Interface", BUSES, "virtio")],
        move |v| {
            Ok(operation(
                machine.clone(),
                Op::DiskBus {
                    target: target.clone(),
                    bus: v[0].clone(),
                },
            ))
        },
    )
}
