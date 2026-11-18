use super::support::{path_text, stamp};
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn apply_screenshot(c: Connection, machine: &Machine, path: PathBuf) -> Result<()> {
    let id = machine.id.as_str();
    screenshot(c, id, &path)?;
    Ok(())
}

pub(super) fn screenshot(c: Connection, id: &str, destination: &Path) -> Result<()> {
    let raw =
        std::env::temp_dir().join(format!("hydroxl-screen-{}-{}.ppm", std::process::id(), stamp()));
    let result = (|| -> Result<()> {
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&raw)?;
        backend::virsh(c, &["screenshot", id, "--file", path_text(&raw)?])?;
        let image = image::ImageReader::open(&raw)?
            .with_guessed_format()?
            .decode()
            .context("Could not read the display image.")?;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(destination)?;
        image
            .save_with_format(destination, image::ImageFormat::Png)
            .context("Could not save the screenshot.")?;
        Ok(())
    })();
    let _ = fs::remove_file(raw);
    result
}

/// Writes guest memory to `path` without stopping the guest. The file is created
/// private to the user because memory contents are sensitive.
pub(super) fn apply_memory_dump(
    c: Connection,
    machine: &Machine,
    path: PathBuf,
    format: String,
    progress: &backend::progress::Reporter,
) -> Result<()> {
    if !["elf", "kdump-zlib", "win-dmp"].contains(&format.as_str()) {
        anyhow::bail!("Choose a memory dump format.");
    }
    if path.exists() {
        anyhow::bail!("Choose a new file name for the memory dump.");
    }
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    let result = backend::virsh_with_progress(
        c,
        &["dump", &machine.id, path_text(&path)?, "--memory-only", "--format", &format],
        progress,
        "Saving memory dump",
    );
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result.map(|_| ())
}
