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

pub fn edit_drive(m: &Machine, device: &crate::operations::Device) -> FormSpec {
    let machine = m.clone();
    let target = device.target.clone();
    let bus = roxmltree::Document::parse(&device.xml)
        .ok()
        .and_then(|doc| {
            doc.root_element()
                .children()
                .find(|n| n.has_tag_name("target"))
                .and_then(|n| n.attribute("bus"))
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let buses = if device.kind == "cdrom" {
        &BUSES[1..]
    } else {
        BUSES
    };
    let old_target = target.clone();
    let old_bus = bus.clone();
    FormSpec::new(
        format!(
            "Edit {} {target}",
            if device.kind == "cdrom" {
                "CD/DVD"
            } else {
                "disk"
            }
        ),
        "Save",
        vec![F::choice("Bus", buses, &bus).row(1), F::text("Target", &target).row(1)],
        move |v| {
            let new_target = v[1].trim();
            if v[0] != old_bus || new_target != old_target {
                crate::operations::validate_disk_target(&v[0], new_target)?;
            }
            Ok(operation(
                machine.clone(),
                Op::DiskInterface {
                    target: old_target.clone(),
                    new_target: new_target.into(),
                    bus: v[0].clone(),
                },
            ))
        },
    )
    .derive(move |v| {
        if v[0] == bus {
            return vec![(1, target.clone())];
        }
        let Some(prefix) = crate::operations::target_prefix(&v[0]) else {
            return Vec::new();
        };
        let suffix = crate::operations::target_prefix(&bus)
            .and_then(|old_prefix| target.strip_prefix(old_prefix))
            .unwrap_or("a");
        vec![(1, format!("{prefix}{suffix}"))]
    })
}
