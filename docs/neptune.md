# Bundled Neptune QEMU and Triton

Hydroxl builds QEMU 11.1.3 with the Neptune device support ported from UTM. The
bundle includes virglrenderer, DXVK for Direct3D 11, vkd3d-proton for Direct3D 12,
and the signed Triton v0.6 Windows drivers. Neptune carries Direct3D commands,
and Venus carries Vulkan commands. Triton is the Windows driver. Hydroxl
displays the guest through its existing embedded viewer.

## Build and run

On Arch Linux, prepare the build dependencies from the package recipe once:

```sh
sudo pacman -S --needed base-devel
(cd packaging/arch && makepkg --syncdeps --nobuild)
```

From the checkout, either Cargo build profile builds the bundle automatically:

```sh
cargo run
cargo build --release
```

The first build downloads the [pinned sources](../scripts/neptune-sources.txt)
and compiles the graphics libraries and QEMU. Debug and release reuse the same
native build for the same Cargo target. Use `cargo build -vv` to see the native
build output. The development executable uses
`target/debug/hydroxl-neptune` with the default Cargo target directory. The Arch
package installs its bundle under `/usr/lib/hydroxl/neptune`. You can set
`HYDROXL_QEMU_ROOT` to the absolute path of a different build of the bundle.

To build only the native bundle and driver discs, use the same script Cargo runs:

```sh
scripts/build-neptune.sh "$PWD/target/debug/hydroxl-neptune" "$PWD/target/debug/hydroxl-neptune-build"
target/debug/hydroxl-neptune/bin/qemu-system-x86_64 -device virtio-vga-gl,help
target/debug/hydroxl-neptune/bin/qemu-img --version
```

The device help should list both `neptune` and `venus`. The build keeps the
system QEMU installation intact. System QEMU packages provide libvirt and
virt-install with architecture discovery. Bundled Neptune QEMU is the default
in New Machine and both import forms. Choose **System QEMU** there to use the
distro emulator for that VM. Hydroxl uses the bundled `qemu-img` for its direct
disk operations.

The VM **Overview** shows the emulator recorded by libvirt. For a powered-off
system QEMU VM, choose **Use bundled** in that row, or **Switch** in the Triton
checklist under **Hardware → Guest tools**. Resume any saved
state and shut down the guest first. The switch keeps the VM's disks and machine
type.

## Enable DirectX in Windows 11

1. Boot Windows with **VGA** or **QXL** graphics. In Hydroxl, open **Hardware →
   Guest tools** and choose **Mount** in the Triton driver checklist. Hydroxl selects the disc for the guest
   architecture.
2. Run `Install-Triton.cmd` from the disc and approve the Windows administrator
   prompt. The installer removes any legacy VirtIO display-only driver and
   adds the signed Triton package. If the old driver is running, restart
   Windows when prompted so setup can finish in the background.
3. Shut down Windows. Choose **Turn on** in the checklist, or in **Hardware →
   Display and sound** choose **VirtIO** graphics and turn on **DirectX**. Save
   the changes.
4. Start the VM from Hydroxl. Confirm **Red Hat VirtIO GPU D3D controller**
   appears under **Display adapters** in Device Manager. Check the feature
   levels in `dxdiag` and run a Direct3D application to verify acceleration.

Local SPICE GL is the default accelerated display. An existing peer-to-peer
QEMU D-Bus display can also carry the accelerated scanout. The Linux host needs
a Vulkan GPU, an EGL render node accessible to QEMU, and access to
`/dev/udmabuf` for shared buffers. Hydroxl leaves the guest's Secure Boot and
TPM settings in place. The bundled QEMU starts its private renderer process
before libvirt's QEMU seccomp filters take effect and passes it a private socket.

## Recover a black display

Power off the VM, disable DirectX, and select **QXL** or **VGA** for a recovery
boot and run `Install-Triton.cmd` again. The setup log is at
`%SystemRoot%\Logs\Hydroxl-Triton.log`.

If the accelerated display is still black, launch Hydroxl with
`HYDROXL_DEBUG_DISPLAY=1 cargo run` before the next VM start. The VM log records
the first shared framebuffer QEMU accepts as a Neptune scanout. The terminal
reports when the embedded SPICE viewer receives a new buffer layout and when
its full-frame readback changes between black and visible pixels.
