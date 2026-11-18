mod actions;
mod creation;
mod devices;
mod hardware;
mod information;
mod media;
mod networks;
mod snapshots;
pub use crate::backend::ARCHITECTURES;
use crate::{
    app::Job,
    backend::{Machine, NewMachine, State},
    forms::{FieldSpec as F, FormSpec, Picker, number},
    operations::{Operation as Op, Snapshot},
};
use anyhow::{Result, bail};
use std::path::PathBuf;
pub const BUSES: &[(&str, &str)] =
    &[("virtio", "VirtIO"), ("sata", "SATA"), ("scsi", "SCSI"), ("ide", "IDE"), ("usb", "USB")];
pub(super) fn name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 64 || value.chars().any(char::is_control) {
        bail!("Use a name between 1 and 64 characters.");
    }
    Ok(value.to_owned())
}
pub(super) fn operation(m: Machine, op: Op) -> Job {
    Job::Operation(m, op)
}

pub use actions::{MachineAction, action_form, direct_action, machine_actions};
pub use creation::{create, import, import_path, import_xml};
pub use devices::{DeviceAction, device_form, edit_share};
pub use hardware::{
    CPU_MODELS, GRAPHICS, HardwareDraft, NIC_MODELS, SOUND, USB_CONTROLLERS, VIDEO_MEMORY, label,
    machine_type_family, machine_types, xml,
};
pub use information::{architecture, confirm, operating_system};
pub use media::{compact, disk_bus, guest_tools, mount, resize};
pub use networks::{host_bridge, virtual_network};
pub use snapshots::{SnapshotAction, snapshot, snapshot_action};

pub(crate) fn convert_disk() -> FormSpec {
    let fields = vec![
        F::path("Source disk", Picker::Disk),
        F::choice("Output format", crate::backend::disk_convert::FORMATS, "qcow2"),
        F::path("Save as", Picker::ConvertedDisk),
    ];
    FormSpec::new("Convert disk", "Convert", fields, |values| {
        let source = PathBuf::from(values[0].trim());
        let format = values[1].clone();
        let destination = PathBuf::from(values[2].trim());
        if source.as_os_str().is_empty() || destination.as_os_str().is_empty() {
            bail!("Choose a source disk and an output file.");
        }
        let destination = if destination.extension().is_none() {
            destination.with_extension(&format)
        } else {
            destination
        };
        Ok(Job::ConvertDisk {
            source,
            destination,
            format,
        })
    })
}
