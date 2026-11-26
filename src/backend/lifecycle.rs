//! Live power operations and shared state guards.
use super::display::ensure_display;
use super::{Action, Connection, State, process::CommandTimedOut, virsh, with_command_deadline};
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

/// Prepare the selected viewer's local display when starting a guest.
pub fn action(connection: Connection, id: &str, action: Action, prefer_spice: bool) -> Result<()> {
    let (command, expected) = match action {
        Action::Display => bail!("Open the display from the machine library."),
        Action::Start => {
            // Keep the optional display changes off the critical path after one short deadline.
            let preparation =
                with_command_deadline(Instant::now() + Duration::from_secs(8), || {
                    let info = virsh(connection, &["dominfo", id])?;
                    if has_managed_save(&info) {
                        return Ok(());
                    }
                    if prefer_spice {
                        super::spice::prepare(connection, id)
                    } else {
                        ensure_display(connection, id)
                    }
                });
            if let Err(error) = preparation {
                eprintln!("Could not prepare the guest display: {error:#}");
            }
            ("start", State::Running)
        }
        Action::Shutdown => ("shutdown", State::Stopped),
        Action::Pause => ("suspend", State::Paused),
        Action::Resume => ("resume", State::Running),
    };
    let result = if matches!(action, Action::Shutdown) {
        with_command_deadline(Instant::now() + Duration::from_secs(10), || {
            virsh(connection, &[command, id])
        })
    } else {
        virsh(connection, &[command, id])
    };
    if let Err(error) = &result
        && error.is::<CommandTimedOut>()
    {
        // The daemon can apply a request before the client times out waiting for its reply.
        let observed = with_command_deadline(Instant::now() + Duration::from_secs(4), || {
            virsh(connection, &["domstate", id])
        });
        if observed.is_ok_and(|state| {
            State::from_virsh(&state) == expected
                || (matches!(action, Action::Shutdown) && state.trim() == "in shutdown")
        }) {
            return Ok(());
        }
    }
    result.map(|_| ())
}

fn has_managed_save(info: &str) -> bool {
    info.lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(field, value)| field == "Managed save" && value.trim() == "yes")
    })
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
        if has_managed_save(&info) {
            bail!("Discard the saved state before changing hardware.");
        }
    }
    Ok(())
}
