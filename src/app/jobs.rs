use crate::{
    appliance,
    backend::{self, Action, Machine},
    forms::EditorForm,
    operations::{Details, Operation},
};
use gpui_kit::WeakEntity;
use std::{path::PathBuf, sync::mpsc::Receiver};
#[derive(Clone)]
pub enum Job {
    Refresh,
    Action(Machine, Action),
    Create(backend::NewMachine),
    Import(PathBuf),
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
impl Job {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Refresh => "Refreshing",
            Self::Icon(..) => "Saving icon",
            Self::Network(_) => "Creating network",
            Self::Create(_) => "Creating machine",
            Self::Import(_) => "Importing machine",
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
            Self::Action(_, action) => match action {
                Action::Start => "Starting machine",
                Action::Shutdown => "Requesting shutdown",
                Action::Pause => "Pausing machine",
                Action::Resume => "Resuming machine",
                Action::Display => "Opening display",
            },
        }
    }
}
pub(crate) struct Response {
    pub(crate) machines: Result<Vec<Machine>, String>,
    pub(crate) details: Option<(String, Result<Details, String>)>,
    pub(crate) outcome: Option<Result<(), String>>,
    pub(crate) select_name: Option<String>,
    /// Read on the first refresh of a connection.
    pub(crate) architectures: Option<Vec<String>>,
}
pub(crate) enum JobEvent {
    DisplayReady(Machine),
    Complete(Box<Response>),
}

pub(crate) struct Pending {
    pub(crate) refresh: bool,
    pub(crate) hardware_save: Option<String>,
    pub(crate) receiver: Receiver<JobEvent>,
    pub(crate) form: Option<WeakEntity<EditorForm>>,
    pub(crate) label: &'static str,
    pub(crate) reporter: backend::progress::Reporter,
    pub(crate) progress: backend::progress::Progress,
}
