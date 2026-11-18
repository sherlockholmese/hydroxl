//! Convert a standalone virtual disk and publish the verified copy.
use super::{progress::Reporter, qemu_convert, run};
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path};

pub(crate) const FORMATS: &[(&str, &str)] = &[("qcow2", "QCOW2"), ("vmdk", "VMDK"), ("vdi", "VDI")];

pub(crate) fn convert(
    source: &Path,
    destination: &Path,
    format: &str,
    progress: &Reporter,
) -> Result<()> {
    progress.update("Checking source image", None);
    ensure!(FORMATS.iter().any(|(key, _)| *key == format), "Choose a supported output format.");
    let source = source.canonicalize().context("Could not find the source disk.")?;
    ensure!(source.is_file(), "Choose a regular disk image file.");
    let name = destination.file_name().context("Enter an output file name.")?;
    let parent =
        destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize().context("Choose an existing output folder.")?;
    let destination = parent.join(name);
    ensure!(!destination.exists(), "Choose a new output file name.");
    let text = |path: &Path| {
        path.to_str().map(str::to_owned).context("Choose a path with a valid UTF-8 name.")
    };
    let source = text(&source)?;
    let info: serde_json::Value =
        serde_json::from_str(&run("qemu-img", &["info", "--output=json", &source])?)?;
    let source_format =
        info["format"].as_str().context("Could not identify the source disk format.")?;
    ensure!(
        FORMATS.iter().any(|(key, _)| *key == source_format),
        "Choose a VMDK, QCOW2, or VDI source disk."
    );
    let work = tempfile::Builder::new()
        .prefix(".hydroxl-convert-")
        .tempdir_in(parent)
        .context("Could not create a temporary file in the output folder.")?;
    let staged = work.path().join(name);
    let staged_arg = text(&staged)?;
    let mut args = vec!["-f", source_format, "-O", format];
    if format == "vmdk" {
        args.extend(["-o", "subformat=monolithicSparse"]);
    }
    args.extend([source.as_str(), staged_arg.as_str()]);
    qemu_convert(&args, progress.clone(), "Writing converted image".into()).context(
        "Could not convert the disk. Shut down any guest using it and check available space.",
    )?;
    progress.update("Verifying converted image", None);
    run("qemu-img", &["compare", "-f", source_format, "-F", format, &source, &staged_arg])
        .context("The converted disk could not be verified.")?;
    fs::File::open(&staged)?.sync_all()?;
    // Both paths are on the same filesystem. Persist atomically and refuse
    // to overwrite an output created while conversion was running.
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(destination)
        .context("Could not save the converted disk. Choose a new file name.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires qemu-img and qemu-io"]
    fn converts_seeded_vmdk_through_qcow2_and_vdi() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.vmdk");
        let qcow = directory.path().join("converted.qcow2");
        let vdi = directory.path().join("converted.vdi");
        let source_arg = source.to_str().unwrap();
        run("qemu-img", &["create", "-f", "vmdk", source_arg, "2M"]).unwrap();
        run("qemu-io", &["-f", "vmdk", "-c", "write -P 0xa5 0 64k", source_arg]).unwrap();
        let progress = Reporter::default();
        let original = fs::read(&source).unwrap();
        convert(&source, &qcow, "qcow2", &progress).unwrap();
        convert(&qcow, &vdi, "vdi", &progress).unwrap();
        assert_eq!(original, fs::read(&source).unwrap());
        let output = fs::read(&vdi).unwrap();
        assert!(convert(&source, &vdi, "vdi", &progress).is_err());
        assert_eq!(output, fs::read(&vdi).unwrap());
        run("qemu-img", &["compare", "-f", "vmdk", "-F", "vdi", source_arg, vdi.to_str().unwrap()])
            .unwrap();
    }
}
