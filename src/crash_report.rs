//! Keep the crash dialog outside the process that owns the renderer and guest connections.
use anyhow::{Context, Result};
use flate2::{Compression, write::GzEncoder};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{self, Read, Seek, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode, ExitStatus, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const UI_ARG: &str = "--hydroxl-ui";
const LOG_LIMIT: usize = 128 * 1024;

pub(crate) fn run(args: &[OsString]) -> Result<ExitCode> {
    let executable = std::env::current_exe()?;
    let started = now();
    let mut command = Command::new(&executable);
    command.arg(UI_ARG).args(args).stderr(Stdio::piped());
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        command.env("RUST_BACKTRACE", "1");
    }
    let supervisor = std::process::id() as libc::pid_t;
    // SAFETY: The hook runs after fork and uses only syscalls before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != supervisor {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    let mut stderr = child.stderr.take().expect("child stderr is piped");
    let log = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_LIMIT)));
    let reader_log = log.clone();
    let (drained, reader_done) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        while let Ok(count @ 1..) = stderr.read(&mut buffer) {
            let bytes = &buffer[..count];
            {
                let mut log = reader_log.lock().unwrap_or_else(|err| err.into_inner());
                let excess = (log.len() + count).saturating_sub(LOG_LIMIT);
                log.drain(..excess);
                log.extend(bytes);
            }
            let _ = io::stderr().write_all(bytes);
        }
        let _ = drained.send(());
    });
    let status = child.wait()?;
    // A terminal or other descendant can keep the pipe open after Hydroxl exits.
    let _ = reader_done.recv_timeout(Duration::from_millis(200));
    if crashed(status) {
        eprintln!("Hydroxl closed unexpectedly ({status}).");
        let report = Report {
            executable,
            pid,
            status,
            started,
            ended: now(),
            log: log.lock().unwrap_or_else(|err| err.into_inner()).iter().copied().collect(),
        };
        report.prompt();
    }
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or_default());
    Ok(ExitCode::from(code as u8))
}

fn crashed(status: ExitStatus) -> bool {
    match status.signal() {
        Some(libc::SIGINT | libc::SIGTERM | libc::SIGKILL | libc::SIGHUP) => false,
        _ => !status.success(),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

struct Report {
    executable: PathBuf,
    pid: u32,
    status: ExitStatus,
    started: u64,
    ended: u64,
    log: Vec<u8>,
}

impl Report {
    fn prompt(&self) {
        const SAVE: &str = "Save Crash Dump…";
        if MessageDialog::new()
            .set_title("Hydroxl closed unexpectedly")
            .set_description(
                "Save a crash dump to help diagnose what happened. It may include file paths, machine names, and process memory. The file stays on this computer.",
            )
            .set_buttons(MessageButtons::OkCancelCustom(SAVE.into(), "Close".into()))
            .show()
            != MessageDialogResult::Custom(SAVE.into())
        {
            return;
        }

        let name = format!("hydroxl-crash-{}-{}.tar.gz", self.ended, self.pid);
        while let Some(path) =
            FileDialog::new().set_title("Save Hydroxl Crash Dump").set_file_name(&name).save_file()
        {
            let has_core = match self.save(&path) {
                Ok(has_core) => has_core,
                Err(error) => {
                    eprintln!("Could not save Hydroxl crash dump: {error:#}");
                    let choose_again = "Choose Another Location…";
                    if MessageDialog::new()
                        .set_level(MessageLevel::Error)
                        .set_title("Crash dump couldn’t be saved")
                        .set_description(
                            "Choose a different folder with enough space and try again.",
                        )
                        .set_buttons(MessageButtons::OkCancelCustom(
                            choose_again.into(),
                            "Close".into(),
                        ))
                        .show()
                        == MessageDialogResult::Custom(choose_again.into())
                    {
                        continue;
                    }
                    break;
                }
            };
            let message = if has_core {
                "The archive includes the crash diagnostics and a copy of process memory."
            } else {
                "The archive contains diagnostic logs. A memory dump was unavailable."
            };
            MessageDialog::new()
                .set_title("Crash dump saved")
                .set_description(format!("{message}\n\n{}", path.display()))
                .show();
            break;
        }
    }

    fn coredumpctl(&self, action: &str) -> Command {
        let mut command = Command::new("coredumpctl");
        command.args([
            "--quiet",
            "--no-pager",
            &format!("--since=@{}", self.started),
            &format!("--until=@{}", self.ended + 30),
            action,
            &format!("COREDUMP_PID={}", self.pid),
        ]);
        let mut executable = OsString::from("COREDUMP_EXE=");
        executable.push(&self.executable);
        command.arg(executable).stdin(Stdio::null()).stderr(Stdio::null());
        command
    }

    fn save(&self, path: &Path) -> Result<bool> {
        let folder = path.parent().context("The destination has no parent folder")?;
        // Stage the archive privately and replace the selected file only after a complete write.
        let mut file = tempfile::NamedTempFile::new_in(folder)?;
        let mut archive =
            tar::Builder::new(GzEncoder::new(file.as_file_mut(), Compression::fast()));
        let mut summary = format!(
            "Hydroxl {}\nProcess: {}\nExit: {}\nCrash time (Unix seconds): {}\n",
            env!("CARGO_PKG_VERSION"),
            self.pid,
            self.status,
            self.ended
        )
        .into_bytes();
        if let Ok(info) = self.coredumpctl("info").output()
            && info.status.success()
        {
            summary.extend_from_slice(b"\nSystem crash record\n");
            summary.extend(info.stdout);
        }
        append(&mut archive, "report.txt", &summary)?;
        append(&mut archive, "stderr.log", &self.log)?;

        let mut has_core = false;
        if self.status.signal().is_some() {
            let mut core = tempfile::tempfile_in(folder)?;
            if self
                .coredumpctl("dump")
                .stdout(core.try_clone()?)
                .status()
                .is_ok_and(|status| status.success())
                && core.metadata()?.len() > 0
            {
                core.rewind()?;
                archive.append_file("hydroxl.core", &mut core)?;
                has_core = true;
            }
        }
        archive.into_inner()?.finish()?.sync_all()?;
        file.persist(path)?;
        Ok(has_core)
    }
}

fn append(archive: &mut tar::Builder<impl Write>, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o600);
    header.set_size(bytes.len() as u64);
    archive.append_data(&mut header, name, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_preserves_exit_details_and_diagnostics_without_a_core() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("crash.tar.gz");
        let report = Report {
            executable: PathBuf::from("/hydroxl-test"),
            pid: 0,
            status: ExitStatus::from_raw(101 << 8),
            started: 0,
            ended: 0,
            log: b"test panic\n".to_vec(),
        };
        assert!(crashed(report.status));
        assert!(!crashed(ExitStatus::from_raw(0)));
        assert!(!crashed(ExitStatus::from_raw(libc::SIGTERM)));
        assert!(crashed(ExitStatus::from_raw(libc::SIGSEGV)));
        assert!(!report.save(&path)?);

        let decoder = flate2::read::GzDecoder::new(std::fs::File::open(path)?);
        let mut archive = tar::Archive::new(decoder);
        let mut entries = archive.entries()?;
        let mut summary = entries.next().context("missing summary")??;
        assert_eq!(summary.path()?, Path::new("report.txt"));
        let mut text = String::new();
        summary.read_to_string(&mut text)?;
        assert!(text.contains("exit status: 101"));
        let mut stderr = entries.next().context("missing stderr")??;
        assert_eq!(stderr.path()?, Path::new("stderr.log"));
        text.clear();
        stderr.read_to_string(&mut text)?;
        assert_eq!(text, "test panic\n");
        assert!(entries.next().is_none());
        Ok(())
    }
}
