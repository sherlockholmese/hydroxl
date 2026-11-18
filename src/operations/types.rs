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
}

impl Drop for Details {
    fn drop(&mut self) {
        if let Some(path) = &self.preview {
            let _ = std::fs::remove_file(path);
        }
    }
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
    DiskBus {
        target: String,
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
    System {
        cpu: Option<String>,
        machine_type: String,
        boot: String,
    },
    Display {
        model: String,
        heads: u32,
        accel3d: bool,
        sound: String,
        clipboard: bool,
        /// Video memory in MiB; 0 keeps the model default.
        vram: u32,
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
            addresses: if machine.state == crate::backend::State::Running {
                vec!["192.168.122.47".into()]
            } else {
                Vec::new()
            },
        }
    }
}
