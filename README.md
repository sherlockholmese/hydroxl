# Hydroxl

A native Linux QEMU/KVM workspace in Rust and GPUI Kit. A machine library,
focused display view, neutral shadcn-inspired controls, and concise copy.

**Implementation draft. Build and automated tests pass. Live desktop validation
is pending.** See [the feature matrix](docs/feature-parity.md).

## Run

```sh
cargo run --release
```

Isolated UI preview:

```sh
cargo run -- --demo
```

Open an import form with a file:

```sh
cargo run -- --import /path/to/appliance.ova
```

The file-open flow shows an import form before making changes. `--demo` never
runs libvirt commands; its lifecycle buttons only change isolated sample state.
Select a live connection in Preferences to leave preview mode. Set the
default VM folder in Preferences. The new-machine and OVA/OVF forms let you
choose a different location for an individual machine.

For desktop integration, install the launcher in `share/applications` and
`packaging/dev.hydroxl.Hydroxl.svg` in `share/icons/hicolor/scalable/apps`
under the same installation prefix as the binary.

## Requirements

- A current stable Rust toolchain and the Linux development libraries required
  by [GPUI Kit](https://gpui-kit.com/docs/installation/).
- Working Vulkan graphics, X11 or Wayland, and fonts.
- QEMU system emulators and `qemu-img`.
- libvirt's QEMU driver, a working user session or system connection, and `virsh`.
- `virt-install` for machine creation and OVA/OVF import.
- `virt-clone` for full clones.
- Hydroxl displays guests through local VNC or SPICE. SPICE needs the system
  SPICE Client GLib runtime. Accelerated SPICE also needs an EGL driver with
  DMA-BUF import support.
- Local peer-to-peer QEMU D-Bus displays are supported. See the
  [GPUI patch plan](docs/gpui-patch-plan.md) for the experimental Vulkan path.
- `nmcli` (NetworkManager) for host bridges; `osinfo-query` for the OS catalog.
- `passt` for shared-network port forwarding.
- `curl` for the explicit Windows guest-tools download action.
- A working desktop file-picker portal for native file and folder selection.
- UEFI firmware when selecting UEFI. Windows 11 also needs host TPM emulation
  support. Install `swtpm` and `libtpms` with TPM 2.0 support. Hydroxl checks
  libvirt’s advertised capabilities before creating a disk. For an x86-64 KVM
  guest, check the `<tpm>` section with:

  ```sh
  virsh -c qemu:///session domcapabilities --arch x86_64 --virttype kvm
  ```

- `xterm`, or `TERMINAL` set to a terminal executable accepting `-e`, for serial
  consoles.

Hydroxl does not install or start host services or invoke sudo. Host networking
changes only when you create a virtual network (libvirt, polkit) or a host bridge
(NetworkManager, polkit; user-session bridges are allowed through pkexec).
New machines and appliances use `qemu:///session`.
Existing system machines can be managed through `qemu:///system`.

## Import and export

**OVA / OVF:** choose Import machine, then OVA / OVF appliance. Imports one virtual
system, its CPU count, memory, disk attachments, network adapter count, and EFI
hint. VMDK disks, including stream-optimized and split extents, are converted to
independent QCOW2 files. Original appliances are kept unchanged. Common SHA-1,
SHA-256, and SHA-512 manifests are checked when present. A checksum is an
integrity check, not verification of the publisher's signature.

Architecture, acceleration, firmware, and name can be overridden in the form.
Guest network adapters become isolated user-mode shared interfaces. Imported
x86 disks use SATA for guest compatibility; ARM64 disks use VirtIO SCSI. The
importer does not install guest drivers or run a guest conversion agent. Guests
with hypervisor-specific drivers may require boot repair or guest tools.

Direct `.ovf` imports require the referenced disk files alongside the descriptor.
OVA archives may be plain tar or gzip-compressed tar. Multi-VM appliances,
backed VMDK chains, encrypted images, compressed individual disk files, external
file references, and OVF deployment scripts/properties are not supported.

**OVA export:** Machine actions → Export OVA. Exports the stopped machine's
current file-backed disks as stream-optimized VMDKs, an OVF descriptor, and a
SHA-256 manifest. Saved RAM and snapshots are outside the OVA export. Normalized
OVF hardware is used; advanced host passthrough and vendor-specific settings
are not portable. Non-x86 architecture metadata is included, but receiving
hypervisors must support that guest architecture.

**Libvirt XML:** choose Import machine, then Libvirt XML. Disk paths are used in
place. Existing names and UUIDs cannot be overwritten through import.

**Folder export:** Machine actions → Export. Writes XML and independent QCOW2
copies of the current disks. Snapshots, saved RAM, shared folders, removable
media, and TPM state are not bundled. Reattach installation media and shares as
needed. Keep exports at their chosen location; XML uses absolute paths.

`.utm` compatibility is intentionally excluded.

## Controls

- `Ctrl N`: new machine
- `F5`: refresh
- `Ctrl ,`: preferences
- `Ctrl Q`: quit the manager; guests keep running

Machine actions include shutdown, restart, pause/resume, save/discard state,
serial console, screenshots, autostart, rename, clone, export, disposable start,
notes, configuration cloning, storage relocation, and removal. Removal keeps
disks by default. **Delete disk files** permanently deletes eligible files in
Hydroxl-owned storage after removing the machine definition. NVRAM is kept.
Remove snapshots and saved state before removing a machine. Force stop and
snapshot restore require explicit confirmation.

Start and Resume open the guest display. The display reconnects across guest
restarts. Click the guest display to capture input and press `Ctrl Alt` to
release it. **Scale image to fit** keeps the current guest resolution.
**Resize guest to fit** asks the guest to match the viewer's size and requires
compatible guest display drivers or a SPICE agent. In fullscreen, move to the top edge
to reveal the toolbar or pin it to keep it visible.

Use **Convert disk** in the machine library's + menu to convert between VMDK,
QCOW2, and VDI. The tool verifies the new image and keeps the source. Select the
activity indicator in the sidebar to see progress while an operation runs.

Hardware, device changes, exports, cloning, and snapshot restoration require a
powered-off guest where indicated. Shared clipboard needs SPICE guest tools;
shared folders use a 9p mount tag. USB devices are listed from sysfs and use the permissions granted by libvirt. Advanced configuration edits the
native domain XML and preserves the VM UUID.

Snapshots use libvirt's internal snapshot support; writable disks must support
it, typically QCOW2. Snapshot display names are stored separately from their
stable engine identities. Renaming does not rewrite a disk's internal snapshot
name. Overwrite saves a replacement before removing the old snapshot.

Disposable mode uses separate QCOW2 overlays and a transient guest; it never
merges disk changes back. It currently rejects machines with NVRAM, TPM, shared
folders, and host passthrough. Overlay files remain in a private temporary
folder until host cleanup; stop the transient guest before removing them.

## Code layout

The entry point only starts the application. UI panes, form primitives,
workflow definitions, libvirt access, VM operations, and appliance handling
live in separate modules. See [the architecture notes](docs/architecture.md).

- `src/app/`: workspace state, preferences, command coordination
- `src/ui/`: individual panes and shared visual primitives
- `src/forms/`: field schema, editor state, rendering, and dialog lifecycle
- `src/workflows/`: typed user actions and domain-specific form definitions
- `src/backend/`: machine types, process boundary, and libvirt access
- `src/operations/`: hardware, storage, sharing, lifecycle, and snapshot handlers
- `src/appliance/`: archive extraction, OVF mapping, VMDK validation, manifests,
  import, and export
- `src/theme.rs`: semantic palette and geometry

Preferences live in `$XDG_CONFIG_HOME/hydroxl` and owned machine files in
`$XDG_DATA_HOME/hydroxl`, using the normal home-directory defaults when unset.
Long operations run off the UI thread. Errors preserve form input and the last
loaded library. Detailed process diagnostics go to stderr.

Disk compaction creates a compressed replacement and compares guest-visible data
before replacing the original. It requires a stopped guest without snapshots or
a saved state. Storage relocation activates independent copies in the chosen
folder and keeps original files for recovery. Configuration cloning allocates
empty disks with the original capacities and generates fresh identities.
