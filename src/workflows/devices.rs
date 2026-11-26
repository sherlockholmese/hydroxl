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

pub fn device_form(
    action: DeviceAction,
    m: &Machine,
    connection: crate::backend::Connection,
) -> Option<FormSpec> {
    let machine = m.clone();
    let label = action.label();
    Some(match action {
        DeviceAction::NewDisk => FormSpec::new(
            label,
            "Create disk",
            vec![
                F::text("Size (GiB)", "32"),
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
        DeviceAction::Share => share_form(m, connection, None),
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
    let tag = tag.trim_start_matches('-');
    if tag.trim_matches('_').is_empty() {
        "share".into()
    } else {
        tag.to_owned()
    }
}

pub fn edit_share(
    m: &Machine,
    device: &crate::operations::Device,
    connection: crate::backend::Connection,
) -> FormSpec {
    share_form(m, connection, Some(device))
}

fn share_form(
    m: &Machine,
    connection: crate::backend::Connection,
    device: Option<&crate::operations::Device>,
) -> FormSpec {
    use crate::backend::Connection;
    use crate::operations::{GuestOs, ShareConfig, ShareDriver, ShareOwnership};
    const DRIVER: usize = 3;
    const OWNERSHIP: usize = 4;
    const UID: usize = 5;
    const GID: usize = 6;

    let machine = m.clone();
    let editing = device.is_some();
    let windows = GuestOs::of(m) == GuestOs::Windows;
    let can_map_ids = !windows && connection == Connection::Session;
    let driver = device.map_or_else(
        || {
            if windows || ShareDriver::virtiofs_available() {
                ShareDriver::Virtiofs
            } else {
                ShareDriver::NineP
            }
        },
        |device| ShareDriver::from_xml(&device.xml),
    );
    let show_driver = !windows || driver == ShareDriver::NineP;
    let folder = device.map(|device| device.source.clone()).unwrap_or_default();
    let replace = device.map(|device| device.target.clone());
    let readonly = device.is_some_and(|device| device.xml.contains("<readonly"));
    let existing_mapping = device.is_some_and(|device| {
        roxmltree::Document::parse(&device.xml)
            .is_ok_and(|doc| doc.root_element().children().any(|node| node.has_tag_name("idmap")))
    });
    let mut ownership_choices =
        vec![("account", "Linux guest account"), ("default", "Libvirt default")];
    if existing_mapping {
        ownership_choices.insert(0, ("keep", "Keep current mapping"));
    }
    let mut fields = vec![
        F {
            value: folder,
            ..F::path("Folder", Picker::Folder)
        },
        F::choice(
            "Access",
            &[("rw", "Read and write"), ("ro", "Read only")],
            if readonly {
                "ro"
            } else {
                "rw"
            },
        ),
        F::text("Mount tag", replace.as_deref().unwrap_or("share")),
        F::choice(
            "Driver",
            &[("virtiofs", "virtio-fs"), ("9p", "9p")],
            if driver == ShareDriver::NineP {
                "9p"
            } else {
                "virtiofs"
            },
        ),
        F::choice(
            "File ownership",
            &ownership_choices,
            if existing_mapping {
                "keep"
            } else {
                "account"
            },
        ),
        F::text("Guest UID", "1000").row(1),
        F::text("Guest GID", "1000").row(1),
    ];
    if !editing {
        for field in &mut fields[2..] {
            field.advanced = true;
        }
    }
    let form = FormSpec::new(
        if editing {
            "Edit shared folder"
        } else {
            "Shared folder"
        },
        if editing {
            "Save"
        } else {
            "Share"
        },
        fields,
        move |v| {
            let driver = if v[DRIVER] == "9p" {
                ShareDriver::NineP
            } else {
                ShareDriver::Virtiofs
            };
            let ownership = if driver == ShareDriver::Virtiofs && can_map_ids {
                match v[OWNERSHIP].as_str() {
                    "account" => ShareOwnership::Guest {
                        uid: number(v, UID, "guest UID")?,
                        gid: number(v, GID, "guest GID")?,
                    },
                    "keep" => ShareOwnership::Preserve,
                    _ => ShareOwnership::Default,
                }
            } else if editing {
                ShareOwnership::Preserve
            } else {
                ShareOwnership::Default
            };
            Ok(operation(
                machine.clone(),
                Op::Share(ShareConfig {
                    path: v[0].trim().into(),
                    tag: name(v, 2)?,
                    readonly: v[1] == "ro",
                    driver,
                    ownership,
                    replace: replace.clone(),
                }),
            ))
        },
    )
    .visible(move |index, v| match index {
        DRIVER => show_driver,
        OWNERSHIP => can_map_ids && v[DRIVER] == "virtiofs",
        UID | GID => can_map_ids && v[DRIVER] == "virtiofs" && v[OWNERSHIP] == "account",
        _ => true,
    });
    let form = if can_map_ids {
        form.note(
            "For a Linux guest account, use the UID and GID from id -u and id -g in the guest.",
        )
    } else {
        form
    };
    if editing {
        form
    } else {
        form.derive(|v| vec![(2, mount_tag(&v[0]))])
    }
}
