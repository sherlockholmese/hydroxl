use anyhow::{Context, Result, anyhow, bail};
use std::{
    io::{self, BufReader, Read, Write},
    os::unix::process::CommandExt,
    process::{Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String> {
    run_inner(program, args, None, None)
}

/// Launches an application that can outlive the calling job, and reaps it when it exits.
pub(crate) fn launch_desktop(mut command: Command) -> Result<()> {
    let (spawned, ready) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name("desktop-launch".into())
        .spawn(move || match command.spawn() {
            Ok(mut child) => {
                let _ = spawned.send(Ok(()));
                match child.wait() {
                    Ok(status) if status.success() => {}
                    Ok(status) => eprintln!("{:?} exited with {status}", command.get_program()),
                    Err(error) => {
                        eprintln!("Could not wait for {:?}: {error}", command.get_program())
                    }
                }
            }
            Err(error) => {
                let _ = spawned.send(Err(error));
            }
        })
        .context("Could not start the desktop application launcher.")?;
    ready.recv().context("The desktop application launcher stopped unexpectedly.")??;
    Ok(())
}

/// Runs a program reading `input` on stdin, such as a batch of virsh commands.
pub(crate) fn run_with_input(program: &str, args: &[&str], input: String) -> Result<String> {
    run_inner(program, args, None, Some(input))
}

pub(crate) fn qemu_convert(
    args: &[&str],
    progress: super::progress::Reporter,
    stage: String,
) -> Result<()> {
    let mut command = vec!["convert", "-p"];
    command.extend_from_slice(args);
    progress.update(stage.clone(), Some(0));
    let meter = Meter {
        reporter: progress,
        stage,
        parse: qemu_percent,
        stderr: false,
    };
    run_inner("qemu-img", &command, Some(meter), None).map(|_| ())
}

/// Runs a virsh command that takes `--verbose` and reports `Name: [ 42.10 %]` on stderr.
pub(crate) fn virsh_with_progress(
    connection: super::Connection,
    args: &[&str],
    progress: &super::progress::Reporter,
    stage: &str,
) -> Result<()> {
    let mut all = vec!["--connect", connection.uri()];
    all.extend_from_slice(args);
    all.push("--verbose");
    progress.update(stage, Some(0));
    let meter = Meter {
        reporter: progress.clone(),
        stage: stage.to_owned(),
        parse: virsh_percent,
        stderr: true,
    };
    run_inner("virsh", &all, Some(meter), None).map(|_| ())
}

/// A progress reading taken from one output stream of a running command.
struct Meter {
    reporter: super::progress::Reporter,
    stage: String,
    parse: fn(&str) -> Option<u8>,
    stderr: bool,
}

impl Meter {
    /// Publishes progress without discarding the command's diagnostic records.
    fn follow(self, stream: impl Read) -> io::Result<String> {
        let mut stream = BufReader::new(stream);
        let mut rest = Vec::new();
        let mut last = None;
        let mut record = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let done = match stream.read(&mut byte) {
                Ok(0) => true,
                Ok(_) => false,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if !done && !matches!(byte[0], b'\r' | b'\n') {
                if record.len() < DIAGNOSTIC_LIMIT {
                    record.push(byte[0]);
                }
                continue;
            }
            let line = String::from_utf8_lossy(&record);
            if let Some(value) = (self.parse)(&line) {
                if Some(value) != last {
                    self.reporter.update(&self.stage, Some(value));
                    last = Some(value);
                }
            } else if !line.trim().is_empty() && rest.len() < DIAGNOSTIC_LIMIT {
                let available = DIAGNOSTIC_LIMIT - rest.len();
                rest.extend_from_slice(&record[..record.len().min(available)]);
                rest.push(b'\n');
            }
            record.clear();
            if done {
                return Ok(String::from_utf8_lossy(&rest).into_owned());
            }
        }
    }
}

const DIAGNOSTIC_LIMIT: usize = 128 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;

fn read_output(mut stream: impl Read, limit: usize, diagnostic: bool) -> io::Result<String> {
    let mut bytes = Vec::new();
    stream.by_ref().take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        // Always drain the pipe so a child can finish even when its output is too large.
        io::copy(&mut stream, &mut io::sink())?;
        if !diagnostic {
            return Err(io::Error::other("The command returned more than 64 MiB of output."));
        }
        bytes.truncate(limit);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_pipe(
    stream: impl Read + Send + 'static,
    meter: Option<Meter>,
    diagnostic: bool,
) -> io::Result<JoinHandle<io::Result<String>>> {
    thread::Builder::new().name("command-output".into()).spawn(move || match meter {
        Some(meter) => meter.follow(stream),
        None => read_output(
            stream,
            if diagnostic {
                DIAGNOSTIC_LIMIT
            } else {
                OUTPUT_LIMIT
            },
            diagnostic,
        ),
    })
}

fn join_io<T>(worker: JoinHandle<io::Result<T>>, message: &str) -> Result<T> {
    worker
        .join()
        .map_err(|_| anyhow!("{message} The command worker stopped unexpectedly."))?
        .context(message.to_owned())
}

fn virsh_percent(record: &str) -> Option<u8> {
    let (_, tail) = record.split_once(": [")?;
    let value = tail.strip_suffix("%]")?.trim().parse::<f32>().ok()?;
    (value.is_finite() && (0. ..=100.).contains(&value)).then_some(value.floor() as u8)
}

fn qemu_percent(chunk: &str) -> Option<u8> {
    let (start, _) = chunk.trim().strip_prefix('(')?.split_once("/100%)")?;
    let value = start.trim().parse::<f32>().ok()?;
    (value.is_finite() && (0. ..=100.).contains(&value)).then_some(value.floor() as u8)
}

fn command_label(program: &str, args: &[&str]) -> String {
    match args {
        ["--connect", _, command, ..] if program == "virsh" => format!("Libvirt ({command})"),
        _ => program.to_owned(),
    }
}

fn run_inner(
    program: &str,
    args: &[&str],
    progress: Option<Meter>,
    input: Option<String>,
) -> Result<String> {
    run_with_timeout(program, args, progress, input, command_timeout(program, args))
}

fn command_timeout(program: &str, args: &[&str]) -> Duration {
    let long_operation = matches!(program, "curl" | "qemu-img" | "virt-clone")
        || matches!(
            args,
            ["--connect", _, "dump" | "managedsave" | "snapshot-create-as" | "snapshot-revert", ..]
        );
    Duration::from_secs(if long_operation {
        3600
    } else if program == "pkexec" {
        120
    } else {
        30
    })
}

fn run_with_timeout(
    program: &str,
    args: &[&str],
    progress: Option<Meter>,
    input: Option<String>,
    timeout: Duration,
) -> Result<String> {
    let executable = if program == "qemu-img" {
        super::qemu::image_tool()?
    } else {
        std::path::PathBuf::from(program)
    };
    let mut child = Command::new(&executable)
        .args(args)
        .env("LC_ALL", "C")
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            let message = match error.kind() {
                io::ErrorKind::NotFound => {
                    format!("Could not start {program}. Install it or check its configured path.")
                }
                io::ErrorKind::PermissionDenied => format!(
                    "Could not start {program}. Check execute permission for {}.",
                    executable.display()
                ),
                _ => format!("Could not start {program}."),
            };
            anyhow::Error::new(error).context(message)
        })?;
    let result = (|| -> Result<_> {
        let stdout = child.stdout.take().context("The command output pipe is unavailable.")?;
        let stderr = child.stderr.take().context("The command error pipe is unavailable.")?;
        let (out_meter, err_meter) = match progress {
            Some(meter) if meter.stderr => (None, Some(meter)),
            meter => (meter, None),
        };
        let out = read_pipe(stdout, out_meter, false).context("Could not read command output.")?;
        let err =
            read_pipe(stderr, err_meter, true).context("Could not read command diagnostics.")?;
        let writer = if let Some(input) = input {
            let mut stdin = child.stdin.take().context("The command input pipe is unavailable.")?;
            Some(
                thread::Builder::new()
                    .name("command-input".into())
                    .spawn(move || stdin.write_all(input.as_bytes()))
                    .context("Could not send input to the command.")?,
            )
        } else {
            None
        };
        let start = Instant::now();
        loop {
            // Hold the child PID until the pipes close or cleanup is needed.
            // Reaping it sooner would allow its process-group ID to be reused.
            if out.is_finished()
                && err.is_finished()
                && writer.as_ref().is_none_or(JoinHandle::is_finished)
                && let Some(status) = child.try_wait().with_context(|| {
                    format!("Could not check the result of {}.", command_label(program, args))
                })?
            {
                return Ok((status, out, err, writer));
            }
            if start.elapsed() >= timeout {
                let label = command_label(program, args);
                if program == "virsh" {
                    bail!(
                        "{label} timed out. Refresh the library to check the result before trying again."
                    );
                }
                bail!("{label} timed out after {} seconds.", timeout.as_secs_f64());
            }
            thread::sleep(Duration::from_millis(40));
        }
    })();
    let (status, out, err, writer) = match result {
        Ok(result) => result,
        Err(error) => {
            // Descendants can hold a pipe open after the direct child exits.
            // This command owns its process group, so stop the entire group.
            if let Ok(pid) = i32::try_from(child.id()) {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            // A hung or privileged child may take longer to exit. Reap it away from the caller.
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = thread::Builder::new()
                    .name("command-cleanup".into())
                    .spawn(move || child.wait());
            }
            return Err(error);
        }
    };
    let output = join_io(out, "Could not read command output.");
    let errors = join_io(err, "Could not read command diagnostics.");
    let sent = writer.map(|writer| join_io(writer, "The command did not receive all its input."));
    if !status.success() {
        let diagnostics = errors
            .as_deref()
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| output.as_deref().ok())
            .unwrap_or_default();
        eprintln!("{} ({status}): {}", command_label(program, args), diagnostics.trim());
        if diagnostics.trim().is_empty() {
            return Err(empty_diagnostic(program, args, status));
        }
        return Err(command_failure(program, args, diagnostics));
    }
    if let Some(sent) = sent {
        sent?;
    }
    errors?;
    output
}

fn empty_diagnostic(program: &str, args: &[&str], status: ExitStatus) -> anyhow::Error {
    anyhow!("{} failed ({status}) without a diagnostic.", command_label(program, args))
}

#[derive(Debug)]
pub(crate) struct SnapshotNotFound;

impl std::fmt::Display for SnapshotNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("This snapshot no longer exists. Refresh the machine and try again.")
    }
}

impl std::error::Error for SnapshotNotFound {}

fn command_failure(program: &str, args: &[&str], error: &str) -> anyhow::Error {
    let lower = error.to_ascii_lowercase();
    if program == "virsh" {
        match args {
            ["--connect", _, "snapshot-create-as", ..]
                if lower.contains("virgl is not yet migratable") =>
            {
                return anyhow!(
                    "VirGL prevents snapshots while the machine is running. Shut it down and retry."
                );
            }
            ["--connect", _, "snapshot-dumpxml" | "snapshot-delete" | "snapshot-revert", ..]
                if lower.contains("domain snapshot not found") =>
            {
                return SnapshotNotFound.into();
            }
            _ => {}
        }
    }
    if lower.contains("tpm")
        && (lower.contains("not supported")
            || lower.contains("swtpm")
            || lower.contains("unsupported"))
    {
        return diagnostic(program, error).context(super::tpm::UNAVAILABLE);
    }
    if error.contains("already active") {
        return diagnostic(program, error)
            .context("The machine is already running. Refresh its state.");
    }
    diagnostic(program, error)
}

fn diagnostic(program: &str, stderr: &str) -> anyhow::Error {
    let message = stderr
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("error: ").unwrap_or(line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !message.is_empty() {
        return anyhow!("{message}");
    }
    if program == "virsh" {
        anyhow!("Libvirt did not provide an error message.")
    } else {
        anyhow!("{program} failed without an error message.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_non_utf8_stdout_and_silent_failures() -> Result<()> {
        let output = run("sh", &["-c", "printf 'first\\377last'"])?;
        assert_eq!(output, "first�last");
        let error = run("sh", &["-c", "exit 7"]).unwrap_err().to_string();
        assert!(error.contains("exit status: 7"));
        let error = run("sh", &["-c", "printf 'diagnostic on stdout'; exit 1"]).unwrap_err();
        assert_eq!(error.to_string(), "diagnostic on stdout");
        Ok(())
    }

    #[test]
    fn times_out_when_a_child_leaves_its_output_pipe_open() {
        let started = Instant::now();
        let error = run_with_timeout(
            "sh",
            &["-c", "sleep 10 & exit 0"],
            None,
            None,
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn reports_incomplete_command_input() {
        // A large write ensures part of the input remains when the shell exits.
        let error = run_with_input("sh", &["-c", "exit 0"], "x".repeat(1024 * 1024)).unwrap_err();
        assert!(error.to_string().contains("did not receive all its input"));
    }

    #[test]
    fn explains_virgl_snapshot_failure() {
        let error = "error: Requested operation is not valid: cannot migrate domain: virgl is not yet migratable";
        let connection = "qemu:///session";
        assert_eq!(
            command_failure("virsh", &["--connect", connection, "snapshot-create-as"], error)
                .to_string(),
            "VirGL prevents snapshots while the machine is running. Shut it down and retry."
        );
        assert_eq!(
            command_failure("virsh", &["--connect", connection, "managedsave"], error).to_string(),
            "Requested operation is not valid: cannot migrate domain: virgl is not yet migratable"
        );
    }

    #[test]
    fn distinguishes_a_missing_snapshot_from_other_lookup_errors() {
        let args = ["--connect", "qemu:///session", "snapshot-dumpxml", "machine", "backup"];
        let missing =
            "error: Domain snapshot not found: no domain snapshot with matching name 'backup'";
        assert!(command_failure("virsh", &args, missing).is::<SnapshotNotFound>());
        assert!(!command_failure("virsh", &args, "permission denied").is::<SnapshotNotFound>());
    }

    #[test]
    fn keeps_the_libvirt_diagnostic_after_the_command_summary() {
        let stderr = "error: failed to connect to the hypervisor\nerror: Failed to connect socket to '/run/libvirt/virtqemud-sock': Permission denied\n";
        let args = ["--connect", "qemu:///system", "list"];
        assert_eq!(
            command_failure("virsh", &args, stderr).to_string(),
            "failed to connect to the hypervisor\nFailed to connect socket to '/run/libvirt/virtqemud-sock': Permission denied"
        );
    }

    #[test]
    fn parses_qemu_img_progress_records() {
        assert_eq!(qemu_percent("    (33.21/100%)\r"), Some(33));
        assert_eq!(qemu_percent("    (100.00/100%)\n"), Some(100));
        assert_eq!(virsh_percent("Dump: [ 56.64 %]"), Some(56));
        assert_eq!(virsh_percent("Dump: [100 %]"), Some(100));
        assert_eq!(virsh_percent("error: Failed to core dump"), None);
        assert_eq!(qemu_percent("qemu-img: Could not open disk"), None);
        assert_eq!(qemu_percent("(NaN/100%)"), None);
    }
}
