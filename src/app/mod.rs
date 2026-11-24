mod commands;
mod jobs;
mod preferences;
mod refresh;
mod workspace;
use crate::{backend::Machine, operations::Details};
use gpui_kit::component::input::InputState;
use gpui_kit::{AnyWindowHandle, Entity, FocusHandle, WeakEntity};
pub(crate) use jobs::{ActionSource, Job, JobEvent, Pending, Response};
pub(crate) use preferences::Preferences;
pub struct Workspace {
    pub(crate) machines: Vec<Machine>,
    pub(crate) machine_statuses: Option<(std::time::Instant, crate::backend::MachineStatuses)>,
    pub(crate) last_library_sample: Option<std::time::Instant>,
    pub(crate) selected: Option<String>,
    pub(crate) details: Details,
    pub(crate) details_error: Option<String>,
    pub(crate) details_for: Option<String>,
    pub(crate) search: Entity<InputState>,
    pub(crate) memory_input: Entity<InputState>,
    pub(crate) preferences: Preferences,
    pub(crate) pending: Option<Pending>,
    pub(crate) library_error: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) loaded: bool,
    pub(crate) demo: bool,
    pub(crate) tab: usize,
    pub(crate) library_open: bool,
    /// The guest display shown in the machine pane, when open.
    pub(crate) display_windows:
        Vec<(String, AnyWindowHandle, WeakEntity<crate::ui::display::DisplayView>)>,
    pub(crate) opening_displays: std::collections::HashSet<String>,
    pub(crate) live_preview: Option<(String, std::sync::Arc<gpui_kit::RenderImage>)>,
    /// Hardware tab edits displayed until a successful save is read back.
    pub(crate) hardware_draft: Option<crate::workflows::HardwareDraft>,
    pub(crate) saved_hardware_for: Option<String>,
    /// Guest family whose share setup is shown; `None` follows the detected system.
    pub(crate) share_os: Option<crate::operations::GuestOs>,
    /// Architectures libvirt can run on this host, read once per connection.
    pub(crate) architectures: Vec<String>,
    /// The machine pane is wide enough for two Hardware columns. Set each render.
    pub(crate) wide: bool,
    /// Hardware category shown beside the category list; its highlight slides between
    /// categories and carries on from mid-flight when clicked again.
    pub(crate) hardware_section: crate::theme::motion::Slide,
    /// The Save / Discard bar, kept mounted while it leaves.
    pub(crate) save_bar: crate::theme::motion::Presence,
    /// When the narrow layout last swapped library and machine pane, so only that swap
    /// animates (a resize across the breakpoint does not).
    pub(crate) pane_swapped: Option<std::time::Instant>,
    pub(crate) focus: FocusHandle,
}
