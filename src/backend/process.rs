use anyhow::{Context, Result, bail};
use std::{
    io::{BufRead as _, BufReader, Read},
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
    run_inner("qemu-img", &command, Some((progress, stage)), None).map(|_| ())
}

fn qemu_percent(chunk: &str) -> Option<u8> {
    let (start, _) = chunk.trim().strip_prefix('(')?.split_once("/100%)")?;
    let value = start.trim().parse::<f32>().ok()?;
    (value.is_finite() && (0. ..=100.).contains(&value)).then_some(value.floor() as u8)
}

fn run_inner(
    program: &str,
    args: &[&str],
    progress: Option<(super::progress::Reporter, String)>,
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
    let out = thread::spawn(move || {
        if let Some((progress, stage)) = progress {
            let mut stdout = BufReader::new(stdout);
            let mut chunk = Vec::new();
            let mut last = None;
            while stdout.read_until(b'\r', &mut chunk).is_ok_and(|size| size > 0) {
                let value = qemu_percent(&String::from_utf8_lossy(&chunk));
                if let Some(value) = value.filter(|value| Some(*value) != last) {
                    progress.update(&stage, Some(value));
                    last = Some(value);
                }
                chunk.clear();
            }
            String::new()
        } else {
            let mut s = String::new();
            let _ = stdout.read_to_string(&mut s);
            s
        }
    });
    let err = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let timeout = if program == "curl"
        || program == "qemu-img"
        || program == "virt-clone"
        || args.iter().any(|a| ["managedsave", "snapshot-create-as", "snapshot-revert"].contains(a))
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
        assert_eq!(qemu_percent("qemu-img: Could not open disk"), None);
        assert_eq!(qemu_percent("(NaN/100%)"), None);
    }
}
