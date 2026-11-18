use anyhow::{Context, Result, bail};
use std::{
    io::{BufReader, Read},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String> {
    run_inner(program, args, None, None)
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
    /// Reads `stream` record by record (records end in `\r` or `\n`), publishing each
    /// new percentage. Records that are not progress are returned for diagnostics.
    fn follow(self, stream: impl Read) -> String {
        let mut stream = BufReader::new(stream);
        let mut rest = String::new();
        let mut last = None;
        let mut record = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let done = !matches!(stream.read(&mut byte), Ok(1));
            if !done && byte[0] != b'\r' && byte[0] != b'\n' {
                record.push(byte[0]);
                continue;
            }
            let text = String::from_utf8_lossy(&record);
            match (self.parse)(&text) {
                Some(value) if Some(value) != last => {
                    self.reporter.update(&self.stage, Some(value));
                    last = Some(value);
                }
                Some(_) => {}
                None if !text.trim().is_empty() => {
                    rest.push_str(&text);
                    rest.push('\n');
                }
                None => {}
            }
            record.clear();
            if done {
                return rest;
            }
        }
    }
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

fn run_inner(
    program: &str,
    args: &[&str],
    progress: Option<Meter>,
    input: Option<String>,
) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Install {program} and try again."))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        thread::spawn(move || {
            use std::io::Write as _;
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let mut stdout = child.stdout.take().context("Could not read command output.")?;
    let mut stderr = child.stderr.take().context("Could not read command errors.")?;
    let (out_meter, err_meter) = match progress {
        Some(meter) if meter.stderr => (None, Some(meter)),
        meter => (meter, None),
    };
    let out = thread::spawn(move || match out_meter {
        Some(meter) => {
            meter.follow(stdout);
            String::new()
        }
        None => {
            let mut s = String::new();
            let _ = stdout.read_to_string(&mut s);
            s
        }
    });
    let err = thread::spawn(move || match err_meter {
        Some(meter) => meter.follow(stderr),
        None => {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        }
    });
    let timeout = if program == "curl"
        || program == "qemu-img"
        || program == "virt-clone"
        || args
            .iter()
            .any(|a| ["dump", "managedsave", "snapshot-create-as", "snapshot-revert"].contains(a))
    {
        Duration::from_secs(3600)
    } else {
        Duration::from_secs(30)
    };
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("The connection timed out. Check libvirt and retry.");
        }
        thread::sleep(Duration::from_millis(40));
    };
    let output = out.join().unwrap_or_default();
    let error = err.join().unwrap_or_default();
    if !status.success() {
        // Detailed diagnostics stay in the process log, not the product interface.
        eprintln!("{program}: {}", error.trim());
        let lower = error.to_ascii_lowercase();
        if lower.contains("tpm")
            && (lower.contains("not supported")
                || lower.contains("swtpm")
                || lower.contains("unsupported"))
        {
            bail!(super::tpm::UNAVAILABLE);
        }
        if error.contains("already active") {
            bail!("The machine is already running. Refresh its state.");
        }
        if error.contains("permission denied")
            || error.contains("authentication")
            || error.contains("not authorized")
        {
            bail!("Access denied. Check your libvirt permissions and retry.");
        }
        bail!("The operation failed. Check the libvirt connection and retry.");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

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
