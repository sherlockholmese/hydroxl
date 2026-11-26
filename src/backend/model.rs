use anyhow::{Result, bail};
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Paused,
    Stopped,
    Other,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopped => "Powered off",
            Self::Other => "Unavailable",
        }
    }
    pub fn from_virsh(value: &str) -> Self {
        match value.trim() {
            "running" | "idle" | "blocked" | "in shutdown" => Self::Running,
            "paused" | "pmsuspended" => Self::Paused,
            "shut off" | "crashed" => Self::Stopped,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub state: State,
    pub cpus: u32,
    pub memory_mib: u64,
    pub arch: String,
    pub disk: Option<String>,
    pub network: String,
    pub display: String,
    pub acceleration: String,
    pub qemu_backend: super::QemuBackend,
    pub autostart: bool,
    pub saved_state: bool,
    pub notes: String,
    /// libosinfo identifier, e.g. "http://ubuntu.com/ubuntu/24.04". Empty when unknown.
    pub os: String,
    /// Icon chosen by the user; empty follows the operating system.
    pub icon: String,
    /// Prefer SPICE over VNC when both displays are available.
    pub spice_viewer: bool,
}
impl Machine {
    pub fn can_edit(&self) -> bool {
        self.state == State::Stopped && !self.saved_state
    }
    pub fn status(&self) -> &'static str {
        if self.saved_state && self.state == State::Stopped {
            "Suspended"
        } else {
            self.state.label()
        }
    }
    pub fn memory(&self) -> String {
        if self.memory_mib.is_multiple_of(1024) {
            format!("{} GiB", self.memory_mib / 1024)
        } else {
            format!("{} MiB", self.memory_mib)
        }
    }
    pub fn architecture(&self) -> &str {
        match self.arch.as_str() {
            "x86_64" => "x86-64",
            "aarch64" => "ARM64",
            _ => &self.arch,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Connection {
    Session,
    System,
}
impl Connection {
    pub fn uri(self) -> &'static str {
        match self {
            Self::Session => "qemu:///session",
            Self::System => "qemu:///system",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "User session",
            Self::System => "System",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Start,
    Shutdown,
    Pause,
    Resume,
    Display,
}

#[derive(Clone, Debug)]
pub struct NewMachine {
    pub name: String,
    pub cpus: u32,
    pub memory_gib: u64,
    pub disk_gib: u64,
    /// Installation image. `None` creates an empty CD/DVD drive to mount later.
    pub iso: Option<PathBuf>,
    pub network_boot: bool,
    /// Disk image used in place instead of allocating a new one.
    pub existing_disk: Option<PathBuf>,
    /// Folder that receives the machine's own folder and disk.
    pub location: PathBuf,
    pub architecture: String,
    pub qemu_build: super::QemuBuild,
    pub os_variant: String,
    pub emulate: bool,
    pub uefi: bool,
}
impl NewMachine {
    pub fn validate(&self) -> Result<()> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            bail!("Use a machine name between 1 and 64 characters.");
        }
        if !(1..=64).contains(&self.cpus) {
            bail!("Choose between 1 and 64 CPU cores.");
        }
        if !(1..=256).contains(&self.memory_gib) {
            bail!("Choose between 1 and 256 GiB of memory.");
        }
        if let Some(disk) = &self.existing_disk {
            if !disk.is_file() {
                bail!("Choose an existing disk image.");
            }
        } else if !(8..=2048).contains(&self.disk_gib) {
            bail!("Choose a disk between 8 and 2048 GiB.");
        }
        if !ARCHITECTURES.iter().any(|(value, _)| *value == self.architecture) {
            bail!("Choose a supported architecture.");
        }
        if !self.emulate && self.architecture != std::env::consts::ARCH {
            bail!("Choose Emulate for a different CPU architecture.");
        }
        if self.iso.as_ref().is_some_and(|iso| !iso.is_file()) {
            bail!("Choose an existing installation ISO.");
        }
        if !self.location.is_dir() {
            bail!("Choose an existing folder for the machine files.");
        }
        Ok(())
    }
}

pub const ARCHITECTURES: &[(&str, &str)] = &[
    ("x86_64", "x86-64"),
    ("i686", "x86 (32-bit)"),
    ("aarch64", "ARM64"),
    ("armv7l", "ARM (32-bit)"),
    ("riscv64", "RISC-V 64"),
    ("ppc", "PowerPC"),
    ("ppc64", "PowerPC 64"),
];
