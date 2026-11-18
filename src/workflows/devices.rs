use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceAction {
    NewDisk,
    AttachDisk,
    Cdrom,
    Network,
    PortForward,
    Share,
    Usb,
    Serial,
}
impl DeviceAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::NewDisk => "New disk",
            Self::AttachDisk => "Attach disk",
            Self::Cdrom => "CD/DVD drive",
            Self::Network => "Network card",
            Self::PortForward => "Port forwarding",
            Self::Share => "Shared folder",
            Self::Usb => "USB device",
            Self::Serial => "Serial port",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::NewDisk => "hard-drive",
            Self::AttachDisk => "hard-drive-download",
            Self::Cdrom => "disc-3",
            Self::Network => "ethernet-port",
            Self::PortForward => "arrow-right-left",
            Self::Share => "folder-plus",
            Self::Usb => "usb",
            Self::Serial => "square-terminal",
        }
    }
    pub fn requires_power_off(self) -> bool {
        self != Self::Usb
    }
}

pub fn device_form(action: DeviceAction, m: &Machine) -> Option<FormSpec> {
    let machine = m.clone();
    let label = action.label();
    Some(match action {
        DeviceAction::NewDisk => FormSpec::new(
            label,
            "Create disk",
            vec![
                F::text("Size (GB)", "32"),
                F::text("Target", "vdb"),
                F::choice("Interface", BUSES, "virtio"),
            ],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::NewDisk {
                        size_gib: number(v, 0, "disk size")?,
                        target: v[1].trim().into(),
                        bus: v[2].clone(),
                    },
                ))
            },
        ),
        DeviceAction::AttachDisk => FormSpec::new(
            label,
            "Attach",
            vec![
                F::path("Disk image", Picker::Disk),
                F::text("Target", "vdb"),
                F::choice("Interface", BUSES, "virtio"),
            ],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::AttachDisk {
                        path: v[0].clone().into(),
                        target: v[1].trim().into(),
                        bus: v[2].clone(),
                    },
                ))
            },
        ),
        DeviceAction::Network => FormSpec::new(
            label,
            "Add network",
            vec![
                F::choice(
                    "Mode",
                    &[
                        ("user", "Shared (user mode)"),
                        ("network", "Virtual network"),
                        ("bridge", "Host bridge"),
                    ],
                    "user",
                ),
                F::text("Network or bridge name", ""),
            ],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::Network {
                        mode: v[0].clone(),
                        source: v[1].trim().into(),
                    },
                ))
            },
        ),
        DeviceAction::Share => {
            // Windows reads only virtio-fs. Linux reads both; virtio-fs is faster when the
            // host has virtiofsd.
            let windows = crate::operations::GuestOs::of(m) == crate::operations::GuestOs::Windows;
            let virtiofs = crate::operations::ShareDriver::virtiofs_available();
            let default = if virtiofs || windows {
                "virtiofs"
            } else {
                "9p"
            };
            let mut fields = vec![
                F::path("Folder", Picker::Folder),
                F::choice("Access", &[("rw", "Read and write"), ("ro", "Read only")], "rw"),
                F::text("Mount tag", "share").advanced(),
            ];
            if !windows {
                fields.push(
                    F::choice("Driver", &[("virtiofs", "virtio-fs"), ("9p", "9p")], default)
                        .advanced(),
                );
            }
            FormSpec::new(label, "Share", fields, move |v| {
                let driver = if v.get(3).map_or(default, String::as_str) == "9p" {
                    crate::operations::ShareDriver::NineP
                } else {
                    crate::operations::ShareDriver::Virtiofs
                };
                Ok(operation(
                    machine.clone(),
                    Op::Share {
                        path: v[0].clone().into(),
                        tag: name(&v[2])?,
                        readonly: v[1] == "ro",
                        driver,
                    },
                ))
            })
            .derive(|v| vec![(2, mount_tag(&v[0]))])
        }
        DeviceAction::Usb => {
            let devices = crate::operations::host_usb();
            if devices.is_empty() {
                FormSpec::new("USB device", "Connect", vec![], |_| {
                    bail!("No USB devices are available.")
                })
                .note("No USB devices available.")
                .unavailable()
            } else {
                let options: Vec<_> =
                    devices.iter().map(|(value, label)| (value.as_str(), label.as_str())).collect();
                let selected = options[0].0;
                FormSpec::new(
                    "USB device",
                    "Apply",
                    vec![
                        F::choice("Device", &options, selected),
                        F::choice(
                            "Action",
                            &[("attach", "Connect"), ("detach", "Disconnect")],
                            "attach",
                        ),
                    ],
                    move |v| {
                        let (bus, device) = v[0]
                            .split_once(':')
                            .ok_or_else(|| anyhow::anyhow!("Choose a USB device."))?;
                        Ok(operation(
                            machine.clone(),
                            if v[1] == "detach" {
                                Op::UsbDetach {
                                    bus: bus.into(),
                                    device: device.into(),
                                }
                            } else {
                                Op::UsbAttach {
                                    bus: bus.into(),
                                    device: device.into(),
                                }
                            },
                        ))
                    },
                )
            }
        }
        DeviceAction::Cdrom => FormSpec::new(
            label,
            "Add drive",
            vec![
                F::text("Target", "sdb"),
                F::choice(
                    "Interface",
                    &[("sata", "SATA"), ("scsi", "SCSI"), ("ide", "IDE"), ("usb", "USB")],
                    "sata",
                ),
            ],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::AddCdrom {
                        target: v[0].trim().into(),
                        bus: v[1].clone(),
                    },
                ))
            },
        ),
        DeviceAction::PortForward => return Some(port_forward(m)),
        DeviceAction::Serial => FormSpec::new(
            label,
            "Add port",
            vec![
                F::choice("Type", &[("pty", "Pseudo-terminal"), ("tcp", "TCP server")], "pty"),
                F::text("TCP port", "4555"),
            ],
            move |v| {
                let port = if v[0] == "tcp" {
                    number(v, 1, "TCP port")?
                } else {
                    0
                };
                Ok(operation(
                    machine.clone(),
                    Op::AddSerial {
                        kind: v[0].clone(),
                        port,
                    },
                ))
            },
        )
        .visible(|i, v| i != 1 || v[0] == "tcp"),
    })
}

pub fn port_forward(m: &Machine) -> FormSpec {
    let machine = m.clone();
    FormSpec::new(
        "Port forwarding",
        "Add rule",
        vec![
            F::choice("Protocol", &[("tcp", "TCP"), ("udp", "UDP")], "tcp"),
            F::text("Host port", "2222"),
            F::text("Guest port", "22"),
        ],
        move |v| {
            Ok(operation(
                machine.clone(),
                Op::PortForward {
                    protocol: v[0].clone(),
                    host: number(v, 1, "host port")?,
                    guest: number(v, 2, "guest port")?,
                },
            ))
        },
    )
}

/// Guest mount tag from a folder path: its name, limited to characters every guest accepts.
pub fn mount_tag(path: &str) -> String {
    let base = std::path::Path::new(path.trim())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tag: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(31)
        .collect();
    if tag.trim_matches('_').is_empty() {
        "share".into()
    } else {
        tag
    }
}

/// Edits a shared folder: the old share is removed and the edited one added in one save.
pub fn edit_share(m: &Machine, device: &crate::operations::Device) -> FormSpec {
    use crate::operations::ShareDriver;
    let machine = m.clone();
    let old = device.xml.clone();
    let driver = ShareDriver::from_xml(&device.xml);
    let readonly = device.xml.contains("<readonly");
    let mut folder = F::path("Folder", Picker::Folder);
    folder.value = device.source.clone();
    FormSpec::new(
        "Edit shared folder",
        "Save",
        vec![
            folder,
            F::choice(
                "Access",
                &[("rw", "Read and write"), ("ro", "Read only")],
                if readonly {
                    "ro"
                } else {
                    "rw"
                },
            ),
            F::text("Mount tag", device.target.clone()),
            F::choice(
                "Driver",
                &[("virtiofs", "virtio-fs"), ("9p", "9p")],
                if driver == ShareDriver::NineP {
                    "9p"
                } else {
                    "virtiofs"
                },
            ),
        ],
        move |v| {
            Ok(operation(
                machine.clone(),
                Op::Sequence(vec![
                    Op::DetachDevice(old.clone()),
                    Op::Share {
                        path: v[0].trim().into(),
                        tag: name(&v[2])?,
                        readonly: v[1] == "ro",
                        driver: if v[3] == "9p" {
                            ShareDriver::NineP
                        } else {
                            ShareDriver::Virtiofs
                        },
                    },
                ]),
            ))
        },
    )
}
