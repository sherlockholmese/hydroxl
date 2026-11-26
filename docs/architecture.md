# Architecture

Hydroxl is a native GPUI application with a local libvirt backend. The entry
point initializes GPUI, key bindings, and a window. It contains no VM logic or
pane composition.

## Boundaries

`ui/` renders the workspace and guest display. Larger panes use submodules for
their controls, input and rendering. They consume workspace state and submit typed
commands. It never starts a process, copies a disk, parses an appliance, or
constructs domain XML.

`app/` coordinates state and asynchronous work. The main-thread coordinator owns
selection, drafts, pending state, errors, and refresh results. `app/worker.rs`
runs blocking work using owned command data and returns explicit events. They never hold GPUI
entities or mutate views. Successful operations are tracked separately from a
subsequent refresh failure, preventing a failed refresh from inviting a duplicate
mutation.

`forms/` owns one field model and one dialog lifecycle. A workflow supplies a
schema and a typed command builder. Input persists through failures. Selects,
keyboard interaction, focus, and modal behavior belong to GPUI Component. There
is no second, custom overlay implementation.

`workflows/` defines typed machine, device, and snapshot actions. Visible labels
are presentation, not dispatch keys. New copy does not change command routing.
Architecture labels and engine values share one registry in the backend model.

`backend/` owns connection identifiers, machine state, and the process boundary.
Commands receive argument arrays, never shell text. Process output is drained
while commands run, with bounded timeouts. The backend checks stopped and saved
state requirements again before writing hardware changes, independently of the
state of the UI controls. A failed machine inspection preserves its last known
record when available. A failed connection preserves the loaded library.

`operations/` has a typed command facade and focused implementations for storage,
media, graphics, boot, networking, sharing, USB, snapshots, and lifecycle. XML
mutation and state guards are shared helpers. Each handler owns its recovery and
cleanup path. Import/export formats are outside the generic operations module.

`appliance/` separates archive handling, OVF parsing, VMDK extent validation,
manifest integrity, conversion, and export. Parsing does not register a VM.
Source files remain unchanged. New appliance files are staged in an owned
folder, and the VM is defined only after conversion finishes. Failures before registration remove only the newly allocated folder. If the
registration result is uncertain, files are retained for recovery.

The display transports publish frames through the shared `display/frame.rs`
model. The viewer owns input capture and presentation. Protocol-specific
clipboard, scanout and software framebuffer handling live beside their transport.

`files.rs` stages outputs beside their destination so a failed write leaves the
previous file available. Use exclusive publication for actions that require a new
file. `xml.rs` provides shared configuration escaping, temporary files and
range edits against the original document.

## Viewer and VM status regression checks

- **Clipboard ownership.** Wait for the display transport to negotiate clipboard
  support before offering host text. When a guest announces a new selection,
  associate the response with that selection and invalidate the previous cache.
  Host clipboard polling should skip text just copied from the guest. SPICE
  announces support when the guest agent connects. VNC negotiates extended
  clipboard on the RFB connection. Local D-Bus also needs a request fallback
  because QEMU can omit grab notifications for agents without grab serials.
  With these agents, stock QEMU may delay a copy until its pending request
  expires and the viewer retries.
- **Power state.** Read live state independently of domain XML and machine
  details. A slow details refresh can otherwise overwrite a newer status
  snapshot. Keep the managed-save flag tied to the stopped state, and tolerate
  transient domains disappearing during enumeration. The viewer follows
  workspace power state when a VM starts or stops outside Hydroxl.
- **SPICE readiness.** Treat the connection as ready after the main and primary
  display channels open. Handle auxiliary channel failures and failed GPU
  readbacks without restarting the session. Back off after a transport failure
  and stop automatic reconnecting when the VM powers off or the retries run out.

Before releasing a viewer change, run `cargo test --locked`. The clipboard
protocol tests cover UTF-8 payloads, stale VNC responses, and D-Bus reads without
a grab announcement. The VM status test covers paused state, managed saves, and
resumes.

Use a disposable VM with the SPICE guest agent for the live checks. Copy
multiline Unicode text in both directions over SPICE, VNC, and local D-Bus.
Repeat after the guest agent starts late, after the viewer regains focus, and
with **Copy guest clipboard** while automatic sync is fetching a selection.
Use `virsh suspend`, `resume`, `managedsave`, and `start` outside Hydroxl and
check that the library and any open viewer reflect the resulting state. The
ignored `live_spice_frames` test exercises display attachment and frame delivery.
See [SPICE test setup](spice-2d-tearing.md#diagnostics) for its invocation.

## Extension rules

- Add a typed command and a focused handler for new engine behavior.
- Add form definitions to the relevant workflow module and compose existing
  fields. Keep process calls and XML construction out of workflow builders.
- Add visual primitives only when several consumers share the same semantic
  contract. Keep colors in the shared theme.
- Preserve an unknown existing setting instead of silently choosing the first
  preset.
- Never make button labels, translated strings, or array positions business
  identifiers.
- Keep related code together. Split by responsibility before a file becomes a
  catch-all; avoid arbitrary one-function abstractions without a boundary.
- Keep source integrity and successful mutation separate from presentation and
  refresh failures.
- Report which build, automated checks, and live scenarios were verified.
