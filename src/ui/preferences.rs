use crate::PreferencesAction;
use crate::app::Workspace;
use crate::backend::Connection;
use crate::theme::{self, Palette};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};

impl Workspace {
    pub(crate) fn preferences(
        &mut self,
        _: &PreferencesAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let Some(parent) = weak.upgrade() else {
                return dialog;
            };
            let state = parent.read(cx);
            let p = Palette::get(cx);
            let dark = state.preferences.dark;
            let connection = (!state.demo).then_some(state.preferences.connection);
            let busy = state.busy();
            let row = |label: &'static str, control: AnyElement| {
                super::primitives::setting(label, control, p)
            };
            let appearance = super::primitives::segmented(
                "appearance",
                &["Light", "Dark"],
                Some(dark as usize),
                true,
                {
                    let weak = weak.clone();
                    move |i, _, cx| {
                        let value = i == 1;
                        let _ = weak.update(cx, |this, cx| {
                            this.preferences.dark = value;
                            this.save_preferences();
                            theme::apply(value, cx);
                            cx.notify();
                        });
                    }
                },
                cx,
            );
            let choices = [Connection::Session, Connection::System];
            let connections = super::primitives::segmented(
                "connection",
                &[Connection::Session.label(), Connection::System.label()],
                connection.and_then(|c| choices.iter().position(|v| *v == c)),
                !busy,
                {
                    let weak = weak.clone();
                    move |i, _, cx| {
                        let Some(value) = choices.get(i).copied() else {
                            return;
                        };
                        let _ = weak.update(cx, |this, cx| {
                            this.switch_connection(value, cx);
                        });
                    }
                },
                cx,
            );
            let location = state.preferences.machines_dir();
            let custom_location = state.preferences.vm_location.is_some();
            let choose_folder = {
                let weak = weak.clone();
                let location = location.clone();
                Button::new("choose-vm-location").outline().small().label("Choose folder").on_click(
                    move |_, _, cx| {
                        let weak = weak.clone();
                        let location = location.clone();
                        cx.spawn(async move |cx| {
                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                .set_directory(location)
                                .pick_folder()
                                .await
                            {
                                let path = folder.path().to_owned();
                                let _ = weak.update(cx, |this, cx| {
                                    this.preferences.vm_location = Some(path);
                                    this.save_preferences();
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                    },
                )
            };
            let folder = v_flex()
                .gap_2()
                .min_w_0()
                .flex_1()
                // The whole path, wrapped: its end is the part that identifies the folder.
                .child(div().pt_2().child(home_relative(&location)))
                .child(h_flex().gap_2().child(choose_folder).when(custom_location, |row| {
                    let weak = weak.clone();
                    row.child(
                        Button::new("reset-vm-location")
                            .ghost()
                            .small()
                            .label("Use default")
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.preferences.vm_location = None;
                                    this.save_preferences();
                                    cx.notify();
                                });
                            }),
                    )
                }));
            crate::forms::frame(dialog, "Preferences", 480.)
                .child(
                    v_flex()
                        .gap_2()
                        .text_color(p.text)
                        .child(row("Appearance", appearance))
                        .child(row("Connection", connections))
                        .child(row("VM folder", folder.into_any_element())),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child({
                            let weak = weak.clone();
                            Button::new("open-about").ghost().label("About Hydroxl").on_click(
                                move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak.update(cx, |this, cx| {
                                        this.about(&crate::AboutAction, window, cx)
                                    });
                                },
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new("close-preferences")
                                .outline()
                                .label("Done")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
        });
    }
}

/// A path with the home folder shown as `~`.
fn home_relative(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}
