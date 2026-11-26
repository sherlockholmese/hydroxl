use anyhow::Context as _;
use std::{
    io::{self, Write},
    path::Path,
};

/// Stage beside the destination so publishing does not expose a partial file.
pub(crate) fn stage(destination: &Path) -> io::Result<tempfile::NamedTempFile> {
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty());
    tempfile::Builder::new().prefix(".hydroxl-").tempfile_in(parent.unwrap_or(Path::new(".")))
}

pub(crate) fn write_atomic(destination: &Path, data: &[u8]) -> io::Result<()> {
    let mut staged = stage(destination)?;
    staged.write_all(data)?;
    staged.as_file().sync_all()?;
    staged.persist(destination)?;
    Ok(())
}

pub(crate) fn path_text(path: &Path) -> anyhow::Result<&str> {
    path.to_str().context("Choose a path with a valid UTF-8 name.")
}
