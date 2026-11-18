use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotAction {
    Restore,
    Rename,
    Overwrite,
    Delete,
}
impl SnapshotAction {
    pub const ALL: [Self; 4] = [Self::Restore, Self::Rename, Self::Overwrite, Self::Delete];
    pub fn label(self) -> &'static str {
        match self {
            Self::Restore => "Restore",
            Self::Rename => "Rename",
            Self::Overwrite => "Overwrite",
            Self::Delete => "Delete",
        }
    }
}

pub fn snapshot(m: &Machine) -> FormSpec {
    let machine = m.clone();
    FormSpec::new("New snapshot", "Save snapshot", vec![F::text("Name", "")], move |v| {
        Ok(operation(machine.clone(), Op::SnapshotCreate(name(&v[0])?)))
    })
}

pub fn snapshot_action(action: SnapshotAction, m: &Machine, s: &Snapshot) -> FormSpec {
    let machine = m.clone();
    let snapshot = s.name.clone();
    match action {
        SnapshotAction::Restore => confirm(
            "Restore snapshot",
            "Restore",
            "Changes since this snapshot will be lost. Power off before restoring.",
            machine,
            Op::SnapshotRestore(snapshot),
        ),
        SnapshotAction::Delete => confirm(
            "Delete snapshot",
            "Delete",
            "This saved state will be permanently removed.",
            machine,
            Op::SnapshotDelete(snapshot),
        ),
        SnapshotAction::Overwrite => confirm(
            "Overwrite snapshot",
            "Overwrite",
            "The saved snapshot will be replaced by the current state.",
            machine,
            Op::SnapshotOverwrite(snapshot),
        ),
        SnapshotAction::Rename => FormSpec::new(
            "Rename snapshot",
            "Rename",
            vec![F::text("Name", s.label.clone())],
            move |v| {
                Ok(operation(
                    machine.clone(),
                    Op::SnapshotRename {
                        old: snapshot.clone(),
                        new: name(&v[0])?,
                    },
                ))
            },
        ),
    }
}
