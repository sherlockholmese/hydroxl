use super::support::{editable, temporary_xml};
use crate::backend::{self, Connection, Machine};
use anyhow::Result;

pub(super) fn apply_detach_device(c: Connection, machine: &Machine, xml: String) -> Result<()> {
    let id = machine.id.as_str();
    editable(c, id)?;
    temporary_xml(&xml, |path| {
        backend::virsh(c, &["detach-device", id, path, "--config"]).map(|_| ())
    })?;
    Ok(())
}
