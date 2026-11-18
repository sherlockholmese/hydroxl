use super::archive::contained;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256, Sha512};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};
pub(crate) fn manifest(path: &Path) -> Result<()> {
    let parent = path.parent().context("The manifest folder is missing.")?;
    let contents = fs::read_to_string(path).context("Could not read the appliance manifest.")?;
    for line in contents.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (algorithm, rest) =
            line.split_once('(').context("The appliance manifest is invalid.")?;
        let (name, expected) = rest
            .rsplit_once(")=")
            .or_else(|| rest.rsplit_once(") ="))
            .context("The appliance manifest is invalid.")?;
        let file = contained(parent, name)?;
        let digest = match algorithm.trim().to_ascii_uppercase().as_str() {
            "SHA1" => {
                let mut h = sha1::Sha1::new();
                hash_file(&file, &mut h)?;
                format!("{:x}", h.finalize())
            }
            "SHA256" => {
                let mut h = Sha256::new();
                hash_file(&file, &mut h)?;
                format!("{:x}", h.finalize())
            }
            "SHA512" => {
                let mut h = Sha512::new();
                hash_file(&file, &mut h)?;
                format!("{:x}", h.finalize())
            }
            _ => bail!("The appliance manifest uses an unsupported checksum."),
        };
        if !digest.eq_ignore_ascii_case(expected.trim()) {
            bail!("An appliance checksum does not match. Download the appliance again.");
        }
    }
    Ok(())
}

pub(crate) fn hash_file(path: &Path, digest: &mut impl Digest) -> Result<()> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(())
}
