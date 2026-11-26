use crate::app::jobs::{LibraryUpdate, Refresh};
use crate::app::{ActionSource, Job, JobEvent, Pending, Workspace};
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
            if refresh {
                let _ = start_failure.send(JobEvent::Library(Box::new(LibraryUpdate {
                    sampled_at: Instant::now(),
                    machines: Err("Could not refresh the library. Retry in a moment.".into()),
                    select_name: None,
                    details_for: None,
                })));
            } else {
                let _ = start_failure.send(JobEvent::OperationFinished {
                    outcome: Err("Could not start the operation. Retry in a moment.".into()),
                    display: None,
                });
            }
            let _ = start_failure.send(JobEvent::RefreshFinished(None));
        }
        self.pending = Some(Pending {
            refresh: refresh.then(Refresh::new),
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
        loop {
            match self.pending.as_ref().map(|pending| pending.receiver.try_recv()) {
                Some(Ok(JobEvent::OperationFinished {
                    outcome,
                    display,
                })) => {
                    if let Some(mut pending) = self.pending.take() {
                        self.finish_operation(&mut pending, outcome, window, cx);
                        pending.form = None;
                        pending.refresh = Some(Refresh::new());
                        self.pending = Some(pending);
                        if let Some(display) = display {
                            let (machine, source, completed_at) = *display;
                            self.machine_started(machine, source, completed_at, cx);
                        }
                        cx.notify();
                    }
                }
                Some(Ok(JobEvent::Library(update))) => self.apply_library(*update, window, cx),
                Some(Ok(JobEvent::Details(id, details))) => {
                    if let Some(refresh) = self.pending.as_mut().and_then(|p| p.refresh.as_mut()) {
                        refresh.details_pending = None;
                    }
                    self.apply_details(id, *details);
                    cx.notify();
                }
                Some(Ok(JobEvent::RefreshFinished(architectures))) => {
                    self.pending = None;
                    if let Some(architectures) = architectures.filter(|a| !a.is_empty()) {
                        self.architectures = architectures;
                    }
                    if self.loaded
                        && self.selected.is_some()
                        && self.details_for != self.selected
                        && self.library_error.is_none()
                    {
                        self.begin(Job::Refresh, None, cx);
                    }
                    cx.notify();
                    return;
                }
                Some(Err(mpsc::TryRecvError::Empty)) => {
                    if self
                        .pending
                        .as_ref()
                        .and_then(|p| p.refresh.as_ref())
                        .is_some_and(|refresh| Instant::now() >= refresh.expires_at)
                    {
                        self.refresh_interrupted("Timed out refreshing the library.", window, cx);
                    }
                    return;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    if self.busy() {
                        if let Some(pending) = self.pending.take() {
                            eprintln!(
                                "{}: background worker stopped before reporting a result",
                                pending.label
                            );
                            report_failure(
                                &pending,
                                "The operation stopped before reporting a result. Check the machine and any output files before trying again.".into(),
                                window,
                                cx,
                            );
                            self.begin(Job::Refresh, None, cx);
                            cx.notify();
                        }
                    } else {
                        self.refresh_interrupted(
                            "Library refresh stopped unexpectedly.",
                            window,
                            cx,
                        );
                    }
                    return;
                }
                None => return,
            }
        }
    }

    fn machine_started(
        &mut self,
        mut machine: backend::Machine,
        source: ActionSource,
        completed_at: Instant,
        cx: &mut Context<Self>,
    ) {
        self.last_library_sample =
            Some(self.last_library_sample.map_or(completed_at, |sample| sample.max(completed_at)));
        if let Some((sampled_at, statuses)) = &self.machine_statuses {
            if *sampled_at > completed_at {
                statuses.apply(&mut machine);
            } else {
                self.machine_statuses = None;
            }
        }
        if let Some(current) = self.machines.iter_mut().find(|m| m.id == machine.id) {
            current.state = machine.state;
            current.saved_state = machine.saved_state;
        }
        if !matches!(machine.state, State::Running | State::Paused) {
            return;
        }
        match source {
            ActionSource::Library => self.open_machine_display(machine, cx),
            ActionSource::Viewer(handle) => {
                if let Some((_, _, _, view)) =
                    self.display_windows.iter().find(|(_, _, window, _)| *window == handle)
                {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = view.update(cx, |view, cx| view.ensure_session(window, cx));
                    });
                }
            }
        }
    }

    fn refresh_interrupted(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(refresh) = self.pending.take().and_then(|pending| pending.refresh) {
            if refresh.library_pending {
                self.report_library_error(message.into(), window, cx);
                self.loaded = true;
            } else if let Some(id) = refresh.details_pending {
                self.apply_details(
                    id,
                    Err("Could not finish loading machine details. Try again.".into()),
                );
            }
            cx.notify();
        }
    }

    fn apply_library(
        &mut self,
        update: LibraryUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let LibraryUpdate {
            sampled_at,
            machines,
            select_name,
            details_for,
        } = update;
        self.loaded = true;
        if let Some(refresh) = self.pending.as_mut().and_then(|p| p.refresh.as_mut()) {
            refresh.library_pending = false;
            refresh.details_pending = details_for;
        }
        let previous_selection = self.selected.clone();
        match machines {
            Ok(list) => {
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
                if let Some(name) = select_name
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
            Err(error) => self.report_library_error(error, window, cx),
        }
        if self.selected != previous_selection {
            self.details = Default::default();
            self.details_for = None;
            self.details_error = None;
            self.hardware_draft = None;
            self.memory_invalid = false;
            self.saved_hardware_for = None;
        }
        cx.notify();
    }

    fn report_library_error(&mut self, error: String, window: &mut Window, cx: &mut App) {
        if !self.machines.is_empty() && self.library_error.as_ref() != Some(&error) {
            window.push_notification(Notification::error(crate::ui::plain_error(&error)), cx);
        }
        self.library_error = Some(error);
    }

    fn apply_details(&mut self, id: String, details: Result<crate::operations::Details, String>) {
        if self.selected.as_ref() != Some(&id) {
            return;
        }
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
            Err(error) => self.details_error = Some(error),
        }
    }

    fn finish_operation(
        &mut self,
        pending: &mut Pending,
        outcome: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = outcome {
            report_failure(pending, error, window, cx);
            return;
        }
        if pending.hardware_save.is_some() && pending.hardware_save == self.selected {
            self.saved_hardware_for = pending.hardware_save.take();
        }
        if let Some((id, old, new)) = pending.disk_rename.take()
            && let Some(draft) = self.hardware_draft.as_mut().filter(|draft| draft.id == id)
        {
            draft.rename_boot_drive(&old, &new);
        }
        self.notice = Some(pending.done.clone());
        let open = pending.form.take().is_some_and(|form| {
            form.update(cx, |form, cx| {
                form.pending = false;
                cx.notify();
                form.open
            })
            .unwrap_or(false)
        });
        if open {
            window.close_dialog(cx);
        }
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

#[cfg(test)]
mod tests {
    use super::{JobEvent, LibraryUpdate, Pending, Workspace, mpsc};
    use crate::app::Preferences;
    use crate::backend::{MachineList, State};
    use gpui_kit::{AppContext, WindowOptions};
    use std::time::{Duration, Instant};

    #[test]
    fn operations_finish_even_when_machine_details_stall() {
        gpui_kit::platform::headless().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
            gpui_kit::init(cx);
            let (handle, workspace) =
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| {
                        Workspace::new(true, None, Preferences::default(), None, window, cx)
                    })
                })
                .unwrap();
            handle
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        for (label, done) in [
                            ("Creating machine", "Machine created"),
                            ("Starting machine", "Machine starting"),
                            ("Requesting shutdown", "Shutdown requested"),
                        ] {
                            let (tx, receiver) = mpsc::channel();
                            workspace.pending = Some(Pending {
                                receiver,
                                refresh: None,
                                hardware_save: None,
                                disk_rename: None,
                                form: None,
                                action_source: None,
                                label,
                                done: done.into(),
                                reporter: Default::default(),
                                progress: Default::default(),
                            });
                            assert!(workspace.busy());

                            tx.send(JobEvent::OperationFinished {
                                outcome: Ok(()),
                                display: None,
                            })
                            .unwrap();
                            workspace.poll(window, cx);
                            assert!(!workspace.busy());
                            assert_eq!(workspace.notice.take().as_deref(), Some(done));

                            let id = workspace.selected.clone().unwrap();
                            let mut machines = workspace.machines.clone();
                            machines.iter_mut().find(|machine| machine.id == id).unwrap().state =
                                if label == "Starting machine" {
                                    State::Running
                                } else {
                                    State::Stopped
                                };
                            tx.send(JobEvent::Library(Box::new(LibraryUpdate {
                                sampled_at: Instant::now(),
                                machines: Ok(MachineList {
                                    machines,
                                    unavailable: 0,
                                }),
                                select_name: None,
                                details_for: Some(id),
                            })))
                            .unwrap();
                            workspace.poll(window, cx);
                            assert_eq!(
                                workspace.selected_machine().unwrap().status(),
                                if label == "Starting machine" {
                                    "Running"
                                } else {
                                    "Powered off"
                                }
                            );
                            assert!(!workspace.busy());

                            // A stalled read can be discarded while the command result is retained.
                            let refresh =
                                workspace.pending.as_mut().unwrap().refresh.as_mut().unwrap();
                            refresh.expires_at = Instant::now();
                            workspace.poll(window, cx);
                            assert!(workspace.pending.is_none());
                            assert!(workspace.details_error.is_some());
                            assert!(workspace.library_error.is_none());
                            assert!(workspace.notice.is_none());
                        }
                    });
                })
                .unwrap();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(1)).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }
}
