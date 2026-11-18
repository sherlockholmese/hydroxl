mod dialog;
mod editor;
mod render;
mod schema;
use anyhow::{Context as _, Result};
pub use dialog::{frame, open};
pub use editor::EditorForm;
pub use schema::{FieldSpec, FormSpec, Picker};
pub fn number<T: std::str::FromStr>(values: &[String], index: usize, label: &str) -> Result<T> {
    values
        .get(index)
        .and_then(|s| s.trim().parse().ok())
        .with_context(|| format!("Enter a valid {label}."))
}
