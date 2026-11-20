use crate::app::{ActionSource, Job, Workspace};
use crate::backend::Action;
use crate::forms;
use crate::forms::FormSpec;
use crate::workflows;
use crate::{NewMachineAction, RefreshAction};
use gpui_kit::{prelude::*, *};

impl Workspace {
    pub(crate) fn open_form(
        &mut self,
        spec: FormSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        forms::open(spec, cx.entity().downgrade(), window, cx);
    }

    pub(crate) fn create(
        &mut self,
        _: &NewMachineAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_form(workflows::create(self.preferences.machines_dir()), window, cx);
    }

    pub(crate) fn refresh(&mut self, _: &RefreshAction, _: &mut Window, cx: &mut Context<Self>) {
        self.begin(Job::Refresh, None, cx);
    }

    pub(crate) fn machine_action(&mut self, action: Action, cx: &mut Context<Self>) {
        if let Some(m) = self.selected_machine().cloned() {
            self.begin(Job::Action(m, action, ActionSource::Library), None, cx);
        }
    }
}
