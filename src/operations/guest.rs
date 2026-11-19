//! Work done inside a running guest: mounting shared folders and typing text.
//!
//! The QEMU guest agent is preferred: it runs commands as root (Linux) or SYSTEM
//! (Windows) with no prompts. Without it, Hydroxl drives the guest's keyboard through
//! libvirt (`virsh send-key`), the way a person at the console would: Win+R and an
//! elevated prompt on Windows, a Ctrl+Alt+T terminal and sudo on Linux. Keys are sent as
//! physical US-layout key codes.
use crate::backend::{self, Connection, Machine, State};
use anyhow::{Context, Result, bail};
use std::time::{Duration, Instant};

/// How a shared folder reaches the guest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShareDriver {
    /// virtio-fs: fast, works on Linux and Windows (with WinFsp). Needs shared memory.
    #[default]
    Virtiofs,
    /// 9p: Linux only, needs nothing on the host beyond QEMU.
    NineP,
}

impl ShareDriver {
    /// virtio-fs needs `virtiofsd` on the host.
    pub fn virtiofs_available() -> bool {
        [
            "/usr/lib/virtiofsd",
            "/usr/libexec/virtiofsd",
            "/usr/bin/virtiofsd",
            "/usr/lib/qemu/virtiofsd",
        ]
        .iter()
        .any(|path| std::path::Path::new(path).exists())
    }

    pub fn from_xml(xml: &str) -> Self {
        if xml.contains("virtiofs") {
            Self::Virtiofs
        } else {
            Self::NineP
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Virtiofs => "virtio-fs",
            Self::NineP => "9p",
        }
    }
}

/// The guest family that decides which setup commands apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestOs {
    Linux,
    Windows,
}

impl GuestOs {
    pub fn of(machine: &Machine) -> Self {
        let os = machine.os.to_ascii_lowercase();
        let name = machine.name.to_ascii_lowercase();
        if os.contains("microsoft.com") || name.contains("windows") || name.starts_with("win") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Debug)]
pub enum GuestSetup {
    /// Mount these shares (tag, driver) now and at every boot.
    Shares {
        os: GuestOs,
        shares: Vec<(String, ShareDriver)>,
    },
    /// Unmount these shares and stop mounting them at boot.
    Unmount {
        os: GuestOs,
        shares: Vec<(String, ShareDriver)>,
    },
    /// Type text into whatever has keyboard focus in the guest.
    Type(String),
}

/// Linux mount point for a tag.
pub fn linux_mount_point(tag: &str) -> String {
    format!("/mnt/{tag}")
}

/// One Linux command that mounts a share now and records it in /etc/fstab.
pub fn linux_command(tag: &str, driver: ShareDriver, sudo: bool) -> String {
    let dir = linux_mount_point(tag);
    let (kind, options) = match driver {
        // nofail keeps boot going without the share; x-systemd.automount mounts it on
        // first access, so it also works when the guest boots before the share is ready.
        ShareDriver::Virtiofs => ("virtiofs", "defaults"),
        ShareDriver::NineP => ("9p", "trans=virtio,version=9p2000.L"),
    };
    let boot = format!("{options},nofail,x-systemd.automount");
    let script = format!(
        "mkdir -p {dir} && (mountpoint -q {dir} || mount -t {kind} -o {options} {tag} {dir}) && \
         (grep -q '^{tag} ' /etc/fstab || echo '{tag} {dir} {kind} {boot} 0 0' >> /etc/fstab)"
    );
    if sudo {
        format!("sudo sh -c \"{script}\"")
    } else {
        script
    }
}

/// One Linux command that unmounts a share and removes its /etc/fstab entry.
pub fn linux_unmount_command(tag: &str, sudo: bool) -> String {
    let dir = linux_mount_point(tag);
    let script =
        format!("(! mountpoint -q {dir} || umount {dir}) && sed -i '/^{tag} /d' /etc/fstab");
    if sudo {
        format!("sudo sh -c \"{script}\"")
    } else {
        script
    }
}

/// Windows: stops the virtio-fs service and keeps it from starting at boot, which
/// removes every shared drive. Run from an elevated prompt.
pub const WINDOWS_UNMOUNT_COMMAND: &str =
    "sc stop VirtioFsSvc & sc config VirtioFsSvc start=demand";

/// Windows: virtio-fs runs as a service from the VirtIO guest tools and needs WinFsp.
/// Run from an elevated prompt. Each share appears as its own drive letter.
pub const WINDOWS_COMMAND: &str = "winget install -e --id WinFsp.WinFsp --silent \
     --accept-package-agreements --accept-source-agreements & \
     sc config VirtioFsSvc start=auto & sc start VirtioFsSvc";

pub(super) fn apply_setup(c: Connection, machine: &Machine, setup: GuestSetup) -> Result<()> {
    if machine.state != State::Running {
        bail!("Start the machine first.");
    }
    let id = machine.id.as_str();
    match setup {
        GuestSetup::Type(text) => type_text(c, id, &text),
        GuestSetup::Unmount {
            os,
            shares,
        } => {
            if shares.is_empty() {
                bail!("Add a shared folder first.");
            }
            let (linux, windows_agent, windows_typed) = (
                shares
                    .iter()
                    .map(|(tag, _)| linux_unmount_command(tag, false))
                    .collect::<Vec<_>>()
                    .join(" && "),
                "Stop-Service VirtioFsSvc -ErrorAction SilentlyContinue\nSet-Service VirtioFsSvc -StartupType Manual",
                WINDOWS_UNMOUNT_COMMAND,
            );
            run_in_guest(c, id, os, linux, windows_agent, windows_typed)
        }
        GuestSetup::Shares {
            os,
            shares,
        } => {
            if shares.is_empty() {
                bail!("Add a shared folder first.");
            }
            if os == GuestOs::Windows && shares.iter().any(|(_, d)| *d == ShareDriver::NineP) {
                bail!("Windows cannot read 9p shares. Remove the folder and add it again.");
            }
            let linux = shares
                .iter()
                .map(|(tag, driver)| linux_command(tag, *driver, false))
                .collect::<Vec<_>>()
                .join(" && ");
            run_in_guest(c, id, os, linux, WINDOWS_AGENT_SCRIPT, WINDOWS_COMMAND)
        }
    }
}

/// Runs a root/SYSTEM command in the guest: through the guest agent when it answers;
/// otherwise, on Windows, by typing into Run and confirming the administrator prompt.
fn run_in_guest(
    c: Connection,
    id: &str,
    os: GuestOs,
    linux: String,
    windows_script: &str,
    windows_typed: &str,
) -> Result<()> {
    if agent_ready(c, id) {
        let (path, args) = match os {
            GuestOs::Linux => ("/bin/sh", vec!["-c".to_owned(), linux]),
            GuestOs::Windows => (
                "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-EncodedCommand".into(),
                    utf16_base64(windows_script),
                ],
            ),
        };
        return agent_exec(c, id, path, &args, Duration::from_secs(300));
    }
    match os {
        // A person would press Win+R, type the command and confirm the
        // administrator prompt. Ctrl+Shift+Enter in Run starts it elevated.
        GuestOs::Windows => {
            keys(c, id, &[&["KEY_LEFTMETA", "KEY_R"]])?;
            std::thread::sleep(Duration::from_millis(1200));
            type_text(c, id, &format!("cmd /c {windows_typed}"))?;
            keys(c, id, &[&["KEY_LEFTCTRL", "KEY_LEFTSHIFT", "KEY_ENTER"]])?;
            std::thread::sleep(Duration::from_millis(3000));
            // Yes on the administrator prompt.
            keys(c, id, &[&["KEY_LEFTALT", "KEY_Y"]])
        }
        // Most Linux desktops open a terminal on Ctrl+Alt+T. The command runs with sudo,
        // so the guest asks for the user's password in that terminal.
        GuestOs::Linux => {
            keys(c, id, &[&["KEY_LEFTCTRL", "KEY_LEFTALT", "KEY_T"]])?;
            std::thread::sleep(Duration::from_millis(1800));
            type_text(c, id, &format!("sudo sh -c \"{linux}\"\n"))
        }
    }
}

/// Run as SYSTEM through the guest agent: install WinFsp if missing, then start the
/// virtio-fs service. Failures surface as a non-zero exit with a short message.
const WINDOWS_AGENT_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
if (-not (Get-Service VirtioFsSvc -ErrorAction SilentlyContinue)) { Write-Error 'guest-tools'; exit 2 }
$winfsp = Join-Path ${env:ProgramFiles(x86)} 'WinFsp\bin\winfsp-x64.dll'
if (-not (Test-Path $winfsp)) {
  [Net.ServicePointManager]::SecurityProtocol = 'Tls12'
  $release = Invoke-RestMethod 'https://api.github.com/repos/winfsp/winfsp/releases/latest' -Headers @{ 'User-Agent' = 'guest-setup' }
  $msi = ($release.assets | Where-Object name -like '*.msi' | Select-Object -First 1).browser_download_url
  $file = Join-Path $env:TEMP 'winfsp.msi'
  Invoke-WebRequest $msi -OutFile $file -UseBasicParsing
  Start-Process msiexec.exe -ArgumentList '/i', $file, '/qn', '/norestart' -Wait
}
Set-Service VirtioFsSvc -StartupType Automatic
Start-Service VirtioFsSvc
"#;

fn agent_ready(c: Connection, id: &str) -> bool {
    let configured = backend::virsh(c, &["dumpxml", id])
        .is_ok_and(|xml| super::inspect::has_agent_channel(&xml));
    configured
        && backend::virsh(
            c,
            &["qemu-agent-command", id, "--timeout", "3", r#"{"execute":"guest-ping"}"#],
        )
        .is_ok()
}

/// Runs a program in the guest and waits for it, through the guest agent.
fn agent_exec(c: Connection, id: &str, path: &str, args: &[String], limit: Duration) -> Result<()> {
    let request = serde_json::json!({
        "execute": "guest-exec",
        "arguments": { "path": path, "arg": args, "capture-output": true }
    });
    let reply =
        backend::virsh(c, &["qemu-agent-command", id, "--timeout", "10", &request.to_string()])
            .context("The guest agent did not start the setup.")?;
    let pid = serde_json::from_str::<serde_json::Value>(&reply)?
        .pointer("/return/pid")
        .and_then(serde_json::Value::as_i64)
        .context("The guest agent did not start the setup.")?;
    let status = serde_json::json!({ "execute": "guest-exec-status", "arguments": { "pid": pid } })
        .to_string();
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let reply = backend::virsh(c, &["qemu-agent-command", id, "--timeout", "10", &status])?;
        let value: serde_json::Value = serde_json::from_str(&reply)?;
        let result = value.get("return").context("The guest agent stopped responding.")?;
        if result.get("exited").and_then(serde_json::Value::as_bool) != Some(true) {
            if start.elapsed() > limit {
                bail!("Setup is taking too long. Check the guest and try again.");
            }
            continue;
        }
        let code = result.get("exitcode").and_then(serde_json::Value::as_i64).unwrap_or(0);
        if code == 0 {
            return Ok(());
        }
        let error = result
            .get("err-data")
            .and_then(serde_json::Value::as_str)
            .and_then(base64_decode)
            .unwrap_or_default();
        eprintln!("guest setup exited with {code}: {}", error.trim());
        if error.contains("guest-tools") {
            bail!("Install guest tools in the guest first.");
        }
        if error.contains("Invoke-") || error.contains("winget") {
            bail!("Could not download WinFsp. Check the guest's network and try again.");
        }
        bail!("The guest could not mount the shared folders.");
    }
}

/// QEMU's input queue holds about 50 events and drops the rest; each chord is a press, a
/// hold delay and a release. Overflowing it loses releases, which leaves keys held down
/// and auto-repeating in the guest. Keys go in small batches with time to drain.
const CHORDS_PER_BATCH: usize = 8;
const HOLD_MS: u64 = 20;

/// Presses key chords in order, each released before the next.
fn keys(c: Connection, id: &str, chords: &[&[&str]]) -> Result<()> {
    for batch in chords.chunks(CHORDS_PER_BATCH) {
        let mut commands = String::new();
        for chord in batch {
            commands.push_str(&format!(
                "send-key {id} --codeset linux --holdtime {HOLD_MS} {}\n",
                chord.join(" ")
            ));
        }
        backend::run_with_input("virsh", &["--connect", c.uri()], commands)
            .context("Could not send keys to the guest.")?;
        // Let the batch play out before queuing the next one.
        std::thread::sleep(Duration::from_millis(batch.len() as u64 * (HOLD_MS + 10)));
    }
    Ok(())
}

/// Types text with US-layout key codes.
pub(super) fn type_text(c: Connection, id: &str, text: &str) -> Result<()> {
    let mut chords: Vec<Vec<&'static str>> = Vec::new();
    for ch in text.chars() {
        let (key, shift) = key_for(ch)
            .with_context(|| format!("Hydroxl cannot type “{ch}”. Copy the command instead."))?;
        let mut chord = Vec::with_capacity(2);
        if shift {
            chord.push("KEY_LEFTSHIFT");
        }
        chord.push(key);
        chords.push(chord);
    }
    let chords: Vec<&[&str]> = chords.iter().map(Vec::as_slice).collect();
    keys(c, id, &chords)
}

/// Linux input key name and whether Shift is held, for a US keyboard layout.
fn key_for(ch: char) -> Option<(&'static str, bool)> {
    const LETTERS: [&str; 26] = [
        "KEY_A", "KEY_B", "KEY_C", "KEY_D", "KEY_E", "KEY_F", "KEY_G", "KEY_H", "KEY_I", "KEY_J",
        "KEY_K", "KEY_L", "KEY_M", "KEY_N", "KEY_O", "KEY_P", "KEY_Q", "KEY_R", "KEY_S", "KEY_T",
        "KEY_U", "KEY_V", "KEY_W", "KEY_X", "KEY_Y", "KEY_Z",
    ];
    const DIGITS: [&str; 10] =
        ["KEY_0", "KEY_1", "KEY_2", "KEY_3", "KEY_4", "KEY_5", "KEY_6", "KEY_7", "KEY_8", "KEY_9"];
    if ch.is_ascii_lowercase() {
        return Some((LETTERS[(ch as u8 - b'a') as usize], false));
    }
    if ch.is_ascii_uppercase() {
        return Some((LETTERS[(ch as u8 - b'A') as usize], true));
    }
    if ch.is_ascii_digit() {
        return Some((DIGITS[(ch as u8 - b'0') as usize], false));
    }
    Some(match ch {
        ' ' => ("KEY_SPACE", false),
        '\n' => ("KEY_ENTER", false),
        '\t' => ("KEY_TAB", false),
        '-' => ("KEY_MINUS", false),
        '_' => ("KEY_MINUS", true),
        '=' => ("KEY_EQUAL", false),
        '+' => ("KEY_EQUAL", true),
        '[' => ("KEY_LEFTBRACE", false),
        '{' => ("KEY_LEFTBRACE", true),
        ']' => ("KEY_RIGHTBRACE", false),
        '}' => ("KEY_RIGHTBRACE", true),
        '\\' => ("KEY_BACKSLASH", false),
        '|' => ("KEY_BACKSLASH", true),
        ';' => ("KEY_SEMICOLON", false),
        ':' => ("KEY_SEMICOLON", true),
        '\'' => ("KEY_APOSTROPHE", false),
        '"' => ("KEY_APOSTROPHE", true),
        '`' => ("KEY_GRAVE", false),
        '~' => ("KEY_GRAVE", true),
        ',' => ("KEY_COMMA", false),
        '<' => ("KEY_COMMA", true),
        '.' => ("KEY_DOT", false),
        '>' => ("KEY_DOT", true),
        '/' => ("KEY_SLASH", false),
        '?' => ("KEY_SLASH", true),
        '!' => ("KEY_1", true),
        '@' => ("KEY_2", true),
        '#' => ("KEY_3", true),
        '$' => ("KEY_4", true),
        '%' => ("KEY_5", true),
        '^' => ("KEY_6", true),
        '&' => ("KEY_7", true),
        '*' => ("KEY_8", true),
        '(' => ("KEY_9", true),
        ')' => ("KEY_0", true),
        _ => return None,
    })
}

/// PowerShell's -EncodedCommand: base64 of UTF-16LE text.
fn utf16_base64(text: &str) -> String {
    let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64_encode(&bytes)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<String> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=') {
        let value = ALPHABET.iter().position(|a| *a == byte)? as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for text in ["", "a", "ab", "abc", "guest-tools error"] {
            assert_eq!(base64_decode(&base64_encode(text.as_bytes())).unwrap(), text);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn every_setup_command_is_typeable() {
        for text in [
            WINDOWS_COMMAND.to_owned(),
            linux_command("Documents", ShareDriver::Virtiofs, true),
            linux_command("my_share-2", ShareDriver::NineP, true),
            linux_unmount_command("Docs", true),
            WINDOWS_UNMOUNT_COMMAND.to_owned(),
        ] {
            for ch in text.chars() {
                assert!(key_for(ch).is_some(), "cannot type {ch:?} in {text}");
            }
        }
    }

    #[test]
    fn linux_command_mounts_and_persists() {
        let command = linux_command("Docs", ShareDriver::Virtiofs, true);
        assert!(command.starts_with("sudo sh -c"));
        assert!(command.contains("mount -t virtiofs -o defaults Docs /mnt/Docs"));
        assert!(command.contains("virtiofs defaults,nofail,x-systemd.automount 0 0"));
        assert!(command.contains(">> /etc/fstab"));
    }
}
