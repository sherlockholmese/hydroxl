use crate::app::{Job, Preferences, Workspace};
use crate::backend;
use crate::backend::Machine;
use crate::operations::Details;
use crate::workflows;
use gpui_kit::component::input::{self, InputState};
use gpui_kit::{prelude::*, *};
use std::{path::PathBuf, time::Duration};

impl Workspace {
    pub(crate) fn new(
        demo: bool,
        initial: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = Preferences::load();
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
            selected: preferences.selected.clone(),
            preferences,
            details: Details::default(),
            details_error: None,
            details_for: None,
            search,
            memory_input,
            pending: None,
            library_error: None,
            error: None,
            notice: None,
            loaded: demo,
            demo,
            tab: 0,
            library_open: false,
            hardware_draft: None,
            saved_hardware_for: None,
            display_windows: Vec::new(),
            live_preview: None,
            share_os: None,
            wide: false,
            hardware_section: 0,
            hardware_section_from: 0,
            architectures: if demo {
                vec!["x86_64".into(), "i686".into(), "aarch64".into(), "riscv64".into()]
            } else {
                Vec::new()
            },
            focus: cx.focus_handle(),
        };
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
        this
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
        self.preferences.save();
        self.apply_icons();
        cx.notify();
    }

    pub(crate) fn set_icon(&mut self, id: String, key: String, cx: &mut Context<Self>) {
        self.preferences.icons.retain(|(existing, _)| *existing != id);
        if key != "auto" {
            self.preferences.icons.push((id, key));
        }
        self.preferences.save();
        self.apply_icons();
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
            self.saved_hardware_for = None;
            self.details_error = None;
            self.share_os = None;
            self.preferences.selected = self.selected.clone();
            self.preferences.save();
            if !self.demo {
                self.begin(Job::Refresh, None, cx);
            }
        }
        self.library_open = false;
        cx.notify();
    }
}
