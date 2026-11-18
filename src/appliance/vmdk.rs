use super::archive::contained_literal;
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
pub(crate) fn validate_vmdk(path: &Path, base: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut header = [0u8; 512];
    let read = file.read(&mut header)?;
    let embedded = read >= 44 && &header[..4] == b"KDMV";
    let descriptor = if embedded {
        let offset = u64::from_le_bytes(header[28..36].try_into()?);
        let sectors = u64::from_le_bytes(header[36..44].try_into()?);
        let bytes = sectors.checked_mul(512).context("The VMDK descriptor is invalid.")?;
        let offset = offset.checked_mul(512).context("The VMDK descriptor is invalid.")?;
        if bytes == 0
            || bytes > 2 * 1024 * 1024
            || offset.checked_add(bytes).is_none_or(|n| n > length)
        {
            bail!("The VMDK descriptor is missing or invalid.");
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut data = vec![0u8; bytes as usize];
        file.read_exact(&mut data)?;
        String::from_utf8_lossy(&data).into_owned()
    } else {
        if length > 2 * 1024 * 1024 {
            bail!("The appliance disk is not a supported VMDK image.");
        }
        file.rewind()?;
        let mut data = String::new();
        file.read_to_string(&mut data)
            .context("The appliance disk is not a supported VMDK image.")?;
        if !data.contains("Disk DescriptorFile") {
            bail!("The appliance disk is not a supported VMDK image.");
        }
        data
    };
    let lines = descriptor.split('\0').next().unwrap_or_default().lines().map(str::trim);
    if lines.clone().any(|line| line.starts_with("parentFileNameHint")) {
        bail!("Flatten the appliance's VMDK backing chain before importing.");
    }
    let extents: Vec<_> = lines
        .clone()
        .filter(|line| matches!(line.split_whitespace().next(), Some("RW" | "RDONLY" | "NOACCESS")))
        .collect();
    let mut create_types = lines
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| key.trim() == "createType")
        .map(|(_, value)| value.trim());
    let self_contained = embedded
        && extents.len() == 1
        && extents[0].split_whitespace().nth(2) == Some("SPARSE")
        && matches!(create_types.next(), Some("\"streamOptimized\"" | "\"monolithicSparse\""))
        && create_types.next().is_none();
    for line in extents {
        if let Some((_, rest)) = line.split_once('"') {
            let (name, _) = rest.split_once('"').context("A VMDK extent path is invalid.")?;
            // In these layouts QEMU reads the extent from the file containing the descriptor.
            if self_contained {
                continue;
            }
            let extent = contained_literal(path.parent().unwrap_or(base), name)?;
            if !extent.starts_with(base.canonicalize()?) {
                bail!("A VMDK extent points outside the appliance.");
            }
        } else if line.split_whitespace().nth(2) != Some("ZERO") {
            bail!("The VMDK uses an unsupported disk extent.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn accepts_a_renamed_embedded_sparse_extent() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("appliance-disk.vmdk");
        for kind in ["streamOptimized", "monolithicSparse"] {
            let mut image = [0u8; 1024];
            image[..4].copy_from_slice(b"KDMV");
            image[28..36].copy_from_slice(&1u64.to_le_bytes());
            image[36..44].copy_from_slice(&1u64.to_le_bytes());
            let descriptor = format!(
                "# Disk DescriptorFile\nparentCID=ffffffff\ncreateType=\"{kind}\"\nRDONLY 128 SPARSE \"original.vmdk\"\n"
            );
            image[512..512 + descriptor.len()].copy_from_slice(descriptor.as_bytes());
            fs::write(&path, image)?;
            validate_vmdk(&path, folder.path())?;
        }
        Ok(())
    }

    #[test]
    fn validates_external_extent_paths_literally() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("disk.vmdk");
        let descriptor = |name: &str| {
            format!(
                "# Disk DescriptorFile\ncreateType=\"monolithicFlat\"\nRW 128 FLAT \"{name}\" 0\n"
            )
        };
        fs::write(&path, descriptor("disk%20flat.vmdk"))?;
        fs::write(folder.path().join("disk flat.vmdk"), [])?;
        assert!(validate_vmdk(&path, folder.path()).is_err());
        fs::write(folder.path().join("disk%20flat.vmdk"), [])?;
        validate_vmdk(&path, folder.path())?;
        fs::write(&path, descriptor("../disk-flat.vmdk"))?;
        assert!(validate_vmdk(&path, folder.path()).is_err());
        Ok(())
    }
}
