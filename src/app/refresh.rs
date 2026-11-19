use crate::app::{Job, JobEvent, Pending, Response, Workspace};
use crate::appliance;
use crate::backend;
use crate::backend::{Action, State};
use crate::forms::EditorForm;
use crate::operations;
use crate::operations::Operation;
use gpui_kit::component::WindowExt;
use gpui_kit::component::notification::Notification;
use gpui_kit::{prelude::*, *};
use std::sync::mpsc;

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
            if let Job::Action(machine, action) = job {
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
        let c = self.preferences.connection;
        let hardware_save = match &job {
            Job::Operation(machine, Operation::Sequence(_)) => Some(machine.id.clone()),
            _ => None,
        };
        let selected = self.selected.clone();
        let cached_machines = matches!(job, Job::ConvertDisk { .. }).then(|| self.machines.clone());
        let previous_ids: Vec<_> = self.machines.iter().map(|m| m.id.clone()).collect();
        let importing = matches!(&job, Job::Import(..) | Job::Appliance(_));
        let need_architectures = self.architectures.is_empty() && !self.demo;
        let (tx, rx) = mpsc::channel();
        if !refresh {
            self.notice = None;
        }
        let reporter = backend::progress::Reporter::default();
        let progress = reporter.clone();
        std::thread::spawn(move || {
            let mut select_name = match &job {
                Job::Create(s) => Some(s.name.trim().to_owned()),
                Job::Operation(
                    _,
                    Operation::Clone(name)
                    | Operation::CloneConfiguration(name)
                    | Operation::Rename(name),
                ) => Some(name.trim().to_owned()),
                Job::Operation(m, Operation::Disposable) => Some(format!("{} Disposable", m.name)),
                _ => None,
            };
            let open_display = match &job {
                Job::Action(machine, Action::Start | Action::Resume) => {
                    let mut machine = machine.clone();
                    machine.state = State::Running;
                    Some(machine)
                }
                _ => None,
            };
            let disk_conversion = matches!(job, Job::ConvertDisk { .. });
            let outcome = match job {
                Job::Refresh | Job::Icon(..) => Ok(()),
                Job::Network(net) => backend::network::create(c, &net),
                Job::Action(m, action) => backend::action(c, &m.id, action, !m.spice_viewer),
                Job::Create(spec) => backend::create(c, &spec),
                Job::Import(path, qemu_build) => backend::import(c, &path, qemu_build),
                Job::ConvertDisk {
                    source,
                    destination,
                    format,
                } => backend::disk_convert::convert(&source, &destination, &format, &progress),
                Job::Appliance(spec) => appliance::import(c, &spec, &progress),
                Job::Operation(m, op) => operations::execute(c, &m, op, &progress),
            }
            .map_err(|e| e.to_string());
            if outcome.is_ok()
                && let Some(machine) = open_display
            {
                let _ = tx.send(JobEvent::DisplayReady(machine));
            }
            if outcome.is_ok() && cached_machines.is_none() && !refresh {
                progress.update("Refreshing library", None);
            }
            let machines = match cached_machines {
                Some(machines) => Ok(machines),
                None => backend::machines(c).map_err(|e| e.to_string()),
            };
            if importing && outcome.is_ok() {
                select_name = machines
                    .as_ref()
                    .ok()
                    .and_then(|list| list.iter().find(|m| !previous_ids.contains(&m.id)))
                    .map(|m| m.name.clone());
            }
            let details = (!disk_conversion).then_some(&machines).and_then(|machines| {
                machines
                    .as_ref()
                    .ok()
                    .and_then(|list| {
                        select_name
                            .as_ref()
                            .and_then(|name| list.iter().find(|m| &m.name == name))
                            .or_else(|| {
                                selected.as_ref().and_then(|id| list.iter().find(|m| &m.id == id))
                            })
                            .or_else(|| list.first())
                    })
                    .map(|machine| {
                        (
                            machine.id.clone(),
                            operations::details(c, &machine.id).map_err(|e| e.to_string()),
                        )
                    })
            });
            let _ = tx.send(JobEvent::Complete(Box::new(Response {
                machines,
                details,
                outcome: (!refresh).then_some(outcome),
                select_name,
                architectures: need_architectures.then(|| operations::available_architectures(c)),
            })));
        });
        self.pending = Some(Pending {
            refresh,
            hardware_save,
            receiver: rx,
            form,
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
            window.push_notification(Notification::error(error), cx);
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
            Some(Ok(JobEvent::DisplayReady(machine))) => {
                self.open_machine_display(machine, cx);
                return;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                if let Some(pending) = self.pending.take() {
                    let error = "The operation was interrupted. Refresh and try again.".to_owned();
                    if let Some(form) = pending.form {
                        let _ = form.update(cx, |form, cx| {
                            form.pending = false;
                            form.error = Some(error.clone());
                            cx.notify();
                        });
                    }
                    if pending.refresh {
                        if self.library_error.as_ref() != Some(&error) {
                            window.push_notification(Notification::error(error.clone()), cx);
                        }
                        self.library_error = Some(error);
                        self.loaded = true;
                    } else {
                        window.push_notification(Notification::error(error), cx);
                    }
                    cx.notify();
                }
                return;
            }
            _ => return,
        };
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
                Err(error) => {
                    if let Some(form) = pending.form {
                        let _ = form.update(cx, |form, cx| {
                            form.pending = false;
                            form.error = Some(error.clone());
                            cx.notify();
                        });
                    }
                    window.push_notification(Notification::error(error), cx);
                }
            }
        }
        if let Some(architectures) = response.architectures.filter(|a| !a.is_empty()) {
            self.architectures = architectures;
        }
        let loaded_list = response.machines.is_ok();
        let previous_selection = self.selected.clone();
        match response.machines {
            Ok(machines) => {
                self.library_error = None;
                self.machines = machines;
                self.apply_icons();
                if let Some(name) = response.select_name {
                    if let Some(m) = self.machines.iter().find(|m| m.name == name) {
                        self.selected = Some(m.id.clone());
                    }
                }
                if !self.machines.iter().any(|m| Some(&m.id) == self.selected.as_ref()) {
                    self.selected = self.machines.first().map(|m| m.id.clone());
                }
                if self.preferences.selected != self.selected {
                    self.preferences.selected = self.selected.clone();
                    self.preferences.save();
                }
            }
            Err(error) => {
                if self.library_error.as_ref() != Some(&error) {
                    window.push_notification(Notification::error(error.clone()), cx);
                }
                self.library_error = Some(error);
            }
        }
        if self.selected != previous_selection {
            self.details = Default::default();
            self.details_for = None;
            self.details_error = None;
            self.hardware_draft = None;
            self.saved_hardware_for = None;
        }
        if let Some((id, details)) = response.details {
            if self.selected.as_ref() == Some(&id) {
                self.details_for = Some(id.clone());
                match details {
                    Ok(details) => {
                        self.details = details;
                        self.details_error = None;
                        if self.saved_hardware_for.as_ref() == Some(&id) {
                            self.hardware_draft = None;
                            self.saved_hardware_for = None;
                        }
                    }
                    Err(error) => {
                        self.details_error = Some(error);
                    }
                }
            }
        }
        if loaded_list && self.selected.is_some() && self.details_for != self.selected {
            self.begin(Job::Refresh, None, cx);
        }
        cx.notify();
    }
}
