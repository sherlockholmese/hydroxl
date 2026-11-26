use crate::app::{ActionSource, Job, JobEvent, Pending, Response, Workspace};
use crate::backend;
use crate::backend::{Action, State};
use crate::forms::EditorForm;
use crate::operations::Operation;
use gpui_kit::component::WindowExt;
use gpui_kit::component::notification::Notification;
use gpui_kit::{prelude::*, *};
use std::sync::mpsc;
use std::time::Instant;

impl Workspace {
    pub(crate) fn begin(
        &mut self,
        job: Job,
        form: Option<WeakEntity<EditorForm>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let refresh = matches!(job, Job::Refresh);
        if self.busy() || (refresh && self.pending.is_some()) {
            return false;
        }
        if self.demo {
            if let Job::Action(machine, action, _) = job {
                if let Some(m) = self.machines.iter_mut().find(|m| m.id == machine.id) {
                    match action {
                        Action::Start | Action::Resume => m.state = State::Running,
                        Action::Pause => m.state = State::Paused,
                        Action::Shutdown => m.state = State::Stopped,
                        Action::Display => {
                            self.notice = Some("Display is unavailable in preview.".into())
                        }
                    }
                }
            } else if !matches!(job, Job::Refresh) {
                self.error =
                    Some("Choose a live connection in Preferences to make changes.".into());
            }
            cx.notify();
            return false;
        }
        let label = job.label();
        let done = job.done();
        let action_source = match &job {
            Job::Action(_, _, source) => Some(*source),
            _ => None,
        };
        let c = self.preferences.connection;
        let hardware_save = match &job {
            Job::Operation(machine, Operation::Sequence(_)) => Some(machine.id.clone()),
            _ => None,
        };
        let disk_rename = match &job {
            Job::Operation(
                machine,
                Operation::DiskInterface {
                    target,
                    new_target,
                    ..
                },
            ) if target != new_target => {
                Some((machine.id.clone(), target.clone(), new_target.clone()))
            }
            _ => None,
        };
        let selected = self.selected.clone();
        let previous = self.machines.clone();
        let need_architectures = self.architectures.is_empty();
        let (tx, rx) = mpsc::channel();
        if !refresh {
            self.notice = None;
        }
        let reporter = backend::progress::Reporter::default();
        let progress = reporter.clone();
        let start_failure = tx.clone();
        let worker = std::thread::Builder::new().name("hydroxl-job".into()).spawn(move || {
            super::worker::run(job, c, selected, previous, need_architectures, progress, tx);
        });
        if let Err(error) = worker {
            eprintln!("Could not start background work ({label}): {error}");
            let message = if refresh {
                "Could not refresh the library. Retry in a moment."
            } else {
                "Could not start the operation. Retry in a moment."
            };
            let _ = start_failure.send(JobEvent::Complete(Box::new(Response {
                machines: refresh.then(|| (Instant::now(), Err(message.into()))),
                details: None,
                outcome: (!refresh).then(|| Err(message.into())),
                select_name: None,
                architectures: None,
            })));
        }
        self.pending = Some(Pending {
            refresh,
            hardware_save,
            disk_rename,
            receiver: rx,
            form,
            action_source,
            label,
            done,
            reporter,
            progress: Default::default(),
        });
        if !refresh {
            cx.notify();
        }
        true
    }

    pub(crate) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Test hook: open the selected machine's display once the library has loaded.
        if self.loaded
            && std::env::var_os("HYDROXL_OPEN_DISPLAY").is_some()
            && self.display_windows.is_empty()
            && let Some(machine) = self.selected_machine().cloned()
            && machine.state == crate::backend::State::Running
        {
            self.open_machine_display(machine, cx);
        }
        if let Some(notice) = self.notice.take() {
            window.push_notification(notice, cx);
        }
        if let Some(error) = self.error.take() {
            window.push_notification(Notification::error(crate::ui::plain_error(&error)), cx);
        }
        if let Some(pending) = &mut self.pending {
            let progress = pending.reporter.snapshot();
            if pending.progress != progress {
                pending.progress = progress;
                if let Some(form) = &pending.form {
                    let _ = form.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            }
        }
        let response = match self.pending.as_ref().map(|p| p.receiver.try_recv()) {
            Some(Ok(JobEvent::Complete(response))) => response,
            Some(Ok(JobEvent::DisplayReady(machine, source))) => {
                self.last_library_sample = Some(Instant::now());
                self.machine_statuses = None;
                if let Some(current) = self.machines.iter_mut().find(|m| m.id == machine.id) {
                    current.state = machine.state;
                    current.saved_state = false;
                    cx.notify();
                }
                match source {
                    ActionSource::Library => self.open_machine_display(*machine, cx),
                    ActionSource::Viewer(handle) => {
                        if let Some((_, _, view)) =
                            self.display_windows.iter().find(|(_, window, _)| *window == handle)
                        {
                            let _ = handle.update(cx, |_, window, cx| {
                                let _ = view.update(cx, |view, cx| view.ensure_session(window, cx));
                            });
                        }
                    }
                }
                return;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                if let Some(pending) = self.pending.take() {
                    eprintln!(
                        "{}: background worker stopped before reporting a result",
                        pending.label
                    );
                    if pending.refresh {
                        let error =
                            "Library refresh stopped unexpectedly. Select Retry to reconnect."
                                .to_owned();
                        if !self.machines.is_empty() && self.library_error.as_ref() != Some(&error)
                        {
                            window.push_notification(Notification::error(error.clone()), cx);
                        }
                        self.library_error = Some(error);
                        self.loaded = true;
                    } else {
                        let error = "The operation stopped before reporting a result. Check the machine and any output files before trying again.".to_owned();
                        report_failure(&pending, error, window, cx);
                        self.begin(Job::Refresh, None, cx);
                    }
                    cx.notify();
                }
                return;
            }
            _ => return,
        };
        let response = *response;
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.loaded = true;
        if let Some(outcome) = response.outcome {
            match outcome {
                Ok(()) => {
                    if pending.hardware_save.is_some() && pending.hardware_save == self.selected {
                        self.saved_hardware_for = pending.hardware_save;
                    }
                    if let Some((id, old, new)) = pending.disk_rename
                        && let Some(draft) =
                            self.hardware_draft.as_mut().filter(|draft| draft.id == id)
                    {
                        draft.rename_boot_drive(&old, &new);
                    }
                    self.notice = Some(pending.done.clone());
                    if let Some(form) = pending.form {
                        let open = form
                            .update(cx, |form, cx| {
                                form.pending = false;
                                cx.notify();
                                form.open
                            })
                            .unwrap_or(false);
                        if open {
                            window.close_dialog(cx);
                        }
                    }
                }
                Err(error) => report_failure(&pending, error, window, cx),
            }
        }
        if let Some(architectures) = response.architectures.filter(|a| !a.is_empty()) {
            self.architectures = architectures;
        }
        let loaded_list = matches!(response.machines, Some((_, Ok(_))));
        let previous_selection = self.selected.clone();
        match response.machines {
            Some((sampled_at, Ok(list))) => {
                let mut machines = list.machines;
                self.last_library_sample = Some(
                    self.last_library_sample
                        .map_or(sampled_at, |previous| previous.max(sampled_at)),
                );
                if let Some((time, statuses)) = &self.machine_statuses
                    && *time > sampled_at
                {
                    for machine in &mut machines {
                        statuses.apply(machine);
                    }
                }
                self.library_error = match list.unavailable {
                    0 => None,
                    1 => Some("Could not refresh one machine.".into()),
                    count => Some(format!("Could not refresh {count} machines.")),
                };
                self.machines = machines;
                self.apply_icons();
                if let Some(name) = response.select_name
                    && let Some(m) = self.machines.iter().find(|m| m.name == name)
                {
                    self.selected = Some(m.id.clone());
                }
                if !self.machines.iter().any(|m| Some(&m.id) == self.selected.as_ref()) {
                    self.selected = self.machines.first().map(|m| m.id.clone());
                }
                if self.preferences.selected != self.selected {
                    self.preferences.selected = self.selected.clone();
                    self.save_preferences();
                }
            }
            None => {}
            Some((_, Err(error))) => {
                if !self.machines.is_empty() && self.library_error.as_ref() != Some(&error) {
                    window
                        .push_notification(Notification::error(crate::ui::plain_error(&error)), cx);
                }
                self.library_error = Some(error);
            }
        }
        if self.selected != previous_selection {
            self.details = Default::default();
            self.details_for = None;
            self.details_error = None;
            self.hardware_draft = None;
            self.memory_invalid = false;
            self.saved_hardware_for = None;
        }
        if let Some((id, details)) = response.details
            && self.selected.as_ref() == Some(&id)
        {
            self.details_for = Some(id.clone());
            match details {
                Ok(details) => {
                    self.details = details;
                    self.details_error = None;
                    if self.saved_hardware_for.as_ref() == Some(&id) {
                        self.hardware_draft = None;
                        self.memory_invalid = false;
                        self.saved_hardware_for = None;
                    }
                }
                Err(error) => {
                    self.details_error = Some(error);
                }
            }
        }
        if loaded_list && self.selected.is_some() && self.details_for != self.selected {
            self.begin(Job::Refresh, None, cx);
        }
        cx.notify();
    }
}

fn report_failure(pending: &Pending, error: String, window: &mut Window, cx: &mut App) {
    let displayed_in_form = pending.form.as_ref().is_some_and(|form| {
        form.update(cx, |form, cx| {
            form.job_failed(error.clone());
            cx.notify();
            form.open
        })
        .unwrap_or(false)
    });
    if !displayed_in_form {
        show_job_error(error, pending.action_source, window, cx);
    }
}

fn show_job_error(error: String, source: Option<ActionSource>, window: &mut Window, cx: &mut App) {
    if let Some(ActionSource::Viewer(handle)) = source
        && handle
            .update(cx, |_, window, cx| {
                window.push_notification(Notification::error(crate::ui::plain_error(&error)), cx)
            })
            .is_ok()
    {
        return;
    }
    window.push_notification(Notification::error(crate::ui::plain_error(&error)), cx);
}
