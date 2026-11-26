use crate::{
    appliance,
    backend::{self, Action, Machine},
    forms::EditorForm,
    operations::{Details, Operation},
};
use gpui_kit::{AnyWindowHandle, WeakEntity};
use std::{
    path::PathBuf,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

pub(super) const REFRESH_BUDGET: Duration = Duration::from_secs(30);
#[derive(Clone)]
pub enum Job {
    Refresh,
    Action(Machine, Action, ActionSource),
    Create(backend::NewMachine),
    Import(PathBuf, backend::QemuBuild, Option<backend::ImportHardware>),
    ConvertDisk {
        source: PathBuf,
        destination: PathBuf,
        format: String,
    },
    Appliance(appliance::ImportSpec),
    Operation(Machine, Operation),
    /// Local preference, applied without a background job.
    Icon(String, String),
    Network(crate::backend::network::HostNetwork),
}

#[derive(Clone, Copy)]
pub enum ActionSource {
    Library,
    Viewer(AnyWindowHandle),
}

impl Job {
    /// The notice shown when the job succeeds: what changed, not that something saved.
    pub(crate) fn done(&self) -> String {
        match self {
            Self::Refresh => "Library refreshed".into(),
            Self::Icon(..) => "Icon saved".into(),
            Self::Network(
                backend::network::HostNetwork::Virtual {
                    name,
                    ..
                }
                | backend::network::HostNetwork::Bridge {
                    name,
                    ..
                },
            ) => format!("Network {name} created"),
            Self::Create(spec) => format!("{} created", spec.name.trim()),
            Self::Import(path, ..)
            | Self::ConvertDisk {
                destination: path,
                ..
            } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                match self {
                    Self::Import(..) => format!("Imported {}", name.unwrap_or_default()),
                    _ => format!("Converted disk saved as {}", name.unwrap_or_default()),
                }
            }
            Self::Appliance(_) => "Appliance imported".into(),
            Self::Operation(_, op) => op.done(),
            Self::Action(machine, action, _) => match action {
                Action::Start => format!("{} starting", machine.name),
                Action::Shutdown => format!("Shutdown requested for {}", machine.name),
                Action::Pause => format!("{} paused", machine.name),
                Action::Resume => format!("{} resumed", machine.name),
                Action::Display => "Display opened".into(),
            },
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Refresh => "Refreshing",
            Self::Icon(..) => "Saving icon",
            Self::Network(_) => "Creating network",
            Self::Create(_) => "Creating machine",
            Self::Import(..) => "Importing machine",
            Self::ConvertDisk {
                ..
            } => "Converting disk",
            Self::Appliance(_) => "Importing appliance",
            Self::Operation(_, Operation::Reboot) => "Restarting machine",
            Self::Operation(_, Operation::Clone(_)) => "Cloning machine",
            Self::Operation(_, Operation::Export(_)) => "Exporting machine",
            Self::Operation(_, Operation::SnapshotCreate(_)) => "Saving snapshot",
            Self::Operation(_, Operation::SnapshotRestore(_)) => "Restoring snapshot",
            Self::Operation(_, Operation::Sequence(_)) => "Saving hardware",
            Self::Operation(
                _,
                Operation::MemoryDump {
                    ..
                },
            ) => "Saving memory dump",
            Self::Operation(..) => "Applying changes",
            Self::Action(_, action, _) => match action {
                Action::Start => "Starting machine",
                Action::Shutdown => "Requesting shutdown",
                Action::Pause => "Pausing machine",
                Action::Resume => "Resuming machine",
                Action::Display => "Opening display",
            },
        }
    }
}
pub(crate) struct LibraryUpdate {
    pub(crate) sampled_at: Instant,
    pub(crate) machines: Result<backend::MachineList, String>,
    pub(crate) select_name: Option<String>,
    pub(crate) details_for: Option<String>,
}

pub(crate) enum JobEvent {
    OperationFinished {
        outcome: Result<(), String>,
        display: Option<Box<(Machine, ActionSource, Instant)>>,
    },
    Library(Box<LibraryUpdate>),
    Details(String, Box<Result<Details, String>>),
    RefreshFinished(Option<Vec<String>>),
}

pub(crate) struct Refresh {
    pub(crate) expires_at: Instant,
    pub(crate) library_pending: bool,
    pub(crate) details_pending: Option<String>,
}

impl Refresh {
    pub(super) fn new() -> Self {
        Self {
            // Give the worker time to publish its error when the command budget expires.
            expires_at: Instant::now() + REFRESH_BUDGET + Duration::from_secs(2),
            library_pending: true,
            details_pending: None,
        }
    }
}

pub(crate) struct Pending {
    pub(crate) refresh: Option<Refresh>,
    pub(crate) hardware_save: Option<String>,
    pub(crate) disk_rename: Option<(String, String, String)>,
    pub(crate) receiver: Receiver<JobEvent>,
    pub(crate) form: Option<WeakEntity<EditorForm>>,
    pub(crate) action_source: Option<ActionSource>,
    pub(crate) label: &'static str,
    /// Notice for success, from `Job::done`.
    pub(crate) done: String,
    pub(crate) reporter: backend::progress::Reporter,
    pub(crate) progress: backend::progress::Progress,
}
