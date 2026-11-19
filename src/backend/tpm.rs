//! Validate TPM emulation against the selected libvirt connection, not the UI host.
use super::{Connection, virsh};
use anyhow::{Context, Result, ensure};

pub(crate) const UNAVAILABLE: &str = "TPM 2.0 emulation is unavailable on this libvirt connection. If you just installed swtpm and libtpms, reconnect to libvirt after its daemon restarts.";

pub(crate) fn emulator_model(
    connection: Connection,
    arch: &str,
    virt_type: &str,
    machine: Option<&str>,
    emulator: Option<&str>,
) -> Result<String> {
    let mut args = vec!["domcapabilities", "--arch", arch, "--virttype", virt_type];
    if let Some(emulator) = emulator {
        args.extend(["--emulatorbin", emulator]);
    }
    if let Some(machine) = machine.filter(|s| !s.is_empty()) {
        args.extend(["--machine", machine]);
    }
    let xml = virsh(connection, &args)
        .context("Could not check TPM support on the selected libvirt connection.")?;
    model_from_capabilities(&xml, arch).map(str::to_owned)
}

fn model_from_capabilities<'a>(xml: &'a str, arch: &str) -> Result<&'a str> {
    let doc =
        roxmltree::Document::parse(xml).context("Libvirt returned invalid domain capabilities.")?;
    let tpm = doc
        .descendants()
        .find(|n| n.has_tag_name("tpm") && n.attribute("supported") == Some("yes"))
        .context(UNAVAILABLE)?;
    let supports = |name: &str, value: &str| {
        tpm.children()
            .filter(|n| n.has_tag_name("enum") && n.attribute("name") == Some(name))
            .flat_map(|n| n.children())
            .any(|n| n.has_tag_name("value") && n.text() == Some(value))
    };
    ensure!(supports("backendModel", "emulator") && supports("backendVersion", "2.0"), UNAVAILABLE);
    let preferred = if matches!(arch, "x86_64" | "i686") {
        ["tpm-crb", "tpm-tis"]
    } else {
        ["tpm-tis", "tpm-crb"]
    };
    for model in preferred {
        if supports("model", model) {
            return Ok(model);
        }
    }
    anyhow::bail!("This machine type does not support a compatible TPM 2.0 device.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_emulator_version_and_compatible_model() {
        let xml = "<domainCapabilities><devices><tpm supported='yes'><enum name='model'><value>tpm-crb</value><value>tpm-tis</value></enum><enum name='backendModel'><value>emulator</value></enum><enum name='backendVersion'><value>2.0</value></enum></tpm></devices></domainCapabilities>";
        assert_eq!(model_from_capabilities(xml, "x86_64").unwrap(), "tpm-crb");
        assert_eq!(model_from_capabilities(xml, "aarch64").unwrap(), "tpm-tis");
        for bad in [
            xml.replace("2.0", "1.2"),
            xml.replace("emulator", "passthrough"),
            xml.replace("supported='yes'", "supported='no'"),
        ] {
            assert!(
                model_from_capabilities(&bad, "x86_64").unwrap_err().to_string().contains("swtpm")
            );
        }
    }
}
