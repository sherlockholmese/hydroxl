# Hydroxl

Native Linux desktop application, Rust and GPUI Kit. Pointer and keyboard input.

Focus: the selected virtual machine. Primary action: start it or open its display.
Tone: quiet, precise, familiar. Styling is the stock shadcn/ui neutral theme from GPUI
Component with Geist; `theme.rs` only adds a raised dark sidebar plane, text-safe status
colors, and control roles for the segmented track and its selection. UTM informs the library and settings grouping; VMware informs the guided wizard.

Rules from review:

- No center-dot punctuation, no descriptive or helper copy. Labels, values, controls.
- No pills or badges for machine state. State is plain colored text under the name,
  a dimmed OS mark when powered off, and the primary action's label.
- Machines show their guest OS mark (libosinfo id, then name). Marks live in
  `assets/os` with their licenses.

Layout: a floating library plane (inset, rounded, bordered) beside the machine pane.
Header: OS mark, name, status; actions menu, contextual Pause / Shut down, one primary.
Tabs: Overview (display well and one ledger column), Hardware, Snapshots.

Hardware follows macOS System Settings: a category list beside the selected category.
The highlight slides between categories; only the content fades. When the pane is
narrow the categories become one row of icons above the content, named on hover.
Devices live in their category (drives in Storage, cards and forwards in Network, USB in USB and input). Repeated items
sit in one inset group divided by hairlines.

The actions menu is flat: only actions that apply in the current state, grouped by what
they touch, each with an icon; destructive ones carry a red icon. No submenus, no scroll.
Menus are at least 220px wide. Short exclusive choices use the one shared segmented
control everywhere, Preferences and the machine tabs included. Entries and buttons that
open a dialog end in "…".

Dialogs share one frame: 18px title, 24px padding, no close button (Cancel and Escape),
fade-and-settle entrance. Field labels are 13px muted, 6px above their control; path
pickers sit inside the input.

Motion: one plane per change (tab or machine swap 180ms fade with 6px travel, save bar
200ms reveal, 130ms exit). Enter uses the emphasized curve, exit the accelerating one.
Anything that leaves stays mounted until it has left and stops taking input at once;
anything retargeted mid-flight continues from where it is. A rebuilt view arrives settled,
so animations never stack. Reduced motion follows the desktop (GNOME, KDE, or
`HYDROXL_REDUCE_MOTION`). Durations, curves and the presence and slide state live in
`theme::motion`.

Hardware is edited in place, never through stacked-field dialogs: joined steppers for
CPU topology, a typed MB field with 1 GB steps, segmented controls for short fixed
choices, switches for on/off, compact menus for longer lists, and a drag-and-drop boot
order with arrow-button alternatives. Edits collect in a draft; a Save / Discard bar
appears only when it differs, and saving applies one ordered operation sequence.

New machine is a three-step wizard (Media, Machine, Review). The OS is chosen as family
then version from the full libosinfo catalog and inferred from the ISO name. The ISO is
optional; disks can be new or existing; location, architecture, execution and firmware
sit under Advanced. Related short fields share a row.

Dialogs remain for focused tasks: the wizard, imports, confirmations, XML.
Verification: build passes; demo-mode screenshots reviewed in dark, light and narrow.
