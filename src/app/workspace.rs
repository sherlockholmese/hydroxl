use crate::app::{Job, Preferences, Workspace};
use crate::backend;
use crate::backend::Machine;
use crate::operations::Details;
use crate::workflows;
use gpui_kit::component::input::{self, InputState};
use gpui_kit::{prelude::*, *};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

impl Workspace {
    pub(crate) fn new(
        demo: bool,
        initial: Option<PathBuf>,
        preferences: Preferences,
        preference_error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search machines"));
        cx.subscribe(&search, |_, _, _: &input::InputEvent, cx| cx.notify()).detach();
        let memory_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(&memory_input, |this: &mut Self, _, event: &input::InputEvent, cx| {
            if matches!(event, input::InputEvent::Change) {
                this.memory_typed(cx);
            }
        })
        .detach();
        let mut this = Self {
            machines: if demo {
                backend::demo_machines()
            } else {
                Vec::new()
            },
            machine_statuses: None,
            last_library_sample: None,
            selected: preferences.selected.clone(),
            preferences,
            details: Details::default(),
            details_error: None,
            details_for: None,
            search,
            memory_input,
            memory_invalid: false,
            pending: None,
            library_error: None,
            error: preference_error,
            notice: None,
            loaded: demo,
            demo,
            tab: 0,
            library_open: false,
            hardware_draft: None,
            saved_hardware_for: None,
            display_windows: Vec::new(),
            opening_displays: Default::default(),
            live_preview: None,
            share_os: None,
            wide: false,
            hardware_section: crate::theme::motion::Slide::at(0),
            save_bar: crate::theme::motion::Presence::new(false),
            pane_swapped: None,
            architectures: if demo {
                vec!["x86_64".into(), "i686".into(), "aarch64".into(), "riscv64".into()]
            } else {
                Vec::new()
            },
            focus: cx.focus_handle(),
        };
        this.focus.focus(window, cx);
        this.apply_icons();
        if demo {
            this.selected = this.machines.first().map(|m| m.id.clone());
            this.details = this.selected_machine().map(Details::demo).unwrap_or_default();
        } else {
            this.begin(Job::Refresh, None, cx);
        }
        if let Some(path) = initial {
            cx.defer_in(window, move |this, window, cx| {
                this.open_form(
                    workflows::import_path(path, this.preferences.machines_dir()),
                    window,
                    cx,
                )
            });
        }
        let mut refresh_ticks = 0u32;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let result = this.update_in(cx, |this, window, cx| {
                    this.poll(window, cx);
                    refresh_ticks += 1;
                    if refresh_ticks >= 54 {
                        refresh_ticks = 0;
                        if !this.demo && this.pending.is_none() {
                            this.begin(Job::Refresh, None, cx);
                        }
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();
        if !demo {
            this.watch_machine_statuses(cx);
        }
        this
    }

    pub(crate) fn watch_machine_statuses(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(connection) = this.update(cx, |this, _| {
                    (!this.machines.is_empty()).then_some(this.preferences.connection)
                }) else {
                    break;
                };
                let Some(connection) = connection else {
                    continue;
                };
                let sampled_at = Instant::now();
                let Ok(statuses) = cx
                    .background_executor()
                    .spawn(async move { backend::MachineStatuses::read(connection) })
                    .await
                else {
                    continue;
                };
                if this
                    .update(cx, |this, cx| {
                        if this.preferences.connection != connection
                            || this.last_library_sample.is_some_and(|time| time >= sampled_at)
                        {
                            return;
                        }
                        let mut changed = false;
                        for machine in &mut this.machines {
                            changed |= statuses.apply(machine);
                        }
                        this.machine_statuses = Some((sampled_at, statuses));
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn selected_machine(&self) -> Option<&Machine> {
        self.selected.as_ref().and_then(|id| self.machines.iter().find(|m| &m.id == id))
    }

    /// Applies per-machine preferences to loaded machines.
    pub(crate) fn apply_icons(&mut self) {
        for machine in &mut self.machines {
            machine.icon = self
                .preferences
                .icons
                .iter()
                .find(|(id, _)| *id == machine.id)
                .map(|(_, key)| key.clone())
                .unwrap_or_default();
            machine.spice_viewer = !self.preferences.vnc.contains(&machine.id);
        }
    }

    pub(crate) fn set_spice_viewer(&mut self, id: String, spice: bool, cx: &mut Context<Self>) {
        if spice {
            self.preferences.vnc.remove(&id);
        } else {
            self.preferences.vnc.insert(id);
        }
        self.save_preferences();
        self.apply_icons();
        cx.notify();
    }

    pub(crate) fn set_icon(
        &mut self,
        id: String,
        key: String,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let mut preferences = self.preferences.clone();
        preferences.icons.retain(|(existing, _)| *existing != id);
        if key != "auto" {
            preferences.icons.push((id, key));
        }
        preferences.save()?;
        self.preferences = preferences;
        self.apply_icons();
        cx.notify();
        Ok(())
    }

    pub(crate) fn save_preferences(&mut self) {
        if let Err(error) = self.preferences.save() {
            let message = format!("{error:#}");
            eprintln!("{message}");
            self.error = Some(message);
        }
    }

    pub(crate) fn switch_connection(
        &mut self,
        connection: backend::Connection,
        cx: &mut Context<Self>,
    ) {
        if self.busy() || (!self.demo && self.preferences.connection == connection) {
            return;
        }
        if std::mem::replace(&mut self.demo, false) {
            self.watch_machine_statuses(cx);
        }
        self.preferences.connection = connection;
        self.save_preferences();
        self.machines.clear();
        self.machine_statuses = None;
        self.last_library_sample = Some(std::time::Instant::now());
        self.selected = None;
        self.details = Details::default();
        if let Some((_, preview)) = self.live_preview.take() {
            cx.defer(move |cx| cx.drop_image(preview, None));
        }
        self.details_for = None;
        self.details_error = None;
        self.hardware_draft = None;
        self.memory_invalid = false;
        self.saved_hardware_for = None;
        self.pending = None;
        self.architectures.clear();
        self.library_error = None;
        self.loaded = false;
        self.begin(Job::Refresh, None, cx);
        cx.notify();
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| !pending.refresh)
    }

    pub(crate) fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.details = if self.demo {
                self.selected_machine().map(Details::demo).unwrap_or_default()
            } else {
                Details::default()
            };
            self.details_for = None;
            self.hardware_draft = None;
            self.memory_invalid = false;
            self.saved_hardware_for = None;
            self.details_error = None;
            self.share_os = None;
            // A different machine is a new view, not navigation: nothing slides.
            self.hardware_section.settle();
            self.preferences.selected = self.selected.clone();
            self.save_preferences();
            if !self.demo {
                self.begin(Job::Refresh, None, cx);
            }
        }
        if self.library_open {
            self.pane_swapped = Some(std::time::Instant::now());
        }
        self.library_open = false;
        cx.notify();
    }
}
