//! Live power operations and shared state guards.
use super::display::ensure_display;
use super::{Action, Connection, State, virsh};
use anyhow::{Result, bail};

/// Prepare the selected viewer's local display when starting a guest.
pub fn action(connection: Connection, id: &str, action: Action, prefer_spice: bool) -> Result<()> {
    match action {
        Action::Display => bail!("Open the display from the machine library."),
        Action::Start => {
            // Best effort: a missing in-app display must never block starting the guest.
            let saved = virsh(connection, &["dominfo", id])?
                .lines()
                .any(|line| line.starts_with("Managed save:") && line.contains("yes"));
            if !saved {
                let display_setup = if prefer_spice {
                    super::spice::prepare(connection, id)
                } else {
                    ensure_display(connection, id)
                };
                if let Err(error) = display_setup {
                    eprintln!("Could not prepare the guest display: {error:#}");
                }
            }
            virsh(connection, &["start", id])?;
        }
        Action::Shutdown => {
            virsh(connection, &["shutdown", id])?;
        }
        Action::Pause => {
            virsh(connection, &["suspend", id])?;
        }
        Action::Resume => {
            virsh(connection, &["resume", id])?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_stopped(
    connection: Connection,
    id: &str,
    changing_hardware: bool,
) -> Result<()> {
    if State::from_virsh(&virsh(connection, &["domstate", id])?) != State::Stopped {
        bail!("Power off the machine first.");
    }
    if changing_hardware {
        let info = virsh(connection, &["dominfo", id])?;
        if info.lines().any(|line| line.starts_with("Managed save:") && line.contains("yes")) {
            bail!("Discard the saved state before changing hardware.");
        }
    }
    Ok(())
}
