//! Platform devices UTM exposes as settings: TPM, USB, tablet, balloon, RNG,
//! Hyper-V enlightenments, clock, network cards, disk interfaces and serial ports.
use super::support::{attach, editable, esc};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, bail};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub struct Platform {
    pub tpm: bool,
    pub tablet: bool,
    /// `qemu-xhci` (USB 3), `ich9-ehci1` (USB 2) or `none`.
    pub usb: String,
    pub balloon: bool,
    pub rng: bool,
    pub hyperv: bool,
    pub localtime: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NicChange {
    pub mac: String,
    /// `user`, `network` or `bridge`.
    pub kind: String,
    /// Network or bridge name; empty for user mode.
    pub source: String,
    pub model: String,
    pub new_mac: String,
    pub isolated: bool,
}

/// The enlightenments virt-install picks for current Windows. Without `synic`/`stimer`
/// Windows falls back to emulated PIT/RTC ticks and stays busy while idle.
pub(crate) const HYPERV: &str = "<hyperv mode='custom'><relaxed state='on'/><vapic state='on'/>\
    <spinlocks state='on' retries='8191'/><vpindex state='on'/><runtime state='on'/>\
    <synic state='on'/><stimer state='on'/><frequencies state='on'/><tlbflush state='on'/>\
    <ipi state='on'/></hyperv>";

const WINDOWS_TIMERS: &str = "<timer name='rtc' tickpolicy='catchup'/>\
    <timer name='pit' tickpolicy='delay'/><timer name='hpet' present='no'/>\
    <timer name='hypervclock' present='yes'/>";

/// Range just before the closing tag of the first `parent` element.
fn before_close(xml: &str, doc: &roxmltree::Document, parent: &str) -> Option<Range<usize>> {
    let node = doc.descendants().find(|n| n.has_tag_name(parent))?;
    let close = format!("</{parent}>");
    let end = node.range().end;
    xml[..end].ends_with(&close).then(|| end - close.len()..end - close.len())
}

pub(super) fn apply_platform(c: Connection, machine: &Machine, p: Platform) -> Result<()> {
    if !["qemu-xhci", "ich9-ehci1", "none"].contains(&p.usb.as_str()) {
        bail!("Choose a USB controller.");
    }
    if p.tablet && p.usb == "none" {
        bail!("The USB tablet needs a USB controller.");
    }
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let devices = before_close(&xml, &doc, "devices").context("The device list is missing.")?;
    let mut edits = Vec::new();
    let mut add = String::new();
    let remove = |edits: &mut Vec<(Range<usize>, String)>,
                  pick: &dyn Fn(roxmltree::Node) -> bool| {
        for n in doc.descendants().filter(|n| n.is_element() && pick(*n)) {
            edits.push((n.range(), String::new()));
        }
    };
    remove(&mut edits, &|n| n.has_tag_name("tpm"));
    if p.tpm {
        let domain_type = doc.root_element().attribute("type").unwrap_or("kvm");
        let machine_type = doc
            .descendants()
            .find(|n| n.has_tag_name("type") && n.parent().is_some_and(|p| p.has_tag_name("os")))
            .and_then(|n| n.attribute("machine"));
        let emulator = backend::qemu::domain_emulator(doc.root_element());
        let model =
            backend::tpm::emulator_model(c, &machine.arch, domain_type, machine_type, emulator)?;
        add += &format!("<tpm model='{model}'><backend type='emulator' version='2.0'/></tpm>");
    }
    remove(&mut edits, &|n| n.has_tag_name("input") && n.attribute("type") == Some("tablet"));
    if p.tablet {
        add += "<input type='tablet' bus='usb'/>";
    }
    remove(&mut edits, &|n| n.has_tag_name("controller") && n.attribute("type") == Some("usb"));
    if p.usb == "none" {
        remove(&mut edits, &|n| {
            n.has_tag_name("redirdev")
                && n.attribute("bus") == Some("usb")
                && n.attribute("type") == Some("spicevmc")
        });
    }
    add += &format!("<controller type='usb' model='{}'/>", p.usb);
    remove(&mut edits, &|n| n.has_tag_name("memballoon"));
    add += if p.balloon {
        "<memballoon model='virtio'/>"
    } else {
        "<memballoon model='none'/>"
    };
    remove(&mut edits, &|n| n.has_tag_name("rng"));
    if p.rng {
        add += "<rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>";
    }
    edits.push((devices, add));

    // An existing block is kept as is so unrelated saves never strip enlightenments.
    let has_hyperv = doc.descendants().any(|n| n.has_tag_name("hyperv"));
    if !p.hyperv {
        remove(&mut edits, &|n| n.has_tag_name("hyperv"));
    } else if !has_hyperv {
        let hyperv = HYPERV;
        match before_close(&xml, &doc, "features") {
            Some(at) => edits.push((at, hyperv.into())),
            None => match doc.descendants().find(|n| n.has_tag_name("features")) {
                // Self-closing <features/>.
                Some(n) => edits.push((n.range(), format!("<features>{hyperv}</features>"))),
                None => {
                    let end = before_close(&xml, &doc, "domain").context("Invalid domain.")?;
                    edits.push((end, format!("<features>{hyperv}</features>")));
                }
            },
        }
    }

    let offset = if p.localtime {
        "localtime"
    } else {
        "utc"
    };
    match doc.descendants().find(|n| n.has_tag_name("clock")) {
        Some(clock) => {
            let text = &xml[clock.range()];
            let open_end = text.find('>').context("Invalid clock.")?;
            let self_closing = text[..open_end].ends_with('/');
            let inner: String =
                clock.children().filter(|c| c.is_element()).map(|c| &xml[c.range()]).collect();
            let _ = self_closing;
            let mut inner = inner;
            if p.hyperv && !inner.contains("'hypervclock'") && !inner.contains("\"hypervclock\"") {
                inner += "<timer name='hypervclock' present='yes'/>";
            }
            edits.push((clock.range(), format!("<clock offset='{offset}'>{inner}</clock>")));
        }
        None => {
            let end = before_close(&xml, &doc, "domain").context("Invalid domain.")?;
            let timers = if p.hyperv {
                WINDOWS_TIMERS
            } else {
                ""
            };
            edits.push((end, format!("<clock offset='{offset}'>{timers}</clock>")));
        }
    }
    drop(doc);
    crate::xml::apply_edits(&mut xml, edits)?;
    backend::define_xml(c, &xml)
}

fn valid_mac(mac: &str) -> bool {
    let parts: Vec<_> = mac.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && u8::from_str_radix(p, 16).is_ok())
}

/// A locally administered unicast MAC in QEMU's 52:54:00 range.
pub fn random_mac() -> String {
    let n = super::support::stamp();
    format!("52:54:00:{:02x}:{:02x}:{:02x}", (n >> 8) as u8, (n >> 16) as u8, (n >> 24) as u8)
}

pub(super) fn apply_nic(c: Connection, machine: &Machine, nic: NicChange) -> Result<()> {
    if !["virtio", "e1000e", "e1000", "rtl8139"].contains(&nic.model.as_str()) {
        bail!("Choose a network card model.");
    }
    if !valid_mac(&nic.new_mac) {
        bail!("Use a MAC address such as 52:54:00:12:34:56.");
    }
    let id = machine.id.as_str();
    editable(c, id)?;
    let mut xml = backend::virsh(c, &["dumpxml", id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let iface = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("interface")
                && n.children()
                    .any(|c| c.has_tag_name("mac") && c.attribute("address") == Some(&nic.mac))
        })
        .context("The network card no longer exists.")?;
    let kind = nic.kind.as_str();
    let source = match kind {
        "user" => String::new(),
        "network" | "bridge" => {
            if nic.source.trim().is_empty() {
                bail!("Choose a network or bridge for the card.");
            }
            format!("<source {kind}='{}'/>", esc(nic.source.trim()))
        }
        _ => bail!("Choose a network mode."),
    };
    let user_only =
        |c: &roxmltree::Node| c.has_tag_name("backend") || c.has_tag_name("portForward");
    let keep: String = iface
        .children()
        .filter(|c| {
            c.is_element()
                && !["mac", "model", "port", "address", "source"].iter().any(|t| c.has_tag_name(*t))
                && (kind == "user" || !user_only(c))
        })
        .map(|c| &xml[c.range()])
        .collect();
    let isolated = if nic.isolated && kind != "user" {
        "<port isolated='yes'/>"
    } else {
        ""
    };
    let replacement = format!(
        "<interface type='{kind}'><mac address='{}'/>{source}<model type='{}'/>{isolated}{keep}</interface>",
        esc(&nic.new_mac),
        nic.model
    );
    let range = iface.range();
    drop(doc);
    xml.replace_range(range, &replacement);
    backend::define_xml(c, &xml)
}

/// Adds a serial port: a pseudo-terminal, or a TCP server bound to localhost.
pub(super) fn apply_add_serial(
    c: Connection,
    machine: &Machine,
    kind: String,
    port: u16,
) -> Result<()> {
    let xml = backend::virsh(c, &["dumpxml", &machine.id, "--inactive"])?;
    let doc = roxmltree::Document::parse(&xml)?;
    let index = doc.descendants().filter(|n| n.has_tag_name("serial")).count();
    let device = match kind.as_str() {
        "pty" => format!("<serial type='pty'><target port='{index}'/></serial>"),
        "tcp" => {
            if port < 1024 {
                bail!("Use a TCP port from 1024 to 65535.");
            }
            format!(
                "<serial type='tcp'><source mode='bind' host='127.0.0.1' service='{port}'/>\
                 <protocol type='raw'/><target port='{index}'/></serial>"
            )
        }
        _ => bail!("Choose a serial port type."),
    };
    attach(c, &machine.id, &device)
}

/// Opens a path in the desktop's default application without blocking.
pub fn open_path(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        bail!("{} does not exist.", path.display());
    }
    let mut command = std::process::Command::new("xdg-open");
    command.arg(path).stdin(std::process::Stdio::null());
    backend::launch_desktop(command)
        .context("Could not open the path. Check the default desktop application.")
}

pub(super) fn apply_show_files(machine: &Machine) -> Result<()> {
    let disk = machine.disk.as_deref().context("This machine has no disk file.")?;
    let folder = std::path::Path::new(disk).parent().context("The disk has no folder.")?;
    open_path(folder)
}

/// libvirt's QEMU log for a machine.
pub fn log_path(c: Connection, machine: &Machine) -> Result<std::path::PathBuf> {
    let file = format!("{}.log", machine.name);
    Ok(match c {
        Connection::Session => std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })
            .context("The cache folder is unknown.")?
            .join("libvirt/qemu/log")
            .join(file),
        Connection::System => std::path::Path::new("/var/log/libvirt/qemu").join(file),
    })
}

/// The end of a machine's log, at most `limit` bytes, starting on a whole line.
pub fn read_log(c: Connection, machine: &Machine, limit: u64) -> Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = log_path(c, machine)?;
    let mut file = std::fs::File::open(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            anyhow::anyhow!("This machine has no log yet. Start it once.")
        }
        std::io::ErrorKind::PermissionDenied => anyhow::anyhow!("Hydroxl cannot read this log."),
        _ => anyhow::anyhow!("Could not read the log."),
    })?;
    let length = file.metadata().context("Could not read the size of the machine log.")?.len();
    let start = length.saturating_sub(limit);
    file.seek(SeekFrom::Start(start)).context("Could not seek to the end of the machine log.")?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).context("Could not read the machine log.")?;
    let text = String::from_utf8_lossy(&bytes);
    let shown = if start > 0 {
        text.split_once('\n').map_or(text.as_ref(), |(_, rest)| rest)
    } else {
        &text
    };
    Ok(shown.to_owned())
}

pub(super) fn apply_open_log(c: Connection, machine: &Machine) -> Result<()> {
    open_path(&log_path(c, machine)?)
}
