use super::*;

pub const CPU_MODELS: &[(&str, &str)] = &[
    ("default", "QEMU default"),
    ("host-model", "Host model"),
    ("host-passthrough", "Host passthrough"),
    ("qemu64", "Generic x86-64"),
    ("max", "Maximum emulated features"),
];
pub const GRAPHICS: &[(&str, &str)] = &[
    ("virtio", "VirtIO GPU"),
    ("qxl", "QXL"),
    ("vga", "VGA"),
    ("bochs", "Bochs"),
    ("ramfb", "RAM framebuffer"),
];
pub const SOUND: &[(&str, &str)] = &[
    ("none", "None"),
    ("ich9", "Intel HD Audio"),
    ("ac97", "AC97"),
    ("es1370", "Ensoniq AudioPCI"),
    ("sb16", "Sound Blaster 16"),
];
/// Visible label for a registry value. Unknown values are real QEMU names, shown as-is.
pub fn label<'a>(registry: &'a [(&'a str, &'a str)], value: &'a str) -> &'a str {
    registry.iter().find(|(v, _)| *v == value).map_or(value, |(_, l)| l)
}
/// Editable settings read from domain XML. One parse feeds both the Hardware tab and its forms.
pub struct Config {
    pub cpu_model: String,
    pub machine_type: String,
    pub uefi: bool,
    pub graphics: String,
    pub heads: String,
    pub accel3d: bool,
    pub sound: String,
    pub clipboard: bool,
}
impl Config {
    pub fn parse(xml: &str) -> Option<Self> {
        let doc = roxmltree::Document::parse(xml).ok()?;
        let find = |tag: &str| doc.descendants().find(|n| n.has_tag_name(tag));
        let cpu = find("cpu");
        let cpu_model = match cpu.and_then(|n| n.attribute("mode")).unwrap_or("default") {
            "custom" => cpu
                .and_then(|n| n.children().find(|c| c.has_tag_name("model")))
                .and_then(|n| n.text())
                .unwrap_or("default"),
            mode => mode,
        };
        let video = find("video").and_then(|n| n.children().find(|c| c.has_tag_name("model")));
        Some(Self {
            cpu_model: cpu_model.into(),
            machine_type: find("type").and_then(|n| n.attribute("machine")).unwrap_or("").into(),
            uefi: doc.descendants().any(|n| {
                n.has_tag_name("loader")
                    || (n.has_tag_name("os") && n.attribute("firmware") == Some("efi"))
            }),
            graphics: video.and_then(|n| n.attribute("type")).unwrap_or("virtio").into(),
            heads: video.and_then(|n| n.attribute("heads")).unwrap_or("1").into(),
            accel3d: doc
                .descendants()
                .any(|n| n.has_tag_name("acceleration") && n.attribute("accel3d") == Some("yes")),
            sound: find("sound").and_then(|n| n.attribute("model")).unwrap_or("none").into(),
            clipboard: !doc
                .descendants()
                .any(|n| n.has_tag_name("clipboard") && n.attribute("copypaste") == Some("no")),
        })
    }
}

pub fn xml(m: &Machine, xml: String) -> FormSpec {
    let machine = m.clone();
    FormSpec::new("Machine configuration XML", "Save", vec![F::xml(xml)], move |v| {
        roxmltree::Document::parse(&v[0])
            .map_err(|_| anyhow::anyhow!("Fix the XML before saving."))?;
        Ok(operation(machine.clone(), Op::SaveXml(v[0].clone())))
    })
    .note("Save updates the configuration in libvirt. The machine.xml beside the disks records the original setup.")
}

/// One device in the boot order list.
#[derive(Clone, Debug, PartialEq)]
pub struct BootEntry {
    /// `disk:<target>` or `net:<mac>`, as the backend expects.
    pub key: String,
    pub label: String,
    pub detail: String,
    pub enabled: bool,
}

/// Bootable devices in boot order: enabled first, then the rest. Per-device `<boot order>`
/// wins; otherwise the global `<os><boot dev>` sequence maps onto device kinds.
pub fn boot_entries(xml: &str) -> Vec<BootEntry> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let file_name = |path: &str| {
        std::path::Path::new(path)
            .file_name()
            .map_or(path.to_owned(), |n| n.to_string_lossy().into_owned())
    };
    // (kind, entry, explicit order)
    let mut devices: Vec<(&str, BootEntry, Option<u32>)> = Vec::new();
    for n in doc.descendants().filter(|n| n.has_tag_name("disk")) {
        let Some(target) =
            n.children().find(|c| c.has_tag_name("target")).and_then(|c| c.attribute("dev"))
        else {
            continue;
        };
        let cdrom = n.attribute("device") == Some("cdrom");
        let source = n
            .children()
            .find(|c| c.has_tag_name("source"))
            .and_then(|c| c.attribute("file").or_else(|| c.attribute("dev")));
        let order = n
            .children()
            .find(|c| c.has_tag_name("boot"))
            .and_then(|c| c.attribute("order"))
            .and_then(|o| o.parse().ok());
        devices.push((
            if cdrom {
                "cdrom"
            } else {
                "hd"
            },
            BootEntry {
                key: format!("disk:{target}"),
                label: format!(
                    "{} {target}",
                    if cdrom {
                        "CD/DVD"
                    } else {
                        "Disk"
                    }
                ),
                detail: source.map_or_else(|| "Empty".into(), file_name),
                enabled: false,
            },
            order,
        ));
    }
    for n in doc.descendants().filter(|n| n.has_tag_name("interface")) {
        let Some(mac) =
            n.children().find(|c| c.has_tag_name("mac")).and_then(|c| c.attribute("address"))
        else {
            continue;
        };
        let order = n
            .children()
            .find(|c| c.has_tag_name("boot"))
            .and_then(|c| c.attribute("order"))
            .and_then(|o| o.parse().ok());
        devices.push((
            "network",
            BootEntry {
                key: format!("net:{mac}"),
                label: "Network".into(),
                detail: mac.into(),
                enabled: false,
            },
            order,
        ));
    }
    let mut ordered: Vec<BootEntry> = Vec::new();
    if devices.iter().any(|d| d.2.is_some()) {
        let mut explicit: Vec<_> = devices.iter().filter(|d| d.2.is_some()).collect();
        explicit.sort_by_key(|d| d.2);
        ordered.extend(explicit.into_iter().map(|d| BootEntry {
            enabled: true,
            ..d.1.clone()
        }));
    } else {
        let sequence: Vec<&str> = doc
            .descendants()
            .filter(|n| n.has_tag_name("boot") && n.parent().is_some_and(|p| p.has_tag_name("os")))
            .filter_map(|n| n.attribute("dev"))
            .collect();
        let sequence = if sequence.is_empty() {
            vec!["hd"]
        } else {
            sequence
        };
        for kind in sequence {
            ordered.extend(devices.iter().filter(|d| d.0 == kind).map(|d| BootEntry {
                enabled: true,
                ..d.1.clone()
            }));
        }
    }
    for d in &devices {
        if !ordered.iter().any(|e| e.key == d.1.key) {
            ordered.push(d.1.clone());
        }
    }
    ordered
}

/// Editable hardware values. The Hardware tab edits a copy; Save turns the differences
/// into one ordered sequence of operations.
#[derive(Clone, Debug, PartialEq)]
pub struct HardwareDraft {
    pub id: String,
    pub topology: crate::backend::Topology,
    pub implicit_topology: bool,
    pub memory_mib: u64,
    pub uefi: bool,
    pub cpu_model: String,
    pub nested_virtualization: bool,
    pub machine_type: String,
    pub graphics: String,
    pub heads: u32,
    pub accel3d: bool,
    pub local_dbus: bool,
    pub sound: String,
    pub clipboard: bool,
    /// Video memory in MiB; 0 is the model default.
    pub vram: u32,
    pub platform: crate::operations::Platform,
    pub nics: Vec<Nic>,
    pub boot: Vec<BootEntry>,
}

/// A network card as edited in the Hardware tab.
#[derive(Clone, Debug, PartialEq)]
pub struct Nic {
    /// Address in the saved configuration; identifies the card.
    pub mac: String,
    pub new_mac: String,
    /// libvirt interface type: user, network or bridge.
    pub kind: String,
    /// Network or bridge name; empty in user mode.
    pub source: String,
    pub model: String,
    pub isolated: bool,
}

pub const NIC_MODELS: &[(&str, &str)] = &[
    ("virtio", "VirtIO"),
    ("e1000e", "Intel e1000e"),
    ("e1000", "Intel e1000"),
    ("rtl8139", "Realtek RTL8139"),
];
pub const USB_CONTROLLERS: &[(&str, &str)] =
    &[("qemu-xhci", "USB 3.0"), ("ich9-ehci1", "USB 2.0"), ("none", "None")];
pub fn machine_types(arch: &str) -> &'static [(&'static str, &'static str)] {
    match arch {
        "x86_64" | "i686" => &[("q35", "Q35"), ("pc", "i440FX")],
        "aarch64" | "armv7l" | "riscv64" => &[("virt", "virt")],
        "ppc64" | "ppc64le" => &[("pseries", "pSeries")],
        "s390x" => &[("s390-ccw-virtio", "s390-ccw-virtio")],
        _ => &[],
    }
}

pub fn machine_type_family(kind: &str) -> &str {
    for (prefix, family) in [
        ("pc-q35-", "q35"),
        ("pc-i440fx-", "pc"),
        ("virt-", "virt"),
        ("pseries-", "pseries"),
        ("s390-ccw-virtio-", "s390-ccw-virtio"),
    ] {
        if kind
            .strip_prefix(prefix)
            .is_some_and(|version| version.as_bytes().first().is_some_and(u8::is_ascii_digit))
        {
            return family;
        }
    }
    kind
}
pub const VIDEO_MEMORY: &[(&str, &str)] = &[
    ("0", "Default"),
    ("16", "16 MB"),
    ("32", "32 MB"),
    ("64", "64 MB"),
    ("128", "128 MB"),
    ("256", "256 MB"),
];

fn platform(doc: &roxmltree::Document) -> crate::operations::Platform {
    let has = |pick: &dyn Fn(roxmltree::Node) -> bool| doc.descendants().any(|n| pick(n));
    crate::operations::Platform {
        tpm: has(&|n| n.has_tag_name("tpm")),
        tablet: has(&|n| n.has_tag_name("input") && n.attribute("type") == Some("tablet")),
        usb: doc
            .descendants()
            .find(|n| n.has_tag_name("controller") && n.attribute("type") == Some("usb"))
            .map(|n| match n.attribute("model") {
                Some("qemu-xhci" | "nec-xhci") => "qemu-xhci",
                Some("none") => "none",
                _ => "ich9-ehci1",
            })
            .unwrap_or("none")
            .into(),
        balloon: !has(&|n| n.has_tag_name("memballoon") && n.attribute("model") == Some("none"))
            && has(&|n| n.has_tag_name("memballoon")),
        rng: has(&|n| n.has_tag_name("rng")),
        hyperv: has(&|n| n.has_tag_name("hyperv")),
        localtime: has(&|n| n.has_tag_name("clock") && n.attribute("offset") == Some("localtime")),
    }
}

fn nics(doc: &roxmltree::Document) -> Vec<Nic> {
    doc.descendants()
        .filter(|n| n.has_tag_name("interface"))
        .filter_map(|n| {
            let mac = n.children().find(|c| c.has_tag_name("mac"))?.attribute("address")?;
            let kind = n.attribute("type").unwrap_or("user");
            let source = n
                .children()
                .find(|c| c.has_tag_name("source"))
                .and_then(|c| c.attribute("network").or_else(|| c.attribute("bridge")));
            Some(Nic {
                mac: mac.into(),
                new_mac: mac.into(),
                kind: kind.into(),
                source: if kind == "user" {
                    String::new()
                } else {
                    source.unwrap_or("").into()
                },
                model: n
                    .children()
                    .find(|c| c.has_tag_name("model"))
                    .and_then(|c| c.attribute("type"))
                    .unwrap_or("virtio")
                    .into(),
                isolated: n
                    .children()
                    .any(|c| c.has_tag_name("port") && c.attribute("isolated") == Some("yes")),
            })
        })
        .collect()
}
impl HardwareDraft {
    pub fn read(machine: &Machine, xml: &str) -> Option<Self> {
        let c = Config::parse(xml)?;
        let doc = roxmltree::Document::parse(xml).ok()?;
        let vram = doc
            .descendants()
            .find(|n| {
                n.has_tag_name("model") && n.parent().is_some_and(|p| p.has_tag_name("video"))
            })
            .and_then(|n| n.attribute("vram"))
            .and_then(|v| v.parse::<u32>().ok())
            .map_or(0, |kib| kib / 1024);
        let topology = topology(&doc);
        let local_dbus = doc.descendants().any(crate::backend::is_peer_dbus);
        Some(Self {
            id: machine.id.clone(),
            topology: topology.unwrap_or(crate::backend::Topology {
                sockets: machine.cpus.max(1),
                cores: 1,
                threads: 1,
            }),
            implicit_topology: topology.is_none(),
            memory_mib: machine.memory_mib,
            uefi: c.uefi,
            cpu_model: c.cpu_model,
            nested_virtualization: doc.descendants().any(|n| {
                n.has_tag_name("feature")
                    && n.parent().is_some_and(|p| p.has_tag_name("cpu"))
                    && matches!(n.attribute("name"), Some("vmx" | "svm"))
                    && matches!(n.attribute("policy"), Some("require" | "force"))
            }),
            machine_type: c.machine_type,
            graphics: c.graphics,
            heads: c.heads.parse().unwrap_or(1),
            accel3d: c.accel3d,
            local_dbus,
            sound: c.sound,
            clipboard: if local_dbus {
                crate::backend::peer_clipboard(&doc)
            } else {
                c.clipboard
            },
            vram,
            platform: platform(&doc),
            nics: nics(&doc),
            boot: boot_entries(xml),
        })
    }

    /// Operations that turn `original` into this draft, in a safe order.
    pub fn changes(&self, original: &Self) -> Vec<Op> {
        let mut ops = Vec::new();
        let changed_topology = self.topology != original.topology
            || (original.implicit_topology && !self.implicit_topology);
        if changed_topology || self.memory_mib != original.memory_mib {
            ops.push(Op::Resources {
                topology: changed_topology.then_some(self.topology),
                memory_mib: self.memory_mib,
            });
        }
        if self.cpu_model != original.cpu_model || self.machine_type != original.machine_type {
            ops.push(Op::System {
                cpu: (self.cpu_model != original.cpu_model).then(|| self.cpu_model.clone()),
                machine_type: self.machine_type.clone(),
                boot: String::new(),
            });
        }
        if (self.nested_virtualization != original.nested_virtualization
            || self.cpu_model != original.cpu_model)
            && self.cpu_model == "host-passthrough"
        {
            ops.push(Op::NestedVirtualization(self.nested_virtualization));
        }
        if self.uefi != original.uefi {
            ops.push(Op::Firmware(self.uefi));
        }
        let settings = self.display_settings();
        let local_dbus = (self.local_dbus != original.local_dbus).then_some(self.local_dbus);
        let settings = (settings != original.display_settings()).then_some(settings);
        if settings.is_some() || local_dbus.is_some() {
            ops.push(Op::Display {
                settings,
                local_dbus,
            });
        }
        if self.platform != original.platform {
            ops.push(Op::Platform(self.platform.clone()));
        }
        for (nic, before) in self.nics.iter().zip(&original.nics) {
            if nic != before {
                ops.push(Op::Nic(crate::operations::NicChange {
                    mac: before.mac.clone(),
                    kind: nic.kind.clone(),
                    source: nic.source.clone(),
                    model: nic.model.clone(),
                    new_mac: nic.new_mac.clone(),
                    isolated: nic.isolated,
                }));
            }
        }
        if self.boot != original.boot {
            ops.push(Op::BootOrder(
                self.boot.iter().filter(|e| e.enabled).map(|e| e.key.clone()).collect(),
            ));
        }
        ops
    }

    fn display_settings(&self) -> crate::operations::DisplaySettings {
        crate::operations::DisplaySettings {
            model: self.graphics.clone(),
            heads: self.heads,
            accel3d: self.accel3d,
            sound: self.sound.clone(),
            clipboard: self.clipboard,
            vram: self.vram,
        }
    }
}

fn topology(doc: &roxmltree::Document) -> Option<crate::backend::Topology> {
    let cpu = doc.root_element().children().find(|n| n.has_tag_name("cpu"))?;
    let node = cpu.children().find(|n| n.has_tag_name("topology"))?;
    let get = |name: &str| node.attribute(name).and_then(|v| v.parse().ok()).unwrap_or(1u32).max(1);
    Some(crate::backend::Topology {
        sockets: get("sockets").saturating_mul(get("dies")),
        cores: get("cores"),
        threads: get("threads"),
    })
}
