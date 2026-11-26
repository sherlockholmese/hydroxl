use crate::app::Job;
use anyhow::Result;
use gpui_kit::SharedString;
use gpui_kit::component::select::SelectItem;
use std::rc::Rc;
#[derive(Clone)]
pub struct Choice {
    pub value: String,
    pub label: String,
}
impl SelectItem for Choice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &String {
        &self.value
    }
}

#[derive(Clone)]
pub enum Picker {
    Iso,
    Disk,
    ConvertedDisk,
    Xml,
    Folder,
    Appliance,
    Screenshot,
    MemoryDump,
    OvaExport,
    Export(String),
}
#[derive(Clone)]
pub struct FieldSpec {
    pub label: &'static str,
    pub value: String,
    pub choices: Vec<Choice>,
    pub picker: Option<Picker>,
    pub multiline: bool,
    pub checkbox: bool,
    pub suffix: Option<&'static str>,
    /// Wizard step that owns this field.
    pub step: usize,
    /// Hidden behind the step's Advanced disclosure until opened.
    pub advanced: bool,
    /// Fields sharing a non-zero row id render side by side.
    pub row: u8,
}
impl FieldSpec {
    pub fn text(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            choices: Vec::new(),
            picker: None,
            multiline: false,
            checkbox: false,
            suffix: None,
            step: 0,
            advanced: false,
            row: 0,
        }
    }
    pub fn checkbox(label: &'static str, checked: bool) -> Self {
        Self {
            checkbox: true,
            ..Self::text(label, checked.to_string())
        }
    }
    pub fn suffix(mut self, suffix: &'static str) -> Self {
        self.suffix = Some(suffix);
        self
    }
    pub fn step(mut self, step: usize) -> Self {
        self.step = step;
        self
    }
    pub fn row(mut self, row: u8) -> Self {
        self.row = row;
        self
    }
    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
    /// Visible text for the current value: the choice label, never the stored key.
    pub fn display(&self, value: &str) -> String {
        if self.checkbox {
            return if value == "true" {
                "Yes"
            } else {
                "No"
            }
            .into();
        }
        if self.choices.is_empty() {
            return value.trim().to_owned();
        }
        self.choices.iter().find(|c| c.value == value).map_or_else(String::new, |c| c.label.clone())
    }
    pub fn choice(label: &'static str, choices: &[(&str, &str)], selected: &str) -> Self {
        let mut choices: Vec<_> = choices
            .iter()
            .map(|(value, label)| Choice {
                value: (*value).into(),
                label: (*label).into(),
            })
            .collect();
        if !selected.is_empty() && !choices.iter().any(|choice| choice.value == selected) {
            choices.push(Choice {
                value: selected.into(),
                label: "Current configuration".into(),
            });
        }
        Self {
            label,
            value: selected.into(),
            choices,
            ..Self::text(label, "")
        }
    }
    pub fn path(label: &'static str, picker: Picker) -> Self {
        Self {
            picker: Some(picker),
            ..Self::text(label, "")
        }
    }
    pub fn xml(value: String) -> Self {
        Self {
            multiline: true,
            ..Self::text("Domain configuration", value)
        }
    }
}

pub(super) type Submit = Rc<dyn Fn(&[String]) -> Result<Job>>;
/// Proposes values for other fields from the current ones. Only fields the user has not
/// changed since the last proposal are updated.
pub(super) type Derive = Rc<dyn Fn(&[String]) -> Vec<(usize, String)>>;
/// Validates one wizard step before moving forward.
pub(super) type Check = Rc<dyn Fn(usize, &[String]) -> Result<()>>;
/// Whether a field is shown for the current values.
pub(super) type Visible = Rc<dyn Fn(usize, &[String]) -> bool>;
/// Choices for a select that depend on other values, such as OS versions.
/// Returns `None` when the field's choices are static.
pub(super) type Options = Rc<dyn Fn(usize, &[String]) -> Option<Vec<(String, String)>>>;
pub struct FormSpec {
    pub title: String,
    pub action: &'static str,
    pub note: Option<String>,
    pub fields: Vec<FieldSpec>,
    pub submit: Submit,
    pub destructive: bool,
    pub unavailable: bool,
    /// Wizard step titles. Empty for single-page forms. A review step is appended.
    pub steps: Vec<&'static str>,
    pub derive: Option<Derive>,
    pub check: Option<Check>,
    pub options: Option<Options>,
    pub visible: Option<Visible>,
}
impl FormSpec {
    pub fn new(
        title: impl Into<String>,
        action: &'static str,
        fields: Vec<FieldSpec>,
        submit: impl Fn(&[String]) -> Result<Job> + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            action,
            note: None,
            fields,
            submit: Rc::new(submit),
            destructive: false,
            unavailable: false,
            steps: Vec::new(),
            derive: None,
            check: None,
            options: None,
            visible: None,
        }
    }
    pub fn steps(mut self, steps: &[&'static str]) -> Self {
        self.steps = steps.to_vec();
        self
    }
    pub fn derive(mut self, f: impl Fn(&[String]) -> Vec<(usize, String)> + 'static) -> Self {
        self.derive = Some(Rc::new(f));
        self
    }
    pub fn options(
        mut self,
        f: impl Fn(usize, &[String]) -> Option<Vec<(String, String)>> + 'static,
    ) -> Self {
        self.options = Some(Rc::new(f));
        self
    }
    pub fn visible(mut self, f: impl Fn(usize, &[String]) -> bool + 'static) -> Self {
        self.visible = Some(Rc::new(f));
        self
    }
    pub fn check(mut self, f: impl Fn(usize, &[String]) -> Result<()> + 'static) -> Self {
        self.check = Some(Rc::new(f));
        self
    }
    pub fn unavailable(mut self) -> Self {
        self.unavailable = true;
        self
    }
    pub fn danger(mut self) -> Self {
        self.destructive = true;
        self
    }
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}
