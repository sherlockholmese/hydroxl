use super::*;
#[derive(Clone, Copy, Debug)]
pub enum MachineAction {
    Restart,
    SaveState,
    DiscardState,
    Disposable,
    Console,
    Screenshot,
    MemoryDump,
    GuestTools,
    ShareFolder,
    ShowFiles,
    ViewLog,
    ChangeIcon,
    Notes,
    Rename,
    CloneConfiguration,
    MoveStorage,
    Clone,
    Export,
    ExportOva,
    Autostart,
    ForceStop,
    Reset,
    Remove,
    OperatingSystem,
}

#[derive(Clone, Copy)]
pub struct ActionEntry {
    pub action: MachineAction,
    pub label: &'static str,
    /// Lucide glyph shown beside the label, so a long menu scans by shape.
    pub icon: &'static str,
    /// Entries sharing a group render together; groups are separated in menus.
    pub group: u8,
    /// Destructive or state-discarding: rendered in the danger color.
    pub danger: bool,
}

/// Actions that apply to the machine in its current state, in menu order. Actions that
/// cannot apply are left out rather than shown disabled: the header already says why
/// (Running, Suspended), and a short menu reads faster than a long gray one.
/// Start, Pause and Shut down live in the header and are not repeated here.
pub fn machine_actions(m: &Machine) -> Vec<ActionEntry> {
    let stopped = m.state == State::Stopped;
    let running = matches!(m.state, State::Running | State::Paused);
    let editable = stopped && !m.saved_state;
    let autostart = if m.autostart {
        "Turn off autostart"
    } else {
        "Turn on autostart"
    };
    [
        // Power
        (MachineAction::Restart, "Restart", "rotate-ccw", 0, running, false),
        (MachineAction::SaveState, "Suspend to disk", "archive", 0, running, false),
        (MachineAction::Reset, "Reset…", "refresh-ccw-dot", 0, running, true),
        (MachineAction::ForceStop, "Force stop…", "power-off", 0, running, true),
        (MachineAction::Disposable, "Start disposable…", "flask-conical", 0, stopped, false),
        (
            MachineAction::DiscardState,
            "Discard suspended state…",
            "archive-x",
            0,
            stopped && m.saved_state,
            true,
        ),
        // Inside the guest
        (MachineAction::Console, "Serial console", "square-terminal", 1, running, false),
        (MachineAction::Screenshot, "Save screenshot…", "camera", 1, running, false),
        (MachineAction::MemoryDump, "Save memory dump…", "cpu", 1, running, false),
        (
            MachineAction::GuestTools,
            "Mount guest tools…",
            "package",
            1,
            m.state != State::Other,
            false,
        ),
        (MachineAction::ShareFolder, "Share a folder…", "folder-plus", 1, editable, false),
        // Identity
        (MachineAction::Rename, "Rename…", "pencil", 2, stopped, false),
        (MachineAction::OperatingSystem, "Operating system…", "monitor-cog", 2, true, false),
        (MachineAction::ChangeIcon, "Change icon…", "image", 2, true, false),
        (MachineAction::Notes, "Notes…", "notebook-pen", 2, stopped, false),
        (MachineAction::Autostart, autostart, "power", 2, true, false),
        // Copies
        (MachineAction::Clone, "Clone…", "copy", 3, stopped, false),
        (
            MachineAction::CloneConfiguration,
            "Clone configuration…",
            "copy-plus",
            3,
            editable,
            false,
        ),
        (MachineAction::MoveStorage, "Move storage…", "folder-input", 3, editable, false),
        (MachineAction::Export, "Export folder…", "folder-output", 3, stopped, false),
        (MachineAction::ExportOva, "Export OVA…", "package-open", 3, editable, false),
        // Files
        (MachineAction::ShowFiles, "Show files", "folder-open", 4, m.disk.is_some(), false),
        (MachineAction::ViewLog, "View log", "scroll-text", 4, true, false),
        (MachineAction::Remove, "Remove from library…", "trash", 5, stopped, true),
    ]
    .into_iter()
    .filter(|entry| entry.4)
    .map(|(action, label, icon, group, _, danger)| ActionEntry {
        action,
        label,
        icon,
        group,
        danger,
    })
    .collect()
}
pub fn direct_action(action: MachineAction, m: &Machine) -> Option<Op> {
    Some(match action {
        MachineAction::Restart => Op::Reboot,
        MachineAction::SaveState => Op::SaveState,
        MachineAction::Console => Op::Console,
        MachineAction::Autostart => Op::Autostart(!m.autostart),
        MachineAction::ShowFiles => Op::ShowFiles,
        MachineAction::ViewLog => Op::OpenLog,
        _ => return None,
    })
}
pub fn action_form(action: MachineAction, m: &Machine) -> Option<FormSpec> {
    let machine = m.clone();
    Some(match action {
        MachineAction::ForceStop => confirm(
            &format!("Force stop {}", m.name),
            "Force stop",
            "Unsaved work inside the guest will be lost.",
            machine,
            Op::ForceStop,
        ),
        MachineAction::Reset => confirm(
            &format!("Reset {}", m.name),
            "Reset",
            "The machine restarts immediately. Unsaved work inside the guest is lost.",
            machine,
            Op::Reset,
        ),
        MachineAction::DiscardState => confirm(
            &format!("Discard the suspended state of {}", m.name),
            "Discard",
            "Unsaved work in the suspended state will be lost.",
            machine,
            Op::DiscardState,
        ),
        MachineAction::Disposable => {
            FormSpec::new(format!("Start {}", m.name), "Start disposable", vec![], move |_| {
                Ok(operation(machine.clone(), Op::Disposable))
            })
        }
        MachineAction::Remove => FormSpec::new(
            format!("Remove {}", m.name),
            "Remove",
            vec![F::checkbox("Delete disk files permanently", false)],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::Remove {
                        delete_disks: v[0] == "true",
                    },
                ))
            },
        )
        .danger(),
        MachineAction::MemoryDump => FormSpec::new(
            format!("Save memory dump of {}", m.name),
            "Save",
            vec![
                F::path("File", Picker::MemoryDump),
                F::choice(
                    "Format",
                    &[
                        ("elf", "ELF core"),
                        ("kdump-zlib", "Compressed kdump"),
                        ("win-dmp", "Windows crash dump"),
                    ],
                    if m.os.contains("microsoft") {
                        "win-dmp"
                    } else {
                        "elf"
                    },
                ),
            ],
            move |v| {
                if v[0].trim().is_empty() {
                    bail!("Choose where to save the memory dump.");
                }
                Ok(operation(
                    machine.clone(),
                    Op::MemoryDump {
                        path: v[0].trim().into(),
                        format: v[1].clone(),
                    },
                ))
            },
        ),
        MachineAction::Screenshot => FormSpec::new(
            "Save screenshot",
            "Save",
            vec![F::path("PNG image", Picker::Screenshot)],
            move |v| {
                if v[0].trim().is_empty() {
                    bail!("Choose where to save the screenshot.");
                }
                Ok(operation(machine.clone(), Op::Screenshot(v[0].clone().into())))
            },
        ),
        MachineAction::Notes => {
            let mut notes = F::text("Notes", m.notes.clone());
            notes.multiline = true;
            FormSpec::new("Notes", "Save", vec![notes], move |v| {
                Ok(operation(machine.clone(), Op::Notes(v[0].clone())))
            })
        }
        MachineAction::CloneConfiguration => FormSpec::new(
            "Clone configuration",
            "Clone",
            vec![F::text("Name", format!("{} Template", m.name))],
            move |v| Ok(operation(machine.clone(), Op::CloneConfiguration(name(v, 0)?))),
        ),
        MachineAction::MoveStorage => FormSpec::new(
            "Move storage",
            "Move",
            vec![F::path("Destination", Picker::Export(format!("{}.hydroxl", m.name)))],
            move |v| {
                let path = PathBuf::from(v[0].trim());
                if path.as_os_str().is_empty() || path.exists() {
                    bail!("Choose a new destination folder.");
                }
                Ok(operation(machine.clone(), Op::MoveStorage(path)))
            },
        ),
        MachineAction::Rename => {
            FormSpec::new("Rename", "Rename", vec![F::text("Name", m.name.clone())], move |v| {
                Ok(operation(machine.clone(), Op::Rename(name(v, 0)?)))
            })
        }
        MachineAction::Clone => FormSpec::new(
            "Clone",
            "Clone",
            vec![F::text("Name", format!("{} Copy", m.name))],
            move |v| Ok(operation(machine.clone(), Op::Clone(name(v, 0)?))),
        ),
        MachineAction::ExportOva => FormSpec::new(
            "Export OVA",
            "Export",
            vec![F::path("Appliance file", Picker::OvaExport)],
            move |v| {
                let path = PathBuf::from(v[0].trim());
                if path.as_os_str().is_empty() || path.exists() {
                    bail!("Choose a new OVA file name.");
                }
                Ok(operation(machine.clone(), Op::ExportOva(path)))
            },
        ),
        MachineAction::Export => FormSpec::new(
            "Export",
            "Export",
            vec![F::path("Destination", Picker::Export(format!("{}.hydroxl", m.name)))],
            move |v| {
                let path = PathBuf::from(v[0].trim());
                if path.as_os_str().is_empty() || path.exists() {
                    bail!("Choose a new export folder.");
                }
                Ok(operation(machine.clone(), Op::Export(path)))
            },
        ),
        MachineAction::GuestTools => super::guest_tools(m, None),
        MachineAction::ChangeIcon => {
            let id = m.id.clone();
            FormSpec::new(
                format!("Icon for {}", m.name),
                "Save",
                vec![F::choice(
                    "Icon",
                    ICONS,
                    if m.icon.is_empty() {
                        "auto"
                    } else {
                        &m.icon
                    },
                )],
                move |v| Ok(Job::Icon(id.clone(), v[0].clone())),
            )
        }
        MachineAction::OperatingSystem => super::operating_system(m),
        MachineAction::ShareFolder => {
            return super::device_form(super::DeviceAction::Share, m);
        }
        _ => return None,
    })
}

/// Icon choices: keys understood by the OS mark matcher. "auto" follows the guest OS.
const ICONS: &[(&str, &str)] = &[
    ("auto", "Automatic"),
    ("none", "Generic"),
    ("win11", "Windows 11"),
    ("win10", "Windows 10"),
    ("win7", "Windows (classic)"),
    ("ubuntu", "Ubuntu"),
    ("fedora", "Fedora"),
    ("debian", "Debian"),
    ("archlinux", "Arch Linux"),
    ("linuxmint", "Linux Mint"),
    ("opensuse", "openSUSE"),
    ("manjaro", "Manjaro"),
    ("centos", "CentOS"),
    ("redhat", "Red Hat"),
    ("rocky", "Rocky Linux"),
    ("almalinux", "AlmaLinux"),
    ("alpine", "Alpine Linux"),
    ("gentoo", "Gentoo"),
    ("nixos", "NixOS"),
    ("kali", "Kali Linux"),
    ("popos", "Pop!_OS"),
    ("elementary", "elementary OS"),
    ("zorin", "Zorin OS"),
    ("freebsd", "FreeBSD"),
    ("openbsd", "OpenBSD"),
    ("netbsd", "NetBSD"),
    ("android", "Android"),
    ("macos", "macOS"),
    ("linux", "Linux"),
];
