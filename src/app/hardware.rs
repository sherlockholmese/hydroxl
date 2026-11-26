use super::{Job, Workspace};
use crate::operations::Operation;
use crate::workflows::HardwareDraft;
use gpui_kit::*;

impl Workspace {
    /// Saved hardware and the values being edited (the draft, or the saved values).
    pub(crate) fn hardware_values(&self) -> Option<(HardwareDraft, HardwareDraft)> {
        let machine = self.selected_machine()?;
        let original = HardwareDraft::read(machine, &self.details.xml)?;
        let current = self
            .hardware_draft
            .clone()
            .filter(|d| d.id == original.id)
            .unwrap_or_else(|| original.clone());
        Some((original, current))
    }

    /// Typed memory in MB, applied to the draft as the field changes.
    pub(crate) fn memory_typed(&mut self, cx: &mut Context<Self>) {
        let value = self.memory_input.read(cx).value();
        let typed =
            value.trim().parse::<u64>().ok().filter(|mib| crate::backend::MEMORY_MIB.contains(mib));
        self.memory_invalid = typed.is_none();
        if let Some(mib) = typed
            && self.hardware_values().is_some_and(|(_, draft)| draft.memory_mib != mib)
        {
            self.edit_hardware(|draft| draft.memory_mib = mib, cx);
        }
        cx.notify();
    }

    pub(crate) fn memory_error(&self) -> Option<&'static str> {
        self.memory_invalid.then_some("Enter a whole number from 64 to 1,048,576 MiB.")
    }

    /// Keeps the memory field showing the draft when it changes elsewhere.
    pub(crate) fn sync_memory_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, d)) = self.hardware_values() else {
            return;
        };
        let input = self.memory_input.clone();
        let shown = input.read(cx).value().trim().parse::<u64>().ok();
        if !self.memory_invalid && shown != Some(d.memory_mib) {
            input.update(cx, |s, cx| s.set_value(d.memory_mib.to_string(), window, cx));
        }
    }

    pub(crate) fn hardware_dirty(&self) -> bool {
        self.memory_invalid
            || self.hardware_values().is_some_and(|(original, current)| original != current)
    }

    pub(crate) fn edit_hardware(
        &mut self,
        edit: impl FnOnce(&mut HardwareDraft),
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        if let Some((_, mut current)) = self.hardware_values() {
            let memory = current.memory_mib;
            edit(&mut current);
            if current.memory_mib != memory {
                self.memory_invalid = false;
            }
            self.hardware_draft = Some(current);
            self.saved_hardware_for = None;
            cx.notify();
        }
    }

    pub(crate) fn discard_hardware(&mut self, cx: &mut Context<Self>) {
        self.hardware_draft = None;
        self.saved_hardware_for = None;
        self.memory_invalid = false;
        cx.notify();
    }

    pub(crate) fn save_hardware(&mut self, cx: &mut Context<Self>) {
        if self.details_error.is_some() || self.memory_error().is_some() {
            return;
        }
        let Some((original, current)) = self.hardware_values() else {
            return;
        };
        let Some(machine) = self.selected_machine().cloned() else {
            return;
        };
        let ops = current.changes(&original);
        if ops.is_empty() {
            return;
        }
        self.begin(Job::Operation(machine, Operation::Sequence(ops)), None, cx);
    }
}
