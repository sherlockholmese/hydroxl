use super::schema::{Check, Choice, Derive, FieldSpec, FormSpec, Options, Picker, Submit, Visible};
use crate::app::Workspace;
use gpui_kit::component::{
    IndexPath, WindowExt,
    input::{self, InputState, TextareaState},
    searchable_list::SearchableVec,
    select::{SelectEvent, SelectState},
};
use gpui_kit::{prelude::*, *};
#[derive(Clone)]
pub(super) enum Control {
    Checkbox(bool),
    Text(Entity<InputState>),
    Select(Entity<SelectState<SearchableVec<Choice>>>),
    Multiline(Entity<TextareaState>),
}
#[derive(Clone)]
pub(super) struct Field {
    pub(super) spec: FieldSpec,
    pub(super) control: Control,
}

pub struct EditorForm {
    pub(super) fields: Vec<Field>,
    pub(super) submit: Submit,
    pub(super) action: &'static str,
    pub pending: bool,
    pub error: Option<String>,
    /// The field the error belongs to, when validation could name one.
    pub(super) error_field: Option<usize>,
    pub open: bool,
    pub(super) parent: WeakEntity<Workspace>,
    pub(super) destructive: bool,
    pub(super) unavailable: bool,
    /// Wizard step titles, review excluded. Empty for single-page forms.
    pub(super) steps: Vec<&'static str>,
    pub(super) step: usize,
    pub(super) advanced_open: bool,
    derive: Option<Derive>,
    check: Option<Check>,
    options: Option<Options>,
    visible: Option<Visible>,
    /// Last value the system proposed per field. A field whose value differs was edited by
    /// the user and is never overwritten.
    derived: Vec<String>,
}
impl EditorForm {
    pub(super) fn new(
        spec: FormSpec,
        parent: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let derived = spec.fields.iter().map(|f| f.value.clone()).collect();
        let fields: Vec<Field> =
            spec.fields
                .into_iter()
                .map(|spec| {
                    let control = if spec.checkbox {
                        Control::Checkbox(spec.value == "true")
                    } else if spec.multiline {
                        Control::Multiline(cx.new(|cx| {
                            TextareaState::new(window, cx).default_value(spec.value.clone())
                        }))
                    } else if !spec.choices.is_empty() {
                        let selected =
                            spec.choices.iter().position(|c| c.value == spec.value).unwrap_or(0);
                        Control::Select(cx.new(|cx| {
                            SelectState::new(
                                SearchableVec::new(spec.choices.clone()),
                                Some(IndexPath::new(selected)),
                                window,
                                cx,
                            )
                            .searchable(spec.choices.len() > 12 && spec.choices.len() <= 300)
                        }))
                    } else {
                        Control::Text(cx.new(|cx| {
                            InputState::new(window, cx).default_value(spec.value.clone())
                        }))
                    };
                    Field {
                        spec,
                        control,
                    }
                })
                .collect();
        if spec.derive.is_some() || spec.options.is_some() {
            for field in &fields {
                match &field.control {
                    Control::Text(state) => cx
                        .subscribe_in(state, window, |this, _, event: &input::InputEvent, w, cx| {
                            if matches!(event, input::InputEvent::Change) {
                                this.changed(w, cx);
                            }
                        })
                        .detach(),
                    Control::Select(state) => cx
                        .subscribe_in(state, window, |this, _, _: &SelectEvent<_>, w, cx| {
                            this.changed(w, cx)
                        })
                        .detach(),
                    Control::Multiline(_) | Control::Checkbox(_) => {}
                }
            }
        }
        let mut this = Self {
            fields,
            submit: spec.submit,
            action: spec.action,
            pending: false,
            error: None,
            error_field: None,
            open: true,
            parent,
            destructive: spec.destructive,
            unavailable: spec.unavailable,
            steps: spec.steps,
            step: 0,
            advanced_open: false,
            derive: spec.derive,
            check: spec.check,
            options: spec.options,
            visible: spec.visible,
            derived,
        };
        this.changed(window, cx);
        this
    }

    pub(super) fn values(&self, cx: &App) -> Vec<String> {
        self.fields
            .iter()
            .map(|field| match &field.control {
                Control::Checkbox(checked) => checked.to_string(),
                Control::Text(state) => state.read(cx).value().to_string(),
                Control::Multiline(state) => state.read(cx).value().to_string(),
                Control::Select(state) => {
                    state.read(cx).selected_value().cloned().unwrap_or_default()
                }
            })
            .collect()
    }

    /// Applies derived proposals to fields the user has not changed, until stable.
    pub(super) fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for _ in 0..4 {
            let mut applied = self.refresh_options(window, cx);
            let values = self.values(cx);
            let proposals = self.derive.as_ref().map(|d| d(&values)).unwrap_or_default();
            for (index, proposal) in proposals {
                let Some(current) = values.get(index) else {
                    eprintln!("Form proposed a value for an unknown field {index}");
                    continue;
                };
                if *current != self.derived[index] || *current == proposal {
                    continue;
                }
                self.set(index, &proposal, window, cx);
                self.derived[index] = proposal;
                applied = true;
            }
            if !applied {
                break;
            }
        }
        cx.notify();
    }

    /// Replaces dependent choices. Keeps the current value when still offered, otherwise
    /// selects the first choice and records it as system-chosen.
    fn refresh_options(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(options) = self.options.clone() else {
            return false;
        };
        let mut changed = false;
        for index in 0..self.fields.len() {
            let values = self.values(cx);
            let Some(list) = options(index, &values) else {
                continue;
            };
            let Control::Select(state) = &self.fields[index].control else {
                continue;
            };
            let choices: Vec<Choice> = list
                .into_iter()
                .map(|(value, label)| Choice {
                    value,
                    label,
                })
                .collect();
            if choices.iter().map(|c| &c.value).eq(self.fields[index]
                .spec
                .choices
                .iter()
                .map(|c| &c.value))
            {
                continue;
            }
            let keep = choices.iter().any(|c| c.value == values[index]);
            let pick = if keep {
                values[index].clone()
            } else {
                choices.first().map(|c| c.value.clone()).unwrap_or_default()
            };
            let state = state.clone();
            self.fields[index].spec.choices = choices.clone();
            state.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(choices), window, cx);
                s.set_selected_value(&pick, window, cx);
            });
            if self.derived[index] == values[index] {
                self.derived[index] = pick;
            }
            changed = true;
        }
        changed
    }

    fn set(&mut self, index: usize, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        match &mut self.fields[index].control {
            Control::Text(state) => {
                state.update(cx, |s, cx| s.set_value(value.to_owned(), window, cx))
            }
            Control::Select(state) => {
                state.update(cx, |s, cx| s.set_selected_value(&value.to_owned(), window, cx))
            }
            Control::Multiline(_) => {}
            Control::Checkbox(checked) => *checked = value == "true",
        }
    }

    pub(super) fn shown(&self, index: usize, values: &[String]) -> bool {
        self.visible.as_ref().is_none_or(|v| v(index, values))
    }

    /// Records a validation failure. One that names its field opens Advanced when the
    /// field is there, and moves a wizard back to the field's step.
    fn fail(&mut self, error: anyhow::Error) {
        self.error = Some(error.to_string());
        self.error_field = error
            .downcast_ref::<super::FieldError>()
            .map(|e| e.field)
            .filter(|&i| i < self.fields.len());
        if let Some(i) = self.error_field {
            let spec = &self.fields[i].spec;
            if spec.advanced {
                self.advanced_open = true;
            }
            if !self.steps.is_empty() {
                self.step = spec.step;
            }
        }
    }

    pub(crate) fn job_failed(&mut self, error: String) {
        self.pending = false;
        self.error = Some(error);
        self.error_field = None;
    }

    pub(super) fn is_review(&self) -> bool {
        !self.steps.is_empty() && self.step == self.steps.len()
    }

    pub(super) fn next(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pending || self.unavailable || self.is_review() {
            return;
        }
        let values = self.values(cx);
        match self.check.as_ref().map_or(Ok(()), |check| check(self.step, &values)) {
            Ok(()) => {
                self.error = None;
                self.error_field = None;
                self.step += 1;
                self.advanced_open = false;
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    pub(super) fn back(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.step = self.step.saturating_sub(1);
        self.error = None;
        self.error_field = None;
        cx.notify();
    }

    pub(super) fn submit(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending || self.unavailable {
            return;
        }
        let values = self.values(cx);
        match (self.submit)(&values) {
            Ok(crate::app::Job::Icon(id, key)) => {
                match self.parent.update(cx, |parent, cx| parent.set_icon(id, key, cx)) {
                    Ok(Ok(())) => window.close_dialog(cx),
                    Ok(Err(error)) => self.fail(error),
                    Err(_) => self.fail(anyhow::anyhow!(
                        "Could not save the icon. Reopen Hydroxl and try again."
                    )),
                }
            }
            Ok(job) => {
                self.error = None;
                self.error_field = None;
                let form = cx.entity().downgrade();
                match self.parent.update(cx, |parent, cx| {
                    if parent.demo {
                        return Err("Choose a live connection in Preferences to make changes.");
                    }
                    if !parent.begin(job, Some(form), cx) {
                        return Err("Wait for the current operation to finish.");
                    }
                    Ok(())
                }) {
                    Ok(Ok(())) => self.pending = true,
                    Ok(Err(error)) => self.job_failed(error.into()),
                    Err(_) => self.job_failed(
                        "This workspace is no longer available. Reopen the form and try again."
                            .into(),
                    ),
                }
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    pub(super) fn browse(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get(index) else {
            return;
        };
        let Some(picker) = field.spec.picker.clone() else {
            return;
        };
        let Control::Text(state) = &field.control else {
            return;
        };
        let state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let dialog = rfd::AsyncFileDialog::new();
            let file = match &picker {
                Picker::Folder | Picker::Export(_) => dialog.pick_folder().await,
                Picker::Iso => dialog.add_filter("Installation image", &["iso"]).pick_file().await,
                Picker::Disk => {
                    dialog
                        .add_filter(
                            "Disk image",
                            &["qcow2", "raw", "img", "vmdk", "vdi", "vhd", "vhdx"],
                        )
                        .pick_file()
                        .await
                }
                Picker::Appliance => {
                    dialog.add_filter("Virtual appliance", &["ova", "ovf"]).pick_file().await
                }
                Picker::ConvertedDisk => dialog.save_file().await,
                Picker::OvaExport => {
                    dialog
                        .add_filter("Virtual appliance", &["ova"])
                        .set_file_name("machine.ova")
                        .save_file()
                        .await
                }
                Picker::MemoryDump => dialog.set_file_name("memory.dump").save_file().await,
                Picker::Screenshot => {
                    dialog
                        .add_filter("PNG image", &["png"])
                        .set_file_name("screenshot.png")
                        .save_file()
                        .await
                }
                Picker::Xml => {
                    dialog.add_filter("Machine configuration", &["xml"]).pick_file().await
                }
            };
            if let Some(file) = file {
                let path = match picker {
                    Picker::Export(name) => file.path().join(name),
                    _ => file.path().to_owned(),
                };
                let _ = this.update_in(cx, |this, window, cx| {
                    state.update(cx, |state, cx| {
                        state.set_value(path.to_string_lossy().into_owned(), window, cx)
                    });
                    this.changed(window, cx);
                });
            }
        })
        .detach();
    }
}
