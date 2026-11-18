use crate::backend::Connection;
use crate::display::geometry::Scaling;
use std::{collections::BTreeMap, path::PathBuf};
pub(crate) struct Preferences {
    pub(crate) dark: bool,
    pub(crate) connection: Connection,
    pub(crate) selected: Option<String>,
    /// Per-machine icon overrides: machine id and mark key.
    pub(crate) icons: Vec<(String, String)>,
    /// Machines configured to prefer SPICE.
    pub(crate) spice: Vec<String>,
    pub(crate) display_scaling: Scaling,
    pub(crate) vm_scaling: BTreeMap<String, Scaling>,
    pub(crate) toolbar_pinned: bool,
    /// Capture guest input on click instead of when the pointer is over the display.
    pub(crate) capture_on_click: bool,
    pub(crate) vm_location: Option<PathBuf>,
}
impl Preferences {
    pub(crate) fn path() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .map(|p| p.join("hydroxl/preferences"))
    }
    pub(crate) fn machines_dir(&self) -> PathBuf {
        self.vm_location.clone().unwrap_or_else(crate::backend::machines_dir)
    }

    pub(crate) fn scaling_for(&self, id: &str) -> Scaling {
        self.vm_scaling.get(id).copied().unwrap_or(self.display_scaling)
    }

    pub(crate) fn load() -> Self {
        let text = Self::path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Self {
            dark: !text.lines().any(|l| l == "light"),
            connection: if text.lines().any(|l| l == "system") {
                Connection::System
            } else {
                Connection::Session
            },
            display_scaling: text
                .lines()
                .find_map(|line| Scaling::from_key(line.strip_prefix("display-scaling=")?))
                .unwrap_or_default(),
            vm_scaling: text
                .lines()
                .filter_map(|line| {
                    let (mode, id) = line.strip_prefix("vm-scaling=")?.split_once(' ')?;
                    Some((id.to_owned(), Scaling::from_key(mode)?))
                })
                .collect(),
            vm_location: text.lines().find_map(|line| {
                let encoded = line.strip_prefix("vm-location=")?;
                serde_json::from_str::<PathBuf>(encoded).ok()
            }),
            toolbar_pinned: text.lines().any(|line| line == "fullscreen-toolbar=pinned"),
            // Capture when the pointer enters is the default; click is opt-in. Earlier builds
            // wrote a `capture=` line for everyone, so only the newer key is read.
            capture_on_click: text.lines().any(|line| line == "input-capture=click"),
            selected: text.lines().find_map(|l| l.strip_prefix("selected=").map(str::to_owned)),
            icons: text
                .lines()
                .filter_map(|l| l.strip_prefix("icon=")?.split_once(' '))
                .map(|(id, key)| (id.to_owned(), key.to_owned()))
                .collect(),
            spice: text
                .lines()
                .filter_map(|l| l.strip_prefix("viewer=spice "))
                .map(String::from)
                .collect(),
        }
    }
    pub(crate) fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let value = format!(
                "{}\n{}\nselected={}\ndisplay-scaling={}\nfullscreen-toolbar={}\ninput-capture={}\n",
                if self.dark {
                    "dark"
                } else {
                    "light"
                },
                if self.connection == Connection::Session {
                    "session"
                } else {
                    "system"
                },
                self.selected.as_deref().unwrap_or(""),
                self.display_scaling.key(),
                if self.toolbar_pinned {
                    "pinned"
                } else {
                    "auto"
                },
                if self.capture_on_click {
                    "click"
                } else {
                    "pointer"
                }
            ) + &self
                .icons
                .iter()
                .map(|(id, key)| format!("icon={id} {key}\n"))
                .collect::<String>()
                + &self.spice.iter().map(|id| format!("viewer=spice {id}\n")).collect::<String>()
                + &self
                    .vm_scaling
                    .iter()
                    .map(|(id, mode)| format!("vm-scaling={} {id}\n", mode.key()))
                    .collect::<String>()
                + &self
                    .vm_location
                    .as_ref()
                    .and_then(|path| serde_json::to_string(path).ok())
                    .map(|path| format!("vm-location={path}\n"))
                    .unwrap_or_default();
            if let Err(error) = std::fs::write(path, value) {
                eprintln!("Could not save preferences: {error}");
            }
        }
    }
}
