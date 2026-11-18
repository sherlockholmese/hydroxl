use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
};
fn relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.contains(['\\', ':', '\0'])
        || path.as_os_str().is_empty()
        || path.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("The appliance references a file outside its folder.");
    }
    Ok(path)
}

pub(crate) fn contained(base: &Path, value: &str) -> Result<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .context("The appliance contains an invalid file name.")?;
    contained_literal(base, &decoded)
}

pub(super) fn contained_literal(base: &Path, value: &str) -> Result<PathBuf> {
    let base = base.canonicalize()?;
    let path = base
        .join(relative(value)?)
        .canonicalize()
        .context("An appliance disk or referenced file is missing.")?;
    if !path.starts_with(base) || !path.is_file() {
        bail!("The appliance references a file outside its folder.");
    }
    Ok(path)
}

pub(crate) fn extract(
    source: &Path,
    folder: &Path,
    progress: &crate::backend::progress::Reporter,
) -> Result<Vec<PathBuf>> {
    let mut file = File::open(source).context("Could not open the OVA file.")?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic).context("The OVA file is empty.")?;
    file.rewind()?;
    let total = file.metadata()?.len();
    let file = ArchiveProgress {
        file,
        total,
        consumed: 0,
        last: 0,
        progress: progress.clone(),
    };
    progress.update("Extracting archive", Some(0));
    let reader: Box<dyn Read> = if magic == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut archive = tar::Archive::new(reader);
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for entry in archive.entries().context("Choose a valid OVA archive.")? {
        let mut entry = entry.context("The OVA archive is damaged.")?;
        let path = entry.path().context("An archive file name is invalid.")?.into_owned();
        if path.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            bail!("The OVA archive contains an unsafe path.");
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(folder.join(&path))?;
            continue;
        }
        if !kind.is_file() {
            bail!("The OVA archive contains an unsupported link or special file.");
        }
        let destination = folder.join(&path);
        if !seen.insert(destination.clone()) {
            bail!("The OVA archive contains duplicate file names.");
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        // Ignore archived permissions, ownership, and executability.
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&destination)?;
        std::io::copy(&mut entry, &mut output)
            .context("Could not extract the appliance. Check available disk space.")?;
        files.push(destination);
    }
    Ok(files)
}

/// Count source bytes, so gzip extraction reports compressed bytes consumed
/// instead of comparing expanded disk sizes to the archive length.
struct ArchiveProgress {
    file: File,
    total: u64,
    consumed: u64,
    last: u8,
    progress: crate::backend::progress::Reporter,
}
impl Read for ArchiveProgress {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        self.consumed += n as u64;
        let percent =
            self.consumed.saturating_mul(100).checked_div(self.total).unwrap_or(0).min(100) as u8;
        if self.last != percent {
            self.last = percent;
            self.progress.update("Extracting archive", Some(percent));
        }
        Ok(n)
    }
}
