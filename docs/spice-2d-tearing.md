# SPICE tearing on 2D displays

Status: **user-confirmed fixed in the tested Windows setup** after the
scheduling/publication and rendering improvements. Investigation updated
2026-11-18. **The source-supported failure
mechanism is presentation of an incomplete software-display update, amplified by
QEMU's strip-based SPICE updates and Hydroxl's throttled GLib dispatch at high
resolutions.** It is not evidence that Windows needs guest 3D acceleration, or
that host monitor VSync is disabled.

The earlier investigation recorded tearing with Windows 11, VirtIO VGA (2D, no 3D
acceleration), and the SPICE viewer at high resolution; the same guest over VNC did
not tear. It also recorded a tear-free accelerated virgl/GL guest, but that is not a
controlled Windows comparison and does not establish a Windows 3D solution.

## Implemented adaptation

Hydroxl retains GPUI and borrows spice-gtk's scheduling strategy, not its GTK
widget or cross-thread canvas access:

- Control messages post a ready GLib idle source, waking the blocking event loop
  without the old Rust receiver's 8 ms polling wait. Ready GLib work is drained
  for up to 4 ms before servicing snapshots and the next control batch. This
  bounds starvation under load without sleeping between ready batches.
- Invalidation callbacks accumulate bounded damage regions rather than copying
  pixels or advancing generation per strip. Adjacent strips merge when their
  bounding rectangle costs no more pixels than separate copies; distant regions
  stay separate. More than 128 unmerged regions falls back to the full primary.
- Visible SPICE displays now consume snapshots from GPUI's platform frame
  callback, before its draw decision, rather than a 4 ms upload timer. Each
  callback requests the next asynchronous decoder-thread snapshot. A 50 ms
  housekeeping timer handles hidden displays and restarts frame scheduling when
  shown. Session identity guards prevent old callbacks/pollers from driving a
  reconnected session. VNC/D-Bus retain their existing polling cadence.
- The decoder copies current damaged canvas regions into owned pixels after
  servicing ready events, advancing generation once per snapshot. No raw canvas
  pointer crosses to GPUI. Unlike GTK's same-loop paint-time read, this remains
  pipelined: a snapshot completed after a callback is consumed at a later frame
  opportunity. Slow decoding or host throttling can add further latency.
- The UI borrows snapshot rows under the framebuffer lock and uses
  `Window::update_texture_with_stride`. This removes temporary packed patch
  allocations/copies and the UI's full-frame clone on texture creation/resize.
  The texture still owns a CPU copy for recovery, and atlas staging/GPU upload
  remain. Holding the lock through this CPU copy is the tradeoff; no GPU wait or
  GLib dispatch occurs while these rows are borrowed.
- Initial publication waits for actual channel readiness. Primary destruction
  clears the retained pointer. Creation queries the current mark because a
  marked channel can replace its primary during resize without emitting another
  mark signal. GL draw/acknowledgment remains separate.
- SPICE no longer uses the UI's 5/40 ms silence heuristic. VNC and D-Bus retain
  that heuristic. GPU upload ordering and presentation modes are unchanged.

Validation: `cargo check --locked` passed; the full default suite passed (38 tests,
5 ignored). The explicitly run SPICE runtime/unavailable-graphics-socket test
also passed. A new pixel-level regression covers 120 coalesced strips, copying
the live canvas at publication, unchanged rows, initial readiness, primary
destruction, and resize with readiness retained. It also checks that two distant
changes publish two small patches without copying the gap, and still advance
generation only once. The headless texture API regression exercises strided
uploads and rejects short buffers, undersized strides, and stride overflow.
The vendored GPUI library's own test target could not compile: the published
crate lacks font fixtures and the current lock/features do not include its
test-only dependencies; its property-test macro also reports a compilation
error. Its extended byte-level texture test was therefore not executed. The
application's headless test does not rasterize GPU output. These tests do **not** prove
tear-free Windows output, real GLib scheduling under SPICE load, or GPU presentation.
No guest was started or reconfigured by the agent during this work.

**User validation, 2026-11-18:** after testing the updated viewer, the user
reported, “yep this fixed it.” This confirms that the combined scheduling,
frame-paced upload, damage-region, and copy-reduction changes resolved the
reported tearing in their tested Windows 2D virtio-gpu/SPICE setup. This is a
user-reported result, not an agent-observed recording or an instrumented
before/after measurement. Exact resolution, refresh rate, workload, and test
duration were not supplied, and the individual changes were not tested in
isolation.

The 4 ms dispatch budget is a fairness bound, **not an end-of-frame fence**.
Publication can still occur partway through a long burst or between separately
arriving draws. The user-confirmed fix applies to the tested setup, not every
guest or workload. A matched virt-viewer comparison remains useful if symptoms
recur; it is not a prerequisite for recording this successful test. The
historical failure path is retained below.

### Evidence and limits

This investigation inspected Hydroxl, upstream sources listed below, and the local
inactive Windows domain XML. The XML has `model type='virtio'`, `device='virtio-vga'`,
no `accel3d`, no SPICE GL, LZ image compression, JPEG/zlib disabled, and streaming
off. Installed versions: QEMU 11.1.3, spice-server 0.16.0, spice-gtk 0.43, libvirt
12.9.0. The VM was shut off; no guest was started or reconfigured. Guest driver
version and failing resolution were not measured. Results in **Tried** and **GL
scanout** below are retained historical observations, not new reproductions.

The code establishes how mixed updates can be displayed, not which exact update
caused an observed tear. A synchronized guest/QEMU/client trace is still required
to exclude additional guest-buffer reuse or host-presentation problems.

## Root-cause mechanism

1. **Windows supplies software pixels, not a guest GPU swapchain.** In the upstream
   VirtIO Windows display-only driver, `PresentDisplayOnly` passes dirty/move
   rectangles to `ExecutePresentDisplayOnly`. That copies pixels into the backing
   framebuffer, unions the update bounds, then queues `TransferToHost2D` and
   `ResFlush`. QEMU's `virtio_gpu_transfer_to_host_2d` copies backing memory into
   the host resource; `virtio_gpu_resource_flush` notifies display listeners through
   `qemu_console_update`. These operations do not convey a Windows present ID or a
   guest-to-viewer atomic page flip. The inspected upstream driver is a reference,
   not proof of the installed guest driver's implementation.
2. **QEMU turns the software console into many SPICE draws.** In
   `ui/spice-display.c`, `qemu_spice_display_update` unions dirty bounds;
   `qemu_spice_display_refresh` calls `qemu_spice_create_update` when its update
   queue is empty. That function compares the surface with a mirror in
   **32-pixel-wide horizontal blocks**, merging changed rows vertically per block.
   Each resulting rectangle becomes a separate `QXL_DRAW_COPY` with its own pixel
   copy. This is QEMU's generic software-display adapter, even though the guest
   device is VirtIO rather than QXL.
3. **spice-gtk exposes incremental state.** Its `DRAW` handlers update the primary
   canvas and emit `display-invalidate` for each primary-surface rectangle. The
   ordinary software draw path does not give Hydroxl a refresh-batch completion
   signal. `display-mark` is an expose/readiness signal: spice-server queues it
   during client initialization, not after every QEMU refresh. It cannot be used
   as a per-frame fence. Draw timestamps and message acknowledgments also do not
   tell the viewer that no further rectangle belongs to the current update.
4. **Before the adaptation, Hydroxl published between those draws.** In
   [`on_damage` / `update_frame`](../src/display/spice/mod.rs), every invalidation
   copies the rectangle into the owned framebuffer and increments `generation`.
   The mutex makes each copy safe, but does not group an entire QEMU refresh.
   [`DisplayView::upload`](../src/ui/display.rs) samples the resulting
   state and uploads damaged pixels. Its settle heuristic waits for 5 ms of
   observed quiet, but permits publication after 40 ms even without quiet. A gap
   between draws, or a long burst reaching the timeout, can therefore publish
   new strips alongside old strips. The first image bypasses this settling too.
5. **Before the adaptation, Hydroxl introduced gaps while work was ready.** spice-gtk's
   `g_coroutine_signal_emit` schedules a GLib idle callback and yields the decoder
   until the callback completes. Hydroxl's `work` dispatches at most 32 GLib
   iterations, then returns to `recv_timeout(8 ms)` on the Rust control channel.
   Pending GLib work does not wake that Rust receiver. A single display channel's
   chained invalidations can therefore be split into bursts with artificial waits.
   The UI polls on a separate 4 ms timer and interprets missing generation changes
   as quiet, although the decoder may simply be waiting for the next dispatch.
   This is an actionable local scheduling problem, not a limitation that should
   be attributed solely to the SPICE protocol. See the line-by-line audit below.

This is **update-coherency tearing**: even a perfectly VSync-presented host window
can show an internally mixed image. It is distinct from a host display swapping
buffers partway through physical scanout. Both are possible; the latter has not
been demonstrated here.

### Why resolution matters

For an entirely changed 32-bit desktop, with all rows in each strip changed:

| Resolution | SPICE strip draws per QEMU update | Raw pixel payload per update |
| --- | ---: | ---: |
| 1920×1080 | 60 | 7.91 MiB |
| 2560×1440 | 80 | 14.06 MiB |
| 3840×2160 | 120 | 31.64 MiB |

These are source-derived examples, not measured traffic. Fragmented changed rows
can create more rectangles; unchanged areas create fewer. At 4K and 60 complete
updates/s the raw pixels alone are about 1.85 GiB/s before compression, excluding
mirror comparisons, copies, decoding, and texture uploads. This is not a claim
that the current software path actually runs at 60 Hz. Greater pixel volume and
draw count widen the opportunity to present a partial update; resolution is an
amplifier, not a separate correctness threshold. Compression reduces wire volume,
not the need for a coherent presentation boundary.

### Why VNC can look better without proving atomicity

RFB groups rectangles into a counted `FramebufferUpdate`. Hydroxl's
[`VNC read_loop`](../src/display/vnc.rs) advances `generation` after that group,
rather than after every rectangle. This provides a useful publication boundary
and avoids SPICE's per-draw generation churn, consistent with the earlier VNC
result. However, the decoder locks and mutates the shared framebuffer per rectangle,
not for the whole message. The UI can still read a later update's partial pixels
while consuming an earlier pending generation. VNC is therefore a practical
workaround, **not a guarantee of atomic guest frames**, and an RFB update itself
need not correspond one-to-one to a Windows present.

## Tried

| Change | Result |
| --- | --- |
| Combined GLib dispatch, decoder-thread snapshots, frame-paced uploads, bounded damage regions, and reduced copying | User confirmed on 2026-11-18 that this fixed the reported Windows 2D virtio-gpu/SPICE tearing. Individual contributions were not isolated. |
| Poll for new frames every 4 ms instead of 16 ms | Lower latency; tearing unchanged. |
| Settle: show a software frame once draws stop for 5 ms, at most 40 ms after the first | Fewer torn frames; still tears. Now replaced for SPICE by decoder-thread snapshots; retained for other transports. |
| Local SPICE encoding: `streaming mode='off'`, `jpeg`/`zlib` never, `image` LZ | Removes MJPEG video regions and banding; still tears. Kept for local (`listen type='none'`) displays. `image compression='off'` made frames tens of MB and was replaced by LZ. |
| Snap near-1:1 scales to one device pixel per guest pixel, origin on whole device pixels | Fixes vertical lines in Fit guest mode. Kept. |
| GL scanout for 2D displays (`<gl enable='yes'/>` without 3D) | Whole frames, but black screen; see below. Off by default, `HYDROXL_SPICE_GL=1` enables it. |

## GL scanout on a 2D display

QEMU renders the 2D console into a GL texture and exports it as a DMA-BUF: `XB24`,
modifier `DRM_FORMAT_MOD_INVALID`, one plane. The modifier makes the Vulkan direct
import decline, so frames go through EGL readback.

- A viewer that connects after the guest reached its final resolution reads complete,
  lit frames (verified headless and in the app).
- A viewer connected across a resolution change (Windows boot: 640×480, 1280×800, then
  the guest-resize size) keeps receiving `gl-draw` for the new scanout, but every read
  is black. `virsh screenshot` shows the lock screen at the same time.
- Re-importing the same DMA-BUF is also black. Waiting on the buffer's write fences
  (`poll` POLLIN) before reading does not help. Reconnecting while the guest is still
  switching modes is also black afterwards.

These observations isolate a mode-switch-dependent failure in the GL export/import
path; they do **not** prove that QEMU stopped filling the texture. A stale exported
buffer or import/lifetime problem could also explain them. A QEMU screenshot uses
a different read path. Not reported upstream yet; diagnose this separately from
software-update tearing before recommending GL for 2D guests.

## Recommendations

1. **Use the updated SPICE viewer for the confirmed setup; retain VNC as a
   fallback if tearing recurs elsewhere.** VNC was the previously observed
   effective workaround and requires no guest 3D driver. See feature tradeoffs
   below. Reducing guest resolution or motion is a secondary mitigation, not a fix.
2. **Keep the current local lossless/LZ settings as
   mitigations.** Disabling streaming avoids separately timed video regions and
   lossy banding; it does not combine draws into a frame. More aggressive polling,
   uncompressed images, larger arbitrary delays, host VSync, or double-buffering
   an already partial snapshot cannot establish a missing completion boundary.
   Do not treat a fixed QEMU refresh interval (including the existing 30 ms code
   comment) as a guest present fence or universal cadence.
3. **Validate the implemented dispatch/publication adaptation against virt-viewer.**
   The implementation removes the 32-iteration/receiver-sleep pattern, integrates
   control wakeups, and copies coalesced damage on the decoder's owner thread.
   It deliberately does not pass spice-gtk's raw canvas pointer to GPUI: unlike
   GTK's shared loop, these are different threads. This can improve pacing and
   reduce partial-update exposure without protocol changes; it does not guarantee
   atomic Windows presents. Measure before/after against virt-viewer with the
   same software SPICE configuration.
4. **For guaranteed coherency, establish a real publication contract.** A
   coordinated QEMU/SPICE server/client change could delimit a software update
   batch and publish an immutable snapshot only after all its draws are decoded.
   The boundary must survive server queueing, draw optimization, and transport;
   an arbitrary marker inserted into QEMU's command queue is not sufficient unless
   those semantics are verified. A complete QEMU batch is still not necessarily
   a Windows present. If true guest-frame atomicity is required, also establish
   ownership/fencing of the guest's source buffer. For VNC, a staging framebuffer
   committed at message completion would close the client-side publication race.
5. **Do not enable GL or guest 3D as a blanket Windows workaround.** Host SPICE GL
   and guest 3D are separate settings. The existing experimental 2D GL path has
   the mode-switch problem above. A tear-free Linux virgl guest does not establish
   Windows driver support. QXL with an appropriate Windows driver, or another
   display transport, can be evaluated as alternatives, but neither has been
   validated here as a fix.

### Verification required before declaring a fix

Use the same Windows build, guest driver, workload, resolution, scale, and host
presentation settings for SPICE/VNC comparisons. Test at 1080p, 1440p, and 4K using
scrolling and a high-contrast moving pattern with a visible frame counter. Capture
guest/source and viewer images to distinguish mixed source pixels from host
scanout tearing. Record QEMU refresh/draw batch identifiers and timestamps alongside
client invalidations and UI publications; verify whether each published image
contains every rectangle of its source batch. Include gaps over 5 ms, sustained
updates over 40 ms, boot-time mode switches, resizing, and reconnecting.

The existing headless test below checks reception, dimensions, and lit pixels at
500 ms sampling intervals; it cannot by itself detect or prove the absence of
transient tearing. A synthetic split update with distinguishable old/new pixels
is needed to test the publication contract independently of visual inspection.

Also log GLib iteration counts, whether ready sources remain at budget exhaustion,
actual control-channel waits, generation changes, and UI snapshot/paint times.
Without those measurements, the dispatch problem is proven in isolation below,
but its contribution to a particular guest recording is not quantified.

## Diagnostics

- `HYDROXL_DEBUG_DISPLAY=1`: logs `gl-draw`, `invalidate`, scanout format, modifier and
  stride, readback lit-pixel counts, and display window uploads.
- `HYDROXL_OPEN_DISPLAY=1`: opens the selected running machine's display at startup.
- `HYDROXL_SPICE_GL=1`: adds GL scanout to 2D SPICE displays at the next start.
- `HYDROXL_SPICE_DOMAIN=<uuid> cargo test live_spice_frames -- --ignored --nocapture`
  attaches headless and reports frames. `HYDROXL_SPICE_GPU=1` takes the direct-import
  branch; `HYDROXL_SPICE_RESIZE=WxH` requests a guest resize first.
- A second SPICE client disconnects the first. Close the app's display window first.

Fallback if tearing persists with the updated viewer: use VNC. What changes with VNC:

- Clipboard sync works over VNC too: starting with the VNC viewer connects the guest
  agent (spice-vdagent) to QEMU's built-in agent (`qemu-vdagent`), and the viewer speaks
  QEMU's extended VNC clipboard. Starting with SPICE connects it back to SPICE.
- Audio plays on the host directly: starting with the VNC viewer routes guest sound to
  PipeWire (or PulseAudio) instead of the SPICE client, and starting with SPICE routes it
  back. Backends set by hand (ALSA, JACK, D-Bus) are left alone.
- USB redirection from the viewer toolbar is SPICE-only. Attaching a host USB device to
  the machine (Hardware, USB and input) works with either viewer.

## Line-by-line code audit

The ranges below cover the statements on the relevant software path, in execution
order; declarations/braces are grouped with their statements. Hydroxl references
describe the implemented adaptation. The original failure path is described in
**Root-cause mechanism**. Upstream ranges refer to the pinned
versions in **Upstream sources**, not to unversioned `master`.

### Windows and QEMU: producing the rectangles

| Source / lines | What the statements actually do; consequence |
| --- | --- |
| Windows `viogpudo.cpp` 2855–2879 | Set destination/source address, pitch, pixel format, and rotation for the CPU blit. This is a software framebuffer path, not a swapchain flip. |
| Windows 2881–2890 | Copy all move destinations and dirty rectangles into the backing buffer. |
| Windows 2892–2901 | Compute union bounds, falling back to the full source extent, then calculate the backing-buffer offset. |
| Windows 2916–2924 | Queue one transfer and one flush for that bounding rectangle and return success. The driver is not emitting QEMU's 32-pixel strips. |
| Windows `viogpu_queue.cpp` 344–386 | Zero/initialize the flush and transfer commands and call `QueueBuffer`; these two helpers contain no completion wait or fence setup. This warrants tracing guest buffer lifetime if source tearing remains, but is not proof of a race in the installed driver. |
| QEMU `virtio-gpu.c` 474–496 | Decode the transfer command, find the resource, reject invalid bounds. No display publication happens here. |
| QEMU 498–517 | Obtain the host image pitch/data, copy each partial-width row or a full-width span from guest backing memory using `iov_to_buf`. |
| QEMU 603–605 | Translate the intersecting flush rectangle into scanout coordinates and call `qemu_console_update`. |
| QEMU `spice-display.c` 382–390 | Form bounds, set notification state, union with existing dirty bounds. No per-Windows-present boundary is retained. |
| QEMU 494–500 | Update the console, lock the software SPICE queue, and generate a new set of draws only if the previous queue is empty and a surface exists. Queue-empty is not client-visible completion. |
| QEMU 196–209 | Set block width to 32, allocate per-column run tracking, initialize each entry to −1 (no active changed run). |
| QEMU 212–223 | Walk dirty rows and 32-pixel horizontal blocks, compare source bytes with the mirror. |
| QEMU 224–232 | On an unchanged row after a changed run, emit that column's rectangle and reset its run marker. |
| QEMU 234–237 | On a changed row, start a run if needed; otherwise extend the existing run by continuing the loop. |
| QEMU 242–253 | Emit runs still open at the bottom of the dirty area. A fully changed 3840-wide desktop yields 120 vertical strips. |
| QEMU 257 | Clear the dirty bounds. There is no appended end-of-refresh draw/message here. |
| QEMU 141–154 | For each rectangle, allocate its bitmap and initialize an opaque `QXL_DRAW_COPY`. |
| QEMU 158–176 | Set a timestamp per draw and describe a top-down 32-bit bitmap with stride `rectangle width × 4`. The timestamp is not a shared batch identifier. |
| QEMU 178–191 | Copy that rectangle into the mirror, copy the mirror pixels into its own bitmap, queue the draw. Each queued rectangle owns its pixels. |
| QEMU 541–548 | Under the queue lock, pop one command for spice-server. No batch identifier accompanies it. |
| spice-server `dcc-send.cpp` 317–325, 972–981 | Marshal ordinary draw-copy surface ID, box, clip, copy operation, and image. `fill_base` does **not** serialize QEMU's `mm_time` into the ordinary draw base. Video timestamps are a different path. |

### spice-gtk and Hydroxl: decoding, dispatching, and copying

| Source / lines | What the statements actually do; consequence |
| --- | --- |
| spice-gtk `spice-channel.c` 2397–2411 | Wait for input, then process incoming messages while the stream is readable. The decoder itself is willing to drain ready input. |
| spice-gtk `channel-display.c` 1918–1921 | Parse a draw-copy and invoke `DRAW(copy)`. |
| spice-gtk 1187–1194 | Find its surface, draw the rectangle into the canvas, then emit invalidation for a primary surface. This rectangle is decoded before Hydroxl receives its callback. |
| spice-gtk 1143–1148 | Emit the bounding coordinates through `g_coroutine_signal_emit`. There is no refresh-end argument. |
| spice-gtk `gio-coroutine.c` 219–226 | If already in the main coroutine, emit directly; otherwise add an idle source and yield the decoder until the callback is handled. |
| spice-gtk 193–203 | The idle callback emits the signal synchronously, marks it notified, resumes the decoder, and removes itself. The resumed decoder can queue the next invalidation idle. |
| Hydroxl [`Worker::send`, 67–72](../src/display/spice/mod.rs#L67-L72) | Queue the command before adding a ready GLib source. The source wakes the worker even if it has not yet started blocking. |
| Hydroxl [`work`, 163–202](../src/display/spice/mod.rs#L163-L202) | Handle up to 32 open/input/snapshot/close messages. A snapshot command sets demand; it does not copy pixels before ready decoder work is serviced. |
| Hydroxl [203–208](../src/display/spice/mod.rs#L203-L208) | Dispatch ready GLib work until no source is ready or 4 ms elapses. The budget bounds fairness, not the number of strips. |
| Hydroxl [210–220](../src/display/spice/mod.rs#L210-L220) | Publish requested snapshots under the framebuffer mutex, then block in GLib only when the control batch was not exhausted. Ready sources dispatch without an artificial receiver timeout. |
| Hydroxl [`SoftwareDisplay::create/destroy`, 232–245](../src/display/spice/mod.rs#L232-L245) | Retain the primary and actual readiness, invalidate the full new extent, and clear the pointer/damage on destruction. Outstanding demand survives replacement. |
| Hydroxl [`invalidate`, 247–300](../src/display/spice/mod.rs#L247-L300) | Validate/clamp geometry. Merge regions only when their union costs no more pixels than separate copies; preserve distant regions. Bound fragmentation at 128 entries, then fall back to full extent. No pixel copy or generation increment occurs here. |
| Hydroxl [`publish`, 303–330](../src/display/spice/mod.rs#L303-L330) | Require demand and mark, copy current damaged regions without yielding, and advance generation once if pixels were copied. This samples decoded state, not a proven Windows frame boundary. |
| Hydroxl [774–788](../src/display/spice/mod.rs#L774-L788) | Connect primary-create/destroy, invalidate, and mark signals. GL callbacks remain conditional on `options.gl`. |
| Hydroxl [`on_create`, 1085–1111](../src/display/spice/mod.rs#L1085-L1111) | Query surface 0's descriptor/readiness and retain it. No forced initial publication. A resize may inherit the existing channel mark. |
| Hydroxl [1113–1135](../src/display/spice/mod.rs#L1113-L1135) | Clear primary lifetime on destroy, accumulate damage on invalidate, and update readiness on mark. None treats mark as a repeating end-of-frame event. |
| Hydroxl [`update_frame`, 1139–1161](../src/display/spice/mod.rs#L1139-L1161) | Select 2/4-byte source format; validate dimensions, pointer, and pitch. These are geometry checks, not frame-boundary checks. |
| Hydroxl [1162–1176](../src/display/spice/mod.rs#L1162-L1176) | Clamp bounds; on first pixels or resize, allocate the extent and copy the full current primary. Full extent does not imply a complete guest frame. |
| Hydroxl [1177–1190](../src/display/spice/mod.rs#L1177-L1190) | Read signed-pitch rows into tightly packed owned rows; force 32-bit alpha opaque. No GLib dispatch occurs while reading. |
| Hydroxl [1191–1206](../src/display/spice/mod.rs#L1191-L1206) | Convert RGB555/RGB565 to 8-bit BGRA. Not the usual 32-bit Windows path. |
| Hydroxl [1208–1209](../src/display/spice/mod.rs#L1208-L1209) | Mark copied damage and report success to `publish`; generation advances after all regions, not after each region. |

### Hydroxl: deciding what to publish

| Source / lines | What the statements actually do; consequence |
| --- | --- |
| [`attach`, 320–376](../src/ui/display.rs#L320-L376) | Start SPICE's platform-frame callback chain; retain a 50 ms hidden/show housekeeping timer. Other transports retain 4/50 ms polling. Ignore tasks belonging to a replaced session. |
| [`schedule_spice_upload`, 378–400](../src/ui/display.rs#L378-L400) | Queue at most one callback, with a weak view and session identity check. Consume available data at the platform frame opportunity and schedule the next callback while visible. |
| [`upload`, 407](../src/ui/display.rs#L407) | Request an asynchronous decoder-thread snapshot without blocking the UI. Completion after this callback is consumed at a later frame opportunity. |
| [408–440](../src/ui/display.rs#L408-L440) | Handle reserved DMA-BUF copying and acknowledgment separately from ordinary CPU pixels. |
| [445–470](../src/ui/display.rs#L445-L470) | Lock the framebuffer, release/reacquire around captured input, and skip an already-consumed generation. Generation is not a guest frame ID. |
| [471–492](../src/ui/display.rs#L471-L492) | Exclude SPICE from the historical 5/40 ms settle heuristic; retain it for other transports. Consume the available generation. |
| [493–520](../src/ui/display.rs#L493-L520) | Handle closure/reconnect, hidden windows, and zero dimensions. None supplies a guest frame fence. |
| [533–568](../src/ui/display.rs#L533-L568) | Create textures from borrowed full pixels or drain damage geometry and borrow strided rows directly. The texture API copies synchronously before the mutex is released; no packed intermediate pixel buffers are allocated. |
| [584–592](../src/ui/display.rs#L584-L592) | Request full damage on failure; otherwise update size/status and notify GPUI. |
| [`mark_damage`, 26–49](../src/display/damage.rs#L26-L49) | Bound damage by collapsing to full extent after 128 pending entries or enough summed area. This bounds bookkeeping; it cannot turn partial source pixels into a complete frame. |
| [`take_damage_rects`, 52–56](../src/display/damage.rs#L52-L56) | Drain geometry without copying pixels. Packed `take_damage` now exists only for tests; production borrows rows while locked. |

### GPUI/WGPU: checking the competing GPU-upload explanation

| Source / lines | What the statements actually do; consequence |
| --- | --- |
| [`Window::update_texture_with_stride`, 143–185](../vendor/gpui-pre/src/window/texture.rs#L143-L185) | Validate ownership, bounds, stride and required source bytes before patching retained CPU rows. Packed `update_texture` delegates here. No producer pointer is retained. |
| [186–208](../vendor/gpui-pre/src/window/texture.rs#L186-L208) | Accumulate bounded damage and request refresh; explicitly defer GPU writes until paint. |
| [`paint_texture_in`, 253–279](../vendor/gpui-pre/src/window/texture.rs#L253-L279) | Pack retained pixels for damaged regions and queue atlas updates, or rebuild the tile when needed. This remaining staging copy is not eliminated. |
| [`upload_texture`, 356–364](../vendor/gpui-pre-wgpu/src/wgpu_atlas.rs#L356-L364) | Own/swizzle the bytes and append a pending GPU upload. |
| [`before_frame`, 74–76](../vendor/gpui-pre-wgpu/src/wgpu_atlas.rs#L74-L76), [`render_frame`, 1461](../vendor/gpui-pre-wgpu/src/wgpu_renderer.rs#L1461) | Flush pending atlas writes before rendering the scene. |
| [`flush_uploads`, 367–397](../vendor/gpui-pre-wgpu/src/wgpu_atlas.rs#L367-L397) | Write every queued patch with the correct origin, extent, and packed row stride. |
| [`render_frame`, 1644–1647](../vendor/gpui-pre-wgpu/src/wgpu_renderer.rs#L1644-L1647), [`draw`, 1235](../vendor/gpui-pre-wgpu/src/wgpu_renderer.rs#L1235) | Submit the rendered scene and present its acquired window image. The code does not present after each SPICE rectangle. |
| [Surface configuration, 470–474](../vendor/gpui-pre-wgpu/src/wgpu_renderer.rs#L470-L474) | Select the supported preferred mode or FIFO. X11 requests no override; Wayland requests Mailbox. This is not an explicit immediate/tearing mode. Actual compositor behavior was not measured. |

**Conclusion from this path:** adding a second guest texture would not repair
already mixed CPU pixels. The code points first to dispatch/publication pacing,
not unsynchronized per-strip window presents. Extra CPU copies may amplify cost,
but their measured share is unknown.

## What virt-viewer does differently

The installed `remote-viewer` is 11.0 (package 11.0-4); its v11.0 source and
spice-gtk 0.43 / GTK 3.24.52 were inspected. No live side-by-side guest comparison
was run. The claim that virt-viewer universally “bypasses” tearing is unverified.

| Source / lines | Behavior worth borrowing |
| --- | --- |
| virt-viewer `remote-viewer-main.c` 39–42 | Create the application and run `g_application_run`: a GLib-driven application loop, unlike Hydroxl's former periodic 32-iteration pump. |
| virt-viewer `virt-viewer-display-spice.c` 308–318 | Create `spice_display_new_with_monitor`, connect readiness, add/show the GTK widget. virt-viewer delegates painting to spice-gtk rather than implementing its own SPICE rectangle renderer. |
| spice-gtk `spice-widget.c` 2769–2775 | Retain the primary canvas address, geometry, and pitch. |
| spice-gtk `spice-widget-cairo.c` 35–49 | For 32-bit pixels, create a Cairo image surface **over the primary data**, without Hydroxl's former per-invalidation owned-framebuffer copy. Hydroxl still needs a coalesced copy across threads. Only 16-bit formats allocate a conversion buffer here. |
| spice-gtk `spice-widget.c` 2976–2983 | Ignore unrealized/outside-monitor damage; convert color only when required. |
| spice-gtk 2985–3006 | Transform damage to widget coordinates, expand for filtering when appropriate, and **queue** a redraw, not an immediate paint. |
| GTK `gtkwidget.c` 5685–5692 | Turn the rectangle into an invalid region and queue it. |
| GTK `gdkwindow.c` 4351–4357 | Union new damage into the existing update area; schedule when first dirty. Multiple invalidations can become one paint. |
| GTK 3798–3801 | Request `GDK_FRAME_CLOCK_PHASE_PAINT`. The toolkit's paint scheduling, not a 5/40 ms decoder-silence guess, decides when to redraw. |
| spice-gtk `spice-widget.c` 1526–1530 | Skip software drawing until marked and the canvas/area are valid; then call Cairo. Before adaptation Hydroxl published during primary-create and forced a full copy on mark; it now honors readiness. |
| spice-gtk `spice-widget-cairo.c` 121–130 | At draw time, set the current canvas surface as the source and paint it. It can use canvas updates decoded since the original redraw request. |
| spice-gtk `spice-widget.c` 3014–3016 | Store mark and update readiness. Mark is not used as a repeating frame delimiter here either. |

The important differences are **continuous event servicing, damage coalescing,
paint-time consumption, fewer copies, and a shared event-loop ownership model**.
They reduce latency and artificial stalls; they do not reconstruct a missing
Windows-present boundary. GTK can still draw between two SPICE messages if a
paint opportunity occurs there. Do not copy its raw-pointer strategy into a
separate GPUI thread, or mistake its GL/video path for the 2D Cairo path.

### Should Hydroxl use the GTK widget directly?

**Yes, if a separate GTK guest-display window is acceptable; not as a drop-in
widget inside the existing GPUI window.** Hydroxl's entry point runs GPUI and its
display is a GPUI `DisplayView`. A `SpiceDisplay` needs a GTK widget hierarchy and
GTK/GLib event-loop ownership. Matching the two toolkits' window/input/rendering
lifecycles is a separate integration project, not a replacement texture element.
X11-specific foreign-window tricks would not be a portable Wayland solution.

| Choice | Recommendation |
| --- | --- |
| Launch the installed virt-viewer | Best initial baseline and lowest custom display-maintenance cost if an external viewer fits the product. First verify the same Windows workload without GL. |
| A Hydroxl-owned GTK helper process using `SpiceDisplay` | Sensible if Hydroxl needs its own display-window controls but wants to reuse the mature widget/session stack. Keep GTK on the helper's main thread and its GLib loop continuously running. |
| Embed GTK into the GPUI display surface | Not recommended as a quick tearing fix. It adds cross-toolkit embedding, focus/capture, scaling, and lifecycle complexity. |
| Keep the GPUI viewer | The adaptation above implements improved event servicing and snapshot pacing while preserving the current UI. Live comparison remains necessary. |

A helper/external viewer must preserve the libvirt connection and socketless
graphics attachment (`listen type='none'`), reconnect and shutdown behavior, and
any required toolbar/thumbnail integration. Audio, clipboard, and USB handling
also need the appropriate SPICE session setup; constructing a display widget
alone is not the whole application. Reuse buys mature behavior and fewer custom
copies, but does not add an atomic guest-frame protocol. The decision should
follow a matched virt-viewer comparison, not a promise that GTK cannot tear.

### Isolated dispatch probe

A synthetic probe used the installed GLib through Python `ctypes`: each idle
callback scheduled the next idle and returned false, matching the successive-idle
handoff pattern, for 120 callbacks. One run produced:

| Dispatch strategy | Completed callbacks after each servicing burst | Elapsed |
| --- | --- | ---: |
| 32 iterations, then 8 ms wait between bursts | 32, 64, 96, 120 | 24.66 ms |
| Continuously service ready callbacks | 120 | 0.31 ms |

This verifies that newly queued idle work survives a 32-iteration budget and that
three unnecessary 8 ms waits can dominate an otherwise trivial burst. It is not
a SPICE throughput benchmark: it omits sockets, decoding, pixel copies, other
GLib sources, and GPU work. An initial receiver wait could add another 8 ms;
control messages can shorten waits. The live number of iterations per draw must
still be measured. This motivated the implemented local event-pump adaptation;
it does not measure its performance on the affected guest.

## Alternative: guest streaming with Lumen / a Moonlight-compatible client

Inspected the requested forks on 2026-11-18: Umbra at
`500d9f3c752db1d9effe310158c5bf83ea914af2`, Lumen at
`b8422791c1868d45f865abbab5d3d611e6ec88e4`. These are **guest desktop streaming**
components, not drop-in GPUI graphics backends.

| Inspected source | Finding and implication |
| --- | --- |
| [Umbra README](https://github.com/simonfalke-01/Umbra/blob/500d9f3c752db1d9effe310158c5bf83ea914af2/README.md), `VideoToolboxDecoder.swift`, `MetalVideoRenderer.swift` | Apple Silicon/macOS client using Swift/AppKit, VideoToolbox, and Metal. Its renderer cannot run directly in Linux Hydroxl. Borrow bounded queues, frame ownership, and presentation timing rather than porting its Apple UI. |
| Umbra `Vendor/GameStreamProtocol/PROVENANCE.md`, `Upstream/src/VideoDepacketizer.c` | GameStream transport derives from pinned Moonlight-common-c. `reassembleFrame` hands a completed frame/decode unit to the decoder; incomplete frames trigger loss handling. This has explicit frame identity unlike software SPICE rectangle invalidation. |
| [Lumen capture](https://github.com/simonfalke-01/Lumen/blob/b8422791c1868d45f865abbab5d3d611e6ec88e4/src/platform/windows/display_base.cpp), `display_ram.cpp` | Create a D3D11 device on the selected adapter, duplicate its output, acquire a desktop frame, and release it after capture. This bypasses QEMU's SPICE strips when Lumen runs **inside Windows**. Capturing the Hydroxl window on the Linux host would merely encode its already mixed pixels. |
| Lumen `src/video.cpp`, `encoder_t software` | Software encoding exists; GPU passthrough is not categorically mandatory for every legacy stream. But CPU encoding does not supply a missing capturable D3D11 display. Capture on this Windows display-only virtio-gpu driver must be tested first; no success or impossibility has been established here. High-resolution software encoding also needs a throughput measurement. |
| [Lumen VDD README](https://github.com/simonfalke-01/Lumen/blob/b8422791c1868d45f865abbab5d3d611e6ec88e4/src/platform/windows/virtual_display_driver/README.md), Umbra `LSPVideoPipeline.swift` | The fork additionally contains native LSP video and an IddCx shared-texture/fence path. Direct-frame admission binds to a validated NVIDIA encoder adapter; Umbra's LSP pipeline uses hardware VideoToolbox or Metal BC1 decoding. This is not a demonstrated solution for a 2D-only virtio-gpu guest. Lumen's top-level README explicitly says VDD is not production-ready until installation/frame-flow hardware gates pass. |

**Integration recommendation:** retain SPICE/VNC as the boot, recovery, and
guest-agent-independent console. First test Lumen inside the unchanged Windows
guest with an existing Linux Moonlight client. If capture and performance pass,
an external-viewer launch is the lowest-cost optional fast-desktop mode. A native
Hydroxl session is feasible at the existing `display/session.rs` boundary, but
requires GameStream pairing/certificate trust, transport, decoder, video/audio
synchronization, input, reconnect, and guest provisioning. A first CPU BGRA
decoder output can reuse the framebuffer interface; efficient GPU decode needs
proper decoder-surface lifetime, color conversion, and a supported import path,
not an assumption that every decoded DMA-BUF fits the existing scanout API.

This moves capture upstream of the SPICE failure path and provides encoded frame
boundaries; it does not guarantee correct source capture or physical tear-free
scanout. Guest networking and a running streaming service become prerequisites,
and ordinary video profiles may trade text/color fidelity for bandwidth. Keep
this optional rather than replacing the VM console. Both requested projects
declare GPL-3.0, while Hydroxl declares MIT: embedding or redistributing their
code requires a license review; no code was copied from them here. No streaming
backend or guest software was installed by this investigation.

## Upstream sources

Inspected 2026-11-18; release-tagged sources match local component versions except
the Windows driver, whose installed version is unknown:

- [QEMU 11.1.3 software SPICE adapter](https://github.com/qemu/qemu/blob/v11.1.3/ui/spice-display.c):
  `qemu_spice_create_update`, `qemu_spice_create_one_update`, and
  `qemu_spice_display_refresh`.
- [QEMU 11.1.3 VirtIO GPU](https://github.com/qemu/qemu/blob/v11.1.3/hw/display/virtio-gpu.c):
  `virtio_gpu_transfer_to_host_2d` and `virtio_gpu_resource_flush`.
- [spice-gtk 0.43 display channel](https://gitlab.freedesktop.org/spice/spice-gtk/-/blob/v0.43/src/channel-display.c):
  `DRAW`, `emit_invalidate`, and the `display-mark` signal documentation.
- [spice-server 0.16.0 display client](https://gitlab.freedesktop.org/spice/spice/-/blob/v0.16.0/server/dcc.cpp):
  `dcc_start`, which queues `SPICE_MSG_DISPLAY_MARK` at initialization.
- [VirtIO Windows display-only driver, revision a3eeb32e6200409ba34071da3ba879fea47b1a57](https://github.com/virtio-win/kvm-guest-drivers-windows/blob/a3eeb32e6200409ba34071da3ba879fea47b1a57/viogpu/viogpudo/viogpudo.cpp):
  `VioGpuDod::PresentDisplayOnly` and `VioGpuAdapter::ExecutePresentDisplayOnly`.
- [VirtIO Windows command queue, same revision](https://github.com/virtio-win/kvm-guest-drivers-windows/blob/a3eeb32e6200409ba34071da3ba879fea47b1a57/viogpu/common/viogpu_queue.cpp):
  `TransferToHost2D`, `ResFlush`, and `QueueBuffer`.
- [spice-server 0.16.0 draw marshalling](https://gitlab.freedesktop.org/spice/spice/-/blob/v0.16.0/server/dcc-send.cpp):
  `fill_base` and `red_marshall_qxl_draw_copy`.
- [spice-gtk 0.43 coroutine handoffs](https://gitlab.freedesktop.org/spice/spice-gtk/-/blob/v0.43/src/gio-coroutine.c)
  and [channel reader](https://gitlab.freedesktop.org/spice/spice-gtk/-/blob/v0.43/src/spice-channel.c).
- [virt-viewer 11.0 SPICE display](https://gitlab.com/virt-viewer/virt-viewer/-/blob/v11.0/src/virt-viewer-display-spice.c)
  and [remote-viewer entry point](https://gitlab.com/virt-viewer/virt-viewer/-/blob/v11.0/src/remote-viewer-main.c).
- [spice-gtk 0.43 widget](https://gitlab.freedesktop.org/spice/spice-gtk/-/blob/v0.43/src/spice-widget.c)
  and [Cairo painter](https://gitlab.freedesktop.org/spice/spice-gtk/-/blob/v0.43/src/spice-widget-cairo.c).
- [GTK 3.24.52 widget invalidation](https://gitlab.gnome.org/GNOME/gtk/-/blob/3.24.52/gtk/gtkwidget.c)
  and [GDK window damage/paint scheduling](https://gitlab.gnome.org/GNOME/gtk/-/blob/3.24.52/gdk/gdkwindow.c).
