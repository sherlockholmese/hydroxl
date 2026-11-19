# UTM feature coverage

Scope: equivalent local QEMU VM workflows on Linux using KVM where possible.
`.utm` bundles are excluded by request. OVA/OVF import and OVA export are added
explicit requirements. Apple Virtualization, macOS recovery/IPSW installation,
Rosetta, Apple-only integrations, and the separate UTM Remote/Server product are
outside this Linux local-manager scope.

All entries below describe source implementation. `cargo build --locked` passes;
runtime behavior and visual presentation have not been verified. Do not treat
this matrix as a release claim.

| Workflow | Implementation | Limits / remaining work |
| --- | --- | --- |
| Library, search, selection | GPUI library/detail workspace | No drag ordering or folders |
| Create machine | Guided wizard; ISO, network boot or no media; new or existing disk; chosen location; full libosinfo catalog | User session; host-supported QEMU architectures |
| Virtualize / emulate | KVM or QEMU selected explicitly | Installed emulator and libvirt capabilities required |
| Architectures | x86-64, x86, ARM64, ARM, RISC-V 64, PPC, PPC64 choices | UTM's entire architecture catalog is not exposed |
| BIOS / UEFI | Creation, import, and inline toggle in Hardware | Firmware change may need guest bootloader repair |
| OVA / OVF import | Single-system appliance parser, multi-disk VMDK conversion | See README for supported appliance subset |
| OVA export | OVF, stream-optimized VMDKs, SHA-256 manifest | Current disks and portable hardware; no RAM or snapshots |
| XML import | Existing native libvirt machine definitions | Existing name/UUID overwrite refused |
| Start / shutdown / force stop | libvirt actions | Shutdown is a request; live state remains authoritative |
| Pause / resume | libvirt suspend/resume | Host support required |
| Save / discard state | libvirt managed save | Restore through Start / Resume |
| Disposable mode | Transient guest with fresh disk overlays and MAC addresses | Restricted to guests without externally writable state |
| Graphical display | Per machine, choose VNC or SPICE in the GPUI viewer. Includes keyboard and pointer forwarding and special keys. | SPICE provides audio and USB redirection. SPICE GL uses EGL readback. 2D SPICE displays can tear at high resolutions; see [SPICE tearing](spice-2d-tearing.md). |
| Display screenshot | Actual intermittent guest capture, PNG save | Captured frame is labeled; never presented as live video |
| Fullscreen / resolution / keyboard capture | In-app fullscreen, guest input capture, Ctrl+Alt release | Host shortcuts depend on compositor support |
| Clipboard sharing | Automatic both ways over SPICE and VNC: guest copies reach the host as they happen; host text is offered when the display window regains focus or captures input. "Copy guest clipboard" copies on demand. "Type clipboard text" types into any guest. With VNC the guest agent channel is connected to QEMU's `qemu-vdagent` | spice-vdagent required in the guest for sync; typing uses a US layout. VNC clipboard is text only |
| Shared folders | virtio-fs (default, needs host virtiofsd, required for Windows) or 9p. Edit or stop sharing per folder. Setup in the guest through the guest agent, keyboard automation on Windows, or copy / type commands. Linux entries are added to fstab with automount | Windows needs guest tools and WinFsp (installed automatically when the guest has network) |
| Architecture change | Stopped machines, from libvirt's available emulators, with confirmation | Machine type, firmware and controllers reset; the installed system must support the new architecture |
| Operating system | Set from the libosinfo catalog in libvirt metadata | |
| Guest agent | Channel added to new machines; one action adds it to existing ones | qemu-guest-agent in the guest |
| Multiple displays | Configurable video heads | Guest, video model and viewer support required |
| Graphics / 3D | VirtIO, QXL, VGA, Bochs, RAM framebuffer. Windows 11 DirectX through the bundled Neptune QEMU and signed Triton driver disc | Requires VirtIO and host graphics support. Windows acceleration is experimental. See [Neptune setup](neptune.md). |
| Sound | HD Audio, AC97, AudioPCI, SB16 over SPICE | Model support depends on guest and architecture |
| Serial console | virsh console in configured terminal | External terminal; advanced serial device setup uses XML |
| CPU / RAM | Inline sockets, cores, threads; memory in MB | Powered-off configuration changes |
| CPU model / machine / boot order | Inline CPU model, machine type, drag-and-drop per-device boot order | Architecture fixed after creation |
| Add / attach / detach disk | New QCOW2 or existing supported QEMU image | Stable target and bus chosen explicitly |
| Resize disk | Offline growth with size validation | Shrinking excluded; partition resize remains a guest task |
| Compress disk | Offline QCOW2 compaction with data comparison | Requires free space and no snapshots |
| Removable media | Empty CD/DVD drive creation, mount / eject ISO, change interface | |
| Network modes | Per-card Shared / Virtual network / Bridged, model, MAC, isolation; create NAT or isolated virtual networks and NetworkManager host bridges | Virtual networks need the System connection; wireless interfaces cannot be bridged |
| Port forwarding | TCP/UDP rules through passt | Localhost only; rules can be removed from Devices |
| Shared folders | Read-only or writable 9p directory sharing | WebDAV and virtiofs convenience setup not implemented |
| USB passthrough | Host-device picker with connect/disconnect | Native select populated from host sysfs; permissions remain authoritative |
| Snapshots | Create, restore, delete, rename, overwrite, parent label | Internal snapshots; no graph or external-chain tooling |
| Snapshot overwrite | Replacement first, old state removed afterward | Partial failure retains a recoverable saved replacement |
| Rename / autostart | Dedicated actions | Persistent domains |
| Full clone | virt-clone copies disks and regenerates identities | Powered-off machine; installed virt-clone required |
| Configuration-only clone | New identities and blank disks of matching capacities | Powered off; custom TPM paths excluded |
| Move machine files | Independent storage copies activated at the destination | Snapshots must be removed; originals retained for recovery |
| Folder export | Native XML and independent current disks | Not a full snapshot/TPM/removable-media backup |
| Remove from library | Confirmation with optional deletion of Hydroxl-owned disks | Keeps disks by default and preserves NVRAM |
| Per-machine notes | Domain description editor | Shown only when notes are present |
| Custom QEMU configuration | Native advanced XML editor | Expert path; dedicated forms do not cover every QEMU flag |
| TPM 2.0 | Windows 11 creation and hardware toggle check libvirt capabilities | Requires TPM 2.0 emulation on the selected connection |
| Input / USB | USB 3.0 / 2.0 / none controller, USB tablet | |
| QEMU devices | Memory balloon, entropy device, Hyper-V enlightenments, local-time clock | Further QEMU flags through XML |
| Video memory | Inline for QXL, VGA and Bochs | |
| Disk interface | Change bus of an existing disk or drive | Guest drivers must support the new bus |
| Serial ports | Add pseudo-terminal or localhost TCP server ports | |
| Custom icon | Per-machine icon from the OS mark set | |
| Show files / log | Opens the machine folder or QEMU log in the desktop | System logs may need permission |
| Memory dump | `virsh dump --memory-only` as ELF, kdump or Windows crash dump | Running or paused guest |
| Guest tools | One-click VirtIO ISO download (cached), auto CD/DVD selection or creation, mount | Guest installation remains a guest action; SPICE agent may need separate installation |
| Themes / narrow desktop | shadcn theme, light and dark, narrow layout; screenshots reviewed | Keyboard-only and screen-reader behavior unverified |

Full feature parity is not complete. Remaining implementation gaps are explicit
above; runtime verification against libvirt remains outstanding for the settings above; the VNC client was tested against a live QEMU. Continue from these entries
before describing Hydroxl as a complete UTM replacement.

Reference inventory:

- [GPUI patch plan](gpui-patch-plan.md) for the remaining display limits
- [UTM actions](https://docs.getutm.app/basics/actions/)
- [UTM controls](https://docs.getutm.app/basics/controls/)
- [UTM system settings](https://docs.getutm.app/settings-qemu/system/)
- [UTM sharing](https://docs.getutm.app/settings-qemu/sharing/)
- [UTM snapshots](https://docs.getutm.app/advanced/snapshots/)
- [libvirt domain configuration](https://libvirt.org/formatdomain.html)
- [libvirt command reference](https://www.libvirt.org/manpages/virsh.html)
- [QEMU image conversion](https://www.qemu.org/docs/master/tools/qemu-img.html)
- [DMTF OVF specification](https://www.dmtf.org/sites/default/files/standards/documents/DSP0243_2.1.0.pdf)
