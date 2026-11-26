use super::DisplayView;
use crate::app::Workspace;
use crate::backend::{Machine, State};
use gpui_kit::*;

impl Workspace {
    /// Opens the selected guest's display in its own window, or brings an open one forward.
    pub(crate) fn open_display(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(machine) = self.selected_machine().cloned() else {
            return;
        };
        self.open_machine_display(machine, cx);
    }

    pub(crate) fn open_machine_display(&mut self, machine: Machine, cx: &mut Context<Self>) {
        if self.demo && std::env::var_os("HYDROXL_VNC_SOCKET").is_none() {
            self.notice = Some("The guest display is unavailable in preview.".into());
            cx.notify();
            return;
        }
        let connection = self.preferences.connection;
        self.display_windows.retain(|(_, _, _, view)| view.upgrade().is_some());
        if let Some((_, _, handle, view)) =
            self.display_windows.iter().find(|(c, id, _, _)| *c == connection && *id == machine.id)
        {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                if matches!(machine.state, State::Running | State::Paused) {
                    let _ = view.update(cx, |view, cx| view.ensure_session(window, cx));
                }
            });
            return;
        }
        let key = (connection, machine.id.clone());
        if !self.opening_displays.insert(key.clone()) {
            return;
        }
        let id = machine.id.clone();
        let workspace = cx.entity().downgrade();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(844.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(machine.name.clone().into()),
                ..Default::default()
            }),
            app_id: Some("dev.hydroxl.Hydroxl".into()),
            ..Default::default()
        };
        let busy = self.busy();
        // Opened after this update: the new window renders immediately, outside the workspace.
        cx.defer(move |cx| {
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let mut view = DisplayView::connect(
                        connection,
                        id,
                        &machine,
                        workspace.clone(),
                        window,
                        cx,
                    );
                    view.busy = busy;
                    view
                })
            });
            let _ = workspace.update(cx, |this, cx| {
                this.opening_displays.remove(&key);
                match opened {
                    Ok((handle, view)) => {
                        this.display_windows.push((
                            connection,
                            machine.id,
                            handle,
                            view.downgrade(),
                        ));
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not open the display window: {error}"))
                    }
                }
                cx.notify();
            });
        });
    }
}
