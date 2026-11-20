use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub name: String,
    pub label: String,
    pub state: String,
    pub parent: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Device {
    pub label: String,
    pub kind: String,
    pub target: String,
    pub source: String,
    pub xml: String,
}
#[derive(Debug, Default)]
pub struct Details {
    pub snapshots: Vec<Snapshot>,
    pub devices: Vec<Device>,
    pub xml: String,
    pub snapshot_error: Option<String>,
    pub preview: Option<PathBuf>,
    /// libvirt virtual networks and host bridges a network card can attach to.
    pub networks: Vec<String>,
    pub bridges: Vec<String>,
    /// Virtual size and space used on the host, in bytes, per disk target.
    pub disk_sizes: Vec<(String, u64, u64)>,
    /// Guest IP addresses, when the guest is running and reports them.
    pub addresses: Vec<String>,
    pub neptune_ready: bool,
}

impl Drop for Details {
    fn drop(&mut self) {
        if let Some(path) = &self.preview {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySettings {
    pub model: String,
    pub heads: u32,
    pub accel3d: bool,
    pub sound: String,
    pub clipboard: bool,
    /// Video memory in MiB; 0 keeps the model default.
    pub vram: u32,
}

#[derive(Clone, Debug)]
pub enum Operation {
    /// Runs each step in order and stops at the first failure.
    Sequence(Vec<Operation>),
    /// Memory in MiB and an optional replacement CPU topology.
    Resources {
        topology: Option<crate::backend::Topology>,
        memory_mib: u64,
    },
    /// Per-device boot order, see `boot::apply_boot_order`.
    BootOrder(Vec<String>),
    Platform(super::platform::Platform),
    Nic(super::platform::NicChange),
    DiskInterface {
        target: String,
        new_target: String,
        bus: String,
    },
    AddSerial {
        kind: String,
        port: u16,
    },
    ShowFiles,
    /// Hard reset, like pressing the machine's reset button.
    Reset,
    OpenLog,
    Notes(String),
    Firmware(bool),
    NestedVirtualization(bool),
    CloneConfiguration(String),
    MoveStorage(PathBuf),
    CompactDisk(String),
    AddCdrom {
        target: String,
        bus: String,
    },
    GuestTools(Option<String>),
    TritonDriver,
    SwitchToBundledQemu,
    System {
        cpu: Option<String>,
        machine_type: String,
        boot: String,
    },
    Display {
        settings: Option<DisplaySettings>,
        local_dbus: Option<bool>,
    },
    PortForward {
        protocol: String,
        host: u16,
        guest: u16,
    },
    DetachDevice(String),
    RemovePortForward(String),
    Reboot,
    ForceStop,
    SaveState,
    DiscardState,
    Disposable,
    Autostart(bool),
    Rename(String),
    Clone(String),
    Export(PathBuf),
    ExportOva(PathBuf),
    Remove {
        delete_disks: bool,
    },
    SnapshotCreate(String),
    SnapshotRestore(String),
    SnapshotDelete(String),
    SnapshotRename {
        old: String,
        new: String,
    },
    SnapshotOverwrite(String),
    AttachDisk {
        path: PathBuf,
        target: String,
        bus: String,
    },
    NewDisk {
        size_gib: u64,
        target: String,
        bus: String,
    },
    DetachDisk(String),
    MountIso {
        path: PathBuf,
        target: String,
    },
    EjectIso(String),
    ResizeDisk {
        target: String,
        size_gib: u64,
    },
    Network {
        mode: String,
        source: String,
    },
    Share {
        path: PathBuf,
        tag: String,
        readonly: bool,
        driver: super::ShareDriver,
    },
    /// Retargets a stopped machine at another CPU architecture.
    Architecture(String),
    /// libosinfo URI of the guest operating system; empty clears it.
    OperatingSystem(String),
    /// Work inside a running guest: mount shared folders or type text.
    Guest(super::GuestSetup),
    /// Adds the virtio channel the QEMU guest agent talks over.
    AddAgentChannel,
    UsbAttach {
        bus: String,
        device: String,
    },
    UsbDetach {
        bus: String,
        device: String,
    },
    SaveXml(String),
    Console,
    Screenshot(PathBuf),
    /// Guest memory written with `virsh dump --memory-only`.
    MemoryDump {
        path: PathBuf,
        format: String,
    },
}

impl Details {
    /// Sample details for `--demo`. Never shown for a live connection.
    pub fn demo(machine: &crate::backend::Machine) -> Self {
        let id = &machine.id;
        let windows = id == "windows";
        let xml = format!(
            "<domain type='kvm'><name>{id}</name><os{firmware}><type machine='pc-q35-9.1'>hvm</type><boot dev='hd'/><boot dev='cdrom'/></os>\
             <cpu mode='host-passthrough'><topology sockets='1' cores='{cpus}' threads='1'/></cpu><devices>\
             <disk type='file' device='disk'><source file='/home/user/.local/share/hydroxl/machines/{id}/disk.qcow2'/><target dev='vda' bus='virtio'/></disk>\
             <disk type='file' device='cdrom'>{iso}<target dev='sda' bus='sata'/></disk>\
             <interface type='user'><mac address='52:54:00:3a:9e:41'/></interface>\
             <video><model type='virtio' heads='1'/></video>\
             <sound model='ich9'/><graphics type='spice'><clipboard copypaste='yes'/></graphics></devices></domain>",
            cpus = machine.cpus,
            firmware = if windows {
                " firmware='efi'"
            } else {
                ""
            },
            iso = if id == "ubuntu" {
                "<source file='/home/user/Downloads/ubuntu-24.04.1-desktop-amd64.iso'/>"
            } else {
                ""
            },
        );
        let disk = |target: &str, source: String| Device {
            label: "Disk".into(),
            kind: "disk".into(),
            target: target.into(),
            source,
            xml: String::new(),
        };
        let mut devices = vec![
            disk("vda", format!("~/.local/share/hydroxl/machines/{id}/disk.qcow2")),
            Device {
                label: "CD/DVD".into(),
                kind: "cdrom".into(),
                target: "sda".into(),
                source: if id == "ubuntu" {
                    "~/Downloads/ubuntu-24.04.1-desktop-amd64.iso".into()
                } else {
                    "Empty".into()
                },
                xml: String::new(),
            },
            Device {
                label: "Network".into(),
                kind: "network".into(),
                target: "52:54:00:3a:9e:41".into(),
                source: "Shared network".into(),
                xml: String::new(),
            },
        ];
        if id == "fedora" || id == "ubuntu" {
            devices.push(Device {
                label: "Shared folder".into(),
                kind: "share".into(),
                target: "Projects".into(),
                source: "~/Projects".into(),
                xml: String::new(),
            });
        }
        let snapshots = if id == "ubuntu" {
            vec![
                Snapshot {
                    name: "s1".into(),
                    label: "Fresh install".into(),
                    state: "Powered off".into(),
                    parent: None,
                },
                Snapshot {
                    name: "s2".into(),
                    label: "Before kernel upgrade".into(),
                    state: "Powered off".into(),
                    parent: Some("Fresh install".into()),
                },
            ]
        } else {
            Vec::new()
        };
        Self {
            snapshots,
            devices,
            xml,
            snapshot_error: None,
            preview: None,
            networks: vec!["default".into()],
            bridges: vec!["br0".into()],
            disk_sizes: vec![("vda".into(), 64 << 30, 18_700_000_000)],
            neptune_ready: windows,
            addresses: if machine.state == crate::backend::State::Running {
                vec!["192.168.122.47".into()]
            } else {
                Vec::new()
            },
        }
    }
}

impl Operation {
    /// What happened, said once the operation succeeds. Names the thing that changed so a
    /// notice never reads as a generic "saved".
    pub(crate) fn done(&self) -> String {
        use super::GuestSetup;
        let file = |p: &PathBuf| {
            p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        };
        let on = |b: bool| {
            if b {
                "on"
            } else {
                "off"
            }
        };
        match self {
            Self::Sequence(steps) => match steps.as_slice() {
                [one] => one.done(),
                _ => "Hardware saved".into(),
            },
            Self::Resources {
                ..
            } => "Processor and memory saved".into(),
            Self::BootOrder(_) => "Boot order saved".into(),
            Self::Platform(_) => "Platform settings saved".into(),
            Self::Nic(_) => "Network adapter saved".into(),
            Self::DiskInterface {
                new_target,
                ..
            } => format!("Drive saved as {new_target}"),
            Self::AddSerial {
                ..
            } => "Serial port added".into(),
            Self::ShowFiles => "Files shown".into(),
            Self::Reset => "Machine reset".into(),
            Self::OpenLog => "Log opened".into(),
            Self::Notes(_) => "Notes saved".into(),
            Self::Firmware(uefi) => if *uefi {
                "Firmware set to UEFI"
            } else {
                "Firmware set to BIOS"
            }
            .into(),
            Self::NestedVirtualization(enabled) => {
                format!("Nested virtualization turned {}", on(*enabled))
            }
            Self::CloneConfiguration(name) => format!("Configuration copied to {name}"),
            Self::MoveStorage(path) => format!("Storage moved to {}", path.display()),
            Self::CompactDisk(target) => format!("{target} compacted"),
            Self::AddCdrom {
                ..
            } => "CD drive added".into(),
            Self::GuestTools(_) => "Guest tools inserted".into(),
            Self::TritonDriver => "Triton driver disc mounted".into(),
            Self::SwitchToBundledQemu => "Switched to bundled Neptune QEMU".into(),
            Self::System {
                ..
            } => "System settings saved".into(),
            Self::Display {
                ..
            } => "Display and sound saved".into(),
            Self::PortForward {
                protocol,
                host,
                guest,
            } => {
                format!("Forwarding {} {host} to {guest}", protocol.to_uppercase())
            }
            Self::DetachDevice(_) => "Device removed".into(),
            Self::RemovePortForward(_) => "Port forward removed".into(),
            Self::Reboot => "Machine restarted".into(),
            Self::ForceStop => "Machine forced off".into(),
            Self::SaveState => "Machine state saved".into(),
            Self::DiscardState => "Saved state discarded".into(),
            Self::Disposable => "Disposable copy started".into(),
            Self::Autostart(enabled) => format!("Start with host turned {}", on(*enabled)),
            Self::Rename(name) => format!("Renamed to {name}"),
            Self::Clone(name) => format!("Cloned as {name}"),
            Self::Export(path) | Self::ExportOva(path) => format!("Exported {}", file(path)),
            Self::Remove {
                delete_disks,
            } => if *delete_disks {
                "Machine and disks removed"
            } else {
                "Machine removed"
            }
            .into(),
            Self::SnapshotCreate(name) => format!("Snapshot {name} saved"),
            Self::SnapshotRestore(name) => format!("Restored {name}"),
            Self::SnapshotDelete(name) => format!("Snapshot {name} deleted"),
            Self::SnapshotRename {
                new,
                ..
            } => format!("Snapshot renamed to {new}"),
            Self::SnapshotOverwrite(name) => format!("Snapshot {name} replaced"),
            Self::AttachDisk {
                path,
                ..
            } => format!("{} attached", file(path)),
            Self::NewDisk {
                size_gib,
                ..
            } => format!("{size_gib} GB disk added"),
            Self::DetachDisk(target) => format!("{target} removed"),
            Self::MountIso {
                path,
                ..
            } => format!("{} inserted", file(path)),
            Self::EjectIso(_) => "Disc ejected".into(),
            Self::ResizeDisk {
                target,
                size_gib,
            } => format!("{target} resized to {size_gib} GB"),
            Self::Network {
                ..
            } => "Network saved".into(),
            Self::Share {
                tag,
                ..
            } => format!("Sharing {tag}"),
            Self::Architecture(arch) => format!("Architecture set to {arch}"),
            Self::OperatingSystem(uri) => {
                if uri.is_empty() {
                    "Operating system cleared".into()
                } else {
                    let name = crate::backend::os_name(uri).unwrap_or("the chosen system");
                    format!("Operating system set to {name}")
                }
            }
            Self::Guest(GuestSetup::Shares {
                ..
            }) => "Shared folders mounted".into(),
            Self::Guest(GuestSetup::Unmount {
                ..
            }) => "Shared folders unmounted".into(),
            Self::Guest(GuestSetup::Type(_)) => "Text typed".into(),
            Self::AddAgentChannel => "Guest agent channel added".into(),
            Self::UsbAttach {
                ..
            } => "USB device connected".into(),
            Self::UsbDetach {
                ..
            } => "USB device disconnected".into(),
            Self::SaveXml(_) => "Configuration saved".into(),
            Self::Console => "Console opened".into(),
            Self::Screenshot(path) => format!("Saved {}", file(path)),
            Self::MemoryDump {
                path,
                ..
            } => format!("Memory dump saved to {}", file(path)),
        }
    }
}
