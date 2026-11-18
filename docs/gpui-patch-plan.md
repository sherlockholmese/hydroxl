# GPUI patches for guest display

Hydroxl uses a small set of local GPUI APIs for input capture and framebuffer
uploads. The patches target `gpui-pre`, `gpui-pre-linux`, and `gpui-pre-wgpu`
0.3.11. Cargo resolves them from `vendor/` through `[patch.crates-io]`.
GPUI Kit and `gpui-pre-platform` use their published sources.

## Input capture

Start and Resume open the guest display after libvirt accepts the request.
The display window reconnects when the guest restarts. Click the display to
capture input. Ctrl+Alt, focus loss, and window close release capture and clear
forwarded keys and mouse buttons. The viewer uses its own cursor hiding policy
so the host cursor returns immediately on release.

In fullscreen, the toolbar overlays the frame and appears at the top edge.
The pin keeps it visible. F11 toggles fullscreen while input is released.

`Window::set_cursor_hidden` hides the host cursor through a null Wayland cursor
or XFixes. `Window::lock_pointer` requests capture for guests that report
relative input. Wayland uses pointer constraints and relative-pointer events.
X11 confines the pointer with a grab and reads raw XInput2 motion. The app takes
the accumulated delta through `Window::take_raw_mouse_motion`. Absolute tablet
input uses the displayed frame's bounds in logical pixels.
`Window::inhibit_system_shortcuts` uses the Wayland shortcut inhibitor or an X11
keyboard grab. Compositor policy and available protocols determine what the
host can capture. Basic VNC input remains available when an extension fails.

Automated tests cover relative motion packets, release, and coordinate mapping at
1.25×, 1.5×, and 1.75×. Live checks on KDE, GNOME, and X11 are still pending.
For those checks, use both absolute and relative guest input in windowed and
fullscreen modes. Check cursor restoration when the pointer leaves and reenters,
Ctrl+Alt release, focus loss, and recapture. `HYDROXL_DEBUG_POINTER=1` logs the
viewport, scale, and fitted frame bounds for diagnosing offsets.

## Damage uploads

The VNC reader accumulates damaged rectangles. `Window::update_texture` updates
a window-owned BGRA texture; `update_texture_with_stride` also accepts borrowed
framebuffer rows without packing intermediate pixel buffers. Both validate all
bounds and source bytes before mutation and retain no producer pointer. The
viewer uses the strided path while holding the framebuffer lock, including
borrowing full pixels for texture creation instead of cloning them first.
The viewer computes a single viewport for painting,
cursor scaling, and input mapping across the scaling modes. Guest resize mode
requests a resolution change through RFB ExtendedDesktopSize or the SPICE agent.
`Window::paint_texture_in` uses those bounds to draw and clip without another
scale calculation. The wgpu atlas writes changed regions with
`queue.write_texture`. A CPU copy supports renderer recovery. A resize or enough
accumulated damage to exceed a full frame triggers a full upload. Tests cover
rectangle bounds, resize, overlapping CopyRect, and the headless texture API.

## SPICE and Phase 3

Hydroxl attaches through libvirt and uses SPICE Client GLib on a worker thread.
Software frames and damaged rectangles from SPICE use the same GPUI texture
path as VNC. SPICE provides audio, manual USB redirection, and text clipboard
actions. The connection is reestablished after a guest restart.

Software invalidations accumulate bounded regions on the decoding thread;
adjacent strips merge without collapsing distant changes into one huge copy.
Visible SPICE uploads run on GPUI's platform frame callbacks, with one pending
callback per session and a 50 ms hidden/show housekeeping timer. Each callback
requests the next asynchronous owned snapshot; it is not a synchronous read of
the decoder's canvas at paint time. Snapshot publication still has no atomic
Windows-present fence. See [the audit and validation limits](spice-2d-tearing.md).

SPICE GL imports scanout buffers with EGL, reads changed pixels into CPU memory,
then uploads them through the GPUI damage path. Hydroxl acknowledges a draw
after the readback finishes. Set `HYDROXL_PROFILE_SPICE=1` to log mean and p95
readback time and MiB per frame every 120 frames. These timings cover readback
and color conversion. Test GUI smoothness and frame time separately.

Phase 3 adds peer-to-peer QEMU Display1 and a Vulkan DMA-BUF copy shared by
SPICE GL and D-Bus. The renderer waits for the producer's DMA-BUF write fence,
acquires the image from the foreign queue family, and copies it into its own
atlas texture. It returns ownership and waits for the submission before
acknowledging the producer. The guest surface stays on the GPU for normal
display updates. An occasional overview preview can still use EGL readback.

Enable the experimental Vulkan path with `HYDROXL_VULKAN_SCANOUT=1`.
It requires a compatible Vulkan device, foreign queue ownership support, and
a single-plane RGB scanout with a supported modifier. Other scanouts use EGL
readback. For an existing local D-Bus domain, Hydroxl attaches when the libvirt
graphics element is `<graphics type='dbus' p2p='yes'/>`. Accelerated domains add
the `<gl enable='yes'/>` child. Existing SPICE domains can exercise the Vulkan
path without changing their graphics configuration.

Automated checks cover listener signatures, damaged software rectangles, EGL
readback, and unsupported renderer behavior. A live DRM render node and QEMU
session are needed to verify hardware import, orientation, restart behavior,
and timing before enabling the Vulkan path by default.

## Maintaining the patches

The original crate checksums and the per-crate diffs are in `patches/gpui/`.
Normal builds use the checked-in trees. To work on the patch set, use Python
3.11 or newer and GNU `patch`:

```sh
python3 scripts/gpui-vendor.py check
python3 scripts/gpui-vendor.py refresh
python3 scripts/gpui-vendor.py rebuild
```

`check` compares the trees against the pinned crates with the diffs applied.
`refresh` regenerates the diffs from local vendor edits. `rebuild` replaces
the vendored trees after validating the patch set. The script uses cached
crates by default. Pass `--download` if a pinned crate is missing from the cache.

When upgrading GPUI Kit, update the crate pins and checksums, reapply the diffs
to the new release, and regenerate the lockfile. Run the patch check, `cargo
check --locked`, and `cargo test --locked` before carrying the update forward.
