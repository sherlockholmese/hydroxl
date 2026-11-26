use super::support::path_text;
use crate::backend::{self, Connection, Machine};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

pub(super) fn apply_screenshot(c: Connection, machine: &Machine, path: PathBuf) -> Result<()> {
    let id = machine.id.as_str();
    screenshot(c, id, &path)?;
    Ok(())
}

pub(super) fn screenshot(c: Connection, id: &str, destination: &Path) -> Result<()> {
    let raw = tempfile::NamedTempFile::new().context("Could not create a temporary screenshot.")?;
    backend::virsh(c, &["screenshot", id, "--file", path_text(raw.path())?])
        .context("Could not capture the guest display.")?;
    let image = image::ImageReader::open(raw.path())?
        .with_guessed_format()?
        .decode()
        .context("Could not read the captured display image.")?;
    let mut staged = crate::files::stage(destination)
        .context("Could not create a screenshot file in the chosen folder.")?;
    image
        .write_to(staged.as_file_mut(), image::ImageFormat::Png)
        .context("Could not encode the screenshot as PNG.")?;
    staged.as_file().sync_all().context("Could not finish writing the screenshot.")?;
    staged
        .persist(destination)
        .context("Could not save the screenshot. Check the output folder.")?;
    Ok(())
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
    ensure!(!path.try_exists()?, "Choose a new file name for the memory dump.");
    let staged = crate::files::stage(&path)
        .context("Could not create the memory dump in the chosen folder.")?;
    backend::virsh_with_progress(
        c,
        &["dump", &machine.id, path_text(staged.path())?, "--memory-only", "--format", &format],
        progress,
        "Saving memory dump",
    )
    .context("Could not capture guest memory.")?;
    staged.as_file().sync_all().context("Could not finish writing the memory dump.")?;
    staged.persist_noclobber(&path).context(
        "Could not save the memory dump. Check the output folder and choose an unused file name.",
    )?;
    Ok(())
}
