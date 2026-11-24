# Contributing

Read [the architecture](docs/architecture.md) and
[feature coverage](docs/feature-parity.md) before adding behavior.

Keep the application entry point small. Give each UI pane and backend subsystem
one home. Use typed actions; never dispatch on visible text. Extend shared form
and theme primitives instead of adding per-screen variants.

Keep VM state guards and filesystem mutation in the backend. Preserve original
data until replacements are ready, and do not expose a success state before the
engine confirms it. Pass process arguments directly without a shell.

Run `cargo fmt` and `cargo check --locked` when editing. For changes that affect
the viewer or VM state, follow the [regression checks](docs/architecture.md#viewer-and-vm-status-regression-checks).
