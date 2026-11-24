mod dialog;
mod editor;
mod render;
mod schema;
use anyhow::Result;
pub use dialog::{frame, open};
pub use editor::EditorForm;
pub use schema::{FieldSpec, FormSpec, Picker};
pub fn number<T: std::str::FromStr>(values: &[String], index: usize, label: &str) -> Result<T> {
    values
        .get(index)
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| field_error(index, format!("Enter a valid {label}.")))
}

/// A validation failure that belongs to one field, so the form shows it under that field
/// (opening Advanced first when the field lives there) instead of above the buttons.
#[derive(Debug)]
pub struct FieldError {
    pub field: usize,
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FieldError {}

pub fn field_error(field: usize, message: impl Into<String>) -> anyhow::Error {
    FieldError {
        field,
        message: message.into(),
    }
    .into()
}
