//! Operation progress published by workers and sampled by the UI.
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) stage: String,
    pub(crate) percent: Option<u8>,
}

#[derive(Clone, Default)]
pub(crate) struct Reporter(Arc<Mutex<Progress>>);

impl Reporter {
    pub(crate) fn update(&self, stage: impl Into<String>, percent: Option<u8>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Progress {
            stage: stage.into(),
            percent,
        };
    }

    pub(crate) fn snapshot(&self) -> Progress {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
