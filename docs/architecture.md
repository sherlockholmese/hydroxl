# Architecture

Hydroxl is a native GPUI application with a local libvirt backend. The entry
point initializes GPUI, key bindings, and a window. It contains no VM logic or
pane composition.

## Boundaries

`ui/` renders one pane per module. It consumes workspace state and submits typed
commands. It never starts a process, copies a disk, parses an appliance, or
constructs domain XML.

`app/` coordinates state and asynchronous work. The main-thread coordinator owns
selection, pending state, errors, and refresh results. Worker threads receive
owned command data and return an explicit response. They never hold GPUI
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
state requirements again before hardware changes; disabled UI controls are not
the authority.

`operations/` has a typed command facade and focused implementations for storage,
media, graphics, boot, networking, sharing, USB, snapshots, and lifecycle. XML
mutation and state guards are shared helpers. Each handler owns its recovery and
cleanup path. Import/export formats are outside the generic operations module.

`appliance/` separates archive handling, OVF parsing, VMDK extent validation,
manifest integrity, conversion, and export. Parsing does not register a VM.
Source files remain unchanged. New appliance files are staged in an owned
folder, and the VM is defined only after conversion finishes. Failures before registration remove only the newly allocated folder. If the
registration result is uncertain, files are retained for recovery.

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
- Report validation precisely. `cargo build --locked` passes. Runtime tests,
  interaction checks, and visual verification remain deferred by the user.
