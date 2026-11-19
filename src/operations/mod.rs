mod architecture;
mod boot;
mod capture;
mod cloning;
mod devices;
mod disks;
mod display;
mod export;
mod guest;
mod images;
mod inspect;
mod lifecycle;
mod media;
mod metadata;
mod nesting;
mod networking;
mod platform;
mod removal;
mod sharing;
mod snapshots;
mod support;
mod types;
mod usb;
use crate::backend::{Connection, Machine};
use anyhow::Result;
pub use architecture::available as available_architectures;
pub use guest::{GuestOs, GuestSetup, ShareDriver, WINDOWS_COMMAND, linux_command};
pub use inspect::{details, has_agent_channel};
pub use platform::{NicChange, Platform, log_path, open_path, random_mac, read_log};
pub use types::{Details, Device, DisplaySettings, Operation, Snapshot};
pub use usb::host_usb;
pub(crate) use usb::toggle_live_usb;
pub fn execute(
    c: Connection,
    machine: &Machine,
    op: Operation,
    progress: &crate::backend::progress::Reporter,
) -> Result<()> {
    match op {
        Operation::Sequence(steps) => {
            if steps.iter().any(|step| matches!(step, Operation::NestedVirtualization(true))) {
                nesting::host_feature(machine)?;
            }
            steps.into_iter().try_for_each(|step| execute(c, machine, step, progress))
        }
        Operation::NestedVirtualization(enabled) => nesting::apply(c, machine, enabled),
        Operation::Resources {
            topology,
            memory_mib,
        } => crate::backend::configure(c, &machine.id, topology, memory_mib),
        Operation::BootOrder(order) => boot::apply_boot_order(c, machine, order),
        Operation::Platform(p) => platform::apply_platform(c, machine, p),
        Operation::Nic(nic) => platform::apply_nic(c, machine, nic),
        Operation::DiskBus {
            target,
            bus,
        } => platform::apply_disk_bus(c, machine, target, bus),
        Operation::AddSerial {
            kind,
            port,
        } => platform::apply_add_serial(c, machine, kind, port),
        Operation::ShowFiles => platform::apply_show_files(machine),
        Operation::Reset => crate::backend::virsh(c, &["reset", &machine.id]).map(|_| ()),
        Operation::OpenLog => platform::apply_open_log(c, machine),
        Operation::Notes(notes) => metadata::apply_notes(c, machine, notes),
        Operation::Firmware(uefi) => boot::apply_firmware(c, machine, uefi),
        Operation::CloneConfiguration(name) => cloning::apply_clone_configuration(c, machine, name),
        Operation::MoveStorage(path) => export::apply_move_storage(c, machine, path),
        Operation::CompactDisk(target) => images::apply_compact_disk(c, machine, target),
        Operation::AddCdrom {
            target,
            bus,
        } => media::apply_add_cdrom(c, machine, target, bus),
        Operation::GuestTools(target) => media::apply_guest_tools(c, machine, target),
        Operation::TritonDriver => media::apply_triton_driver(c, machine),
        Operation::SwitchToBundledQemu => crate::backend::qemu::switch_to_bundled(c, &machine.id),
        Operation::System {
            cpu,
            machine_type,
            boot,
        } => boot::apply_system(c, machine, cpu, machine_type, boot),
        Operation::Display {
            settings,
            local_dbus,
        } => display::apply_display(c, machine, settings, local_dbus),
        Operation::PortForward {
            protocol,
            host,
            guest,
        } => networking::apply_port_forward(c, machine, protocol, host, guest),
        Operation::DetachDevice(xml) => devices::apply_detach_device(c, machine, xml),
        Operation::RemovePortForward(xml) => networking::remove_port_forward(c, machine, &xml),
        Operation::Reboot => lifecycle::apply_reboot(c, machine),
        Operation::ForceStop => lifecycle::apply_force_stop(c, machine),
        Operation::SaveState => lifecycle::apply_save_state(c, machine),
        Operation::DiscardState => lifecycle::apply_discard_state(c, machine),
        Operation::Disposable => lifecycle::apply_disposable(c, machine),
        Operation::Autostart(enabled) => metadata::apply_autostart(c, machine, enabled),
        Operation::Rename(name) => metadata::apply_rename(c, machine, name),
        Operation::Clone(name) => cloning::apply_clone(c, machine, name),
        Operation::Export(destination) => export::apply_export(c, machine, destination),
        Operation::ExportOva(destination) => export::apply_export_ova(c, machine, destination),
        Operation::Remove {
            delete_disks,
        } => removal::apply_remove(c, machine, delete_disks),
        Operation::SnapshotCreate(name) => snapshots::apply_snapshot_create(c, machine, name),
        Operation::SnapshotRestore(name) => snapshots::apply_snapshot_restore(c, machine, name),
        Operation::SnapshotDelete(name) => snapshots::apply_snapshot_delete(c, machine, name),
        Operation::SnapshotRename {
            old,
            new,
        } => snapshots::apply_snapshot_rename(c, machine, old, new),
        Operation::SnapshotOverwrite(name) => snapshots::apply_snapshot_overwrite(c, machine, name),
        Operation::AttachDisk {
            path,
            target,
            bus,
        } => disks::apply_attach_disk(c, machine, path, target, bus),
        Operation::NewDisk {
            size_gib,
            target,
            bus,
        } => disks::apply_new_disk(c, machine, size_gib, target, bus),
        Operation::DetachDisk(target) => disks::apply_detach_disk(c, machine, target),
        Operation::MountIso {
            path,
            target,
        } => media::apply_mount_iso(c, machine, path, target),
        Operation::EjectIso(target) => media::apply_eject_iso(c, machine, target),
        Operation::ResizeDisk {
            target,
            size_gib,
        } => images::apply_resize_disk(c, machine, target, size_gib),
        Operation::Network {
            mode,
            source,
        } => networking::apply_network(c, machine, mode, source),
        Operation::Share {
            path,
            tag,
            readonly,
            driver,
        } => sharing::apply_share(c, machine, path, tag, readonly, driver),
        Operation::Architecture(arch) => architecture::apply(c, machine, arch),
        Operation::OperatingSystem(uri) => metadata::apply_operating_system(c, machine, uri),
        Operation::Guest(setup) => guest::apply_setup(c, machine, setup),
        Operation::AddAgentChannel => support::attach_xml(
            c,
            &machine.id,
            "<channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>",
        ),
        Operation::UsbAttach {
            bus,
            device,
        } => usb::apply_usb_attach(c, machine, bus, device),
        Operation::UsbDetach {
            bus,
            device,
        } => usb::apply_usb_detach(c, machine, bus, device),
        Operation::SaveXml(xml) => metadata::apply_save_xml(c, machine, xml),
        Operation::Console => lifecycle::apply_console(c, machine),
        Operation::Screenshot(path) => capture::apply_screenshot(c, machine, path),
        Operation::MemoryDump {
            path,
            format,
        } => capture::apply_memory_dump(c, machine, path, format, progress),
    }
}
