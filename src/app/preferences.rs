use crate::backend::Connection;
use crate::display::geometry::Scaling;
use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::ErrorKind,
    path::{Path, PathBuf},
};
#[derive(Clone)]
pub(crate) struct Preferences {
    pub(crate) dark: bool,
    pub(crate) connection: Connection,
    pub(crate) selected: Option<String>,
    /// Per-machine icon overrides: machine id and mark key.
    pub(crate) icons: Vec<(String, String)>,
    /// Machines explicitly configured to prefer VNC over the default SPICE viewer.
    pub(crate) vnc: BTreeSet<String>,
    pub(crate) display_scaling: Scaling,
    pub(crate) vm_scaling: BTreeMap<String, Scaling>,
    pub(crate) toolbar_pinned: bool,
    /// Capture guest input on click instead of when the pointer is over the display.
    pub(crate) capture_on_click: bool,
    pub(crate) vm_location: Option<PathBuf>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self::parse("")
    }
}

impl Preferences {
    pub(crate) fn path() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .map(|p| p.join("hydroxl/preferences"))
    }
    pub(crate) fn machines_dir(&self) -> PathBuf {
        self.vm_location.clone().unwrap_or_else(crate::backend::machines_dir)
    }

    pub(crate) fn scaling_for(&self, id: &str) -> Scaling {
        self.vm_scaling.get(id).copied().unwrap_or(self.display_scaling)
    }

    pub(crate) fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "Could not read preferences from {}. Check that the file is readable.",
                    path.display()
                )
            }),
        }
    }

    fn parse(text: &str) -> Self {
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
            selected: text.lines().find_map(|l| {
                l.strip_prefix("selected=").filter(|id| !id.is_empty()).map(str::to_owned)
            }),
            icons: text
                .lines()
                .filter_map(|l| l.strip_prefix("icon=")?.split_once(' '))
                .map(|(id, key)| (id.to_owned(), key.to_owned()))
                .collect(),
            vnc: text
                .lines()
                .filter_map(|l| l.strip_prefix("viewer=vnc "))
                .map(String::from)
                .collect(),
        }
    }
    pub(crate) fn save(&self) -> Result<()> {
        let path = Self::path()
            .context("Could not save preferences. Set XDG_CONFIG_HOME to a writable folder.")?;
        self.write(&path).with_context(|| {
            format!(
                "Could not save preferences to {}. Check that the folder is writable.",
                path.display()
            )
        })
    }

    fn write(&self, path: &Path) -> Result<()> {
        // Preferences can be symlinked into a user's dotfiles. Replace the target atomically.
        let target = match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Some(
                path.canonicalize()
                    .context("The preferences link does not point to a readable file.")?,
            ),
            Ok(_) => None,
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("Could not inspect the existing preferences."),
        };
        let path = target.as_deref().unwrap_or(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::read_to_string(path) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("The existing preferences could not be read."),
        }
        let location = self
            .vm_location
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("Choose a VM folder with a valid UTF-8 name.")?;
        let mut value = format!(
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
            },
        );
        for (id, key) in &self.icons {
            writeln!(value, "icon={id} {key}")?;
        }
        for id in &self.vnc {
            writeln!(value, "viewer=vnc {id}")?;
        }
        for (id, mode) in &self.vm_scaling {
            writeln!(value, "vm-scaling={} {id}", mode.key())?;
        }
        if let Some(location) = location {
            writeln!(value, "vm-location={location}")?;
        }
        crate::files::write_atomic(path, value.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Preferences;

    #[test]
    fn saves_preferences_and_preserves_an_unreadable_existing_file() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("preferences");
        let original = b"dark\nsession\n\xff";
        std::fs::write(&path, original)?;
        let prefs = Preferences {
            dark: false,
            ..Preferences::default()
        };
        assert!(prefs.write(&path).is_err());
        assert_eq!(std::fs::read(&path)?, original);
        std::fs::write(&path, "dark\nsession\n")?;
        prefs.write(&path)?;
        assert!(!Preferences::parse(&std::fs::read_to_string(path)?).dark);
        Ok(())
    }

    #[test]
    fn preserves_a_preferences_symlink() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("preferences");
        let target = folder.path().join("dotfiles-preferences");
        std::fs::write(&target, "dark\nsession\n")?;
        std::os::unix::fs::symlink(&target, &path)?;
        let prefs = Preferences {
            dark: false,
            ..Preferences::default()
        };
        prefs.write(&path)?;
        assert!(std::fs::symlink_metadata(path)?.is_symlink());
        assert!(!Preferences::parse(&std::fs::read_to_string(target)?).dark);
        Ok(())
    }

    #[test]
    fn only_explicit_vnc_overrides_the_spice_default() {
        let preferences = Preferences::parse("viewer=spice legacy\nviewer=vnc opted-out\n");
        assert!(!preferences.vnc.contains("legacy"));
        assert!(!preferences.vnc.contains("new-machine"));
        assert!(preferences.vnc.contains("opted-out"));
    }
}
