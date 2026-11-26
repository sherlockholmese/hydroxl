mod catalog;
mod creation;
pub(crate) mod disk_convert;
mod display;
mod gpu;
mod hardware;
mod library;
mod lifecycle;
mod model;
pub mod network;
mod process;
pub(crate) mod progress;
pub(crate) mod qemu;
mod spice;
pub(crate) mod tpm;
pub use catalog::{family_of, infer_os, os_by_uri, os_catalog, os_families, os_name, os_versions};
pub use creation::machines_dir;
pub(crate) use creation::{
    ImportHardware, OwnedStorage, STORAGE_RECORD, add_hardware_review, data_dir, define_xml,
    import_hardware, record_owned_storage,
};
pub use creation::{create, disable_cow, import};
pub use display::ensure_live_tablet;
pub(crate) use display::{DisplayTarget, SpiceOptions, display_target, is_peer_dbus};
pub use gpu::{
    HOSTMEM_BYTES, NEPTUNE_HOST_MISSING, TRITON_ARCH_UNSUPPORTED, gpu_features, triton_arch,
};
pub(crate) use hardware::MEMORY_MIB;
pub use hardware::{Topology, configure, nested_host_feature};
pub(crate) use library::{MachineList, MachineStatuses, machines};
pub use library::{demo_machines, virsh};
pub use lifecycle::action;
pub use model::{ARCHITECTURES, Action, Connection, Machine, NewMachine, State};
pub(crate) use process::{
    SnapshotNotFound, launch_desktop, qemu_convert, run, run_with_input, virsh_with_progress,
};
pub use qemu::{QemuBackend, QemuBuild};
pub(crate) use spice::{
    host_audio_backend, is_agent_channel, peer_clipboard, remove_vnc_for_spice_gl,
    route_agent_clipboard,
};

pub(crate) use lifecycle::ensure_stopped;
