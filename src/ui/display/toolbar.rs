use super::DisplayView;
use crate::app::{ActionSource, Job};
use crate::backend::{Action, Machine, State};
use crate::display::geometry::Scaling;
use crate::operations::Operation;
use crate::theme::{BODY_SIZE, CAPTION_SIZE, Palette};
use crate::ui::primitives::{MENU_MIN_WIDTH, glyph};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};
use std::{sync::mpsc, time::Duration};

pub(super) const TOOLBAR_HEIGHT: f32 = 44.;
const COMPACT_TOOLBAR_WIDTH: f32 = 480.;

const CTRL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
const SUPER: u32 = 0xffeb;
/// Special key combinations the host would otherwise intercept, as keycaps. Groups are
/// separated in the menu: session, consoles, then window keys.
const SPECIAL_KEYS: &[(&[&str], &[u32], u8)] = &[
    (&["Ctrl", "Alt", "Del"], &[CTRL, ALT, 0xffff], 0),
    (&["Ctrl", "Alt", "Backspace"], &[CTRL, ALT, 0xff08], 0),
    (&["Ctrl", "Alt", "F1"], &[CTRL, ALT, 0xffbe], 1),
    (&["Ctrl", "Alt", "F2"], &[CTRL, ALT, 0xffbf], 1),
    (&["Ctrl", "Alt", "F3"], &[CTRL, ALT, 0xffc0], 1),
    (&["Ctrl", "Alt", "F4"], &[CTRL, ALT, 0xffc1], 1),
    (&["Ctrl", "Alt", "F5"], &[CTRL, ALT, 0xffc2], 1),
    (&["Ctrl", "Alt", "F6"], &[CTRL, ALT, 0xffc3], 1),
    (&["Ctrl", "Alt", "F7"], &[CTRL, ALT, 0xffc4], 1),
    (&["Alt", "Tab"], &[ALT, 0xff09], 2),
    (&["Alt", "F4"], &[ALT, 0xffc1], 2),
    (&["Ctrl", "Esc"], &[CTRL, 0xff1b], 2),
    (&["Super"], &[SUPER], 2),
    (&["Print Screen"], &[0xff61], 2),
];

/// A key drawn as a keycap: the same shape in menus and in the capture hint.
fn keycap(label: &'static str, cx: &App) -> Div {
    let p = Palette::get(cx);
    div()
        .px_1()
        .min_w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_b_2()
        .border_color(p.border)
        .bg(p.well)
        .text_size(px(CAPTION_SIZE))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .child(label)
}

fn scaling_icon(mode: Scaling) -> &'static str {
    match mode {
        Scaling::Fit => "shrink",
        Scaling::GuestResize => "scan",
        Scaling::Stretch => "move-diagonal-2",
        Scaling::Fill => "expand",
        Scaling::ActualSize => "ratio",
    }
}

impl DisplayView {
    fn run(&self, job: impl FnOnce(Machine) -> Job, cx: &mut Context<Self>) {
        let Some(machine) = self.machine(cx) else {
            return;
        };
        let job = job(machine);
        let _ = self.workspace.update(cx, |w, cx| w.begin(job, None, cx));
    }

    fn power(&self, action: Action, window: &Window, cx: &mut Context<Self>) {
        let source = ActionSource::Viewer(window.window_handle());
        self.run(|machine| Job::Action(machine, action, source), cx);
    }

    fn receive_session_result<T: Send + 'static>(
        result: anyhow::Result<mpsc::Receiver<anyhow::Result<T>>>,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = match result {
            Ok(rx) => rx,
            Err(error) => {
                window.push_notification(
                    crate::ui::primitives::plain_error(&format!("{error:#}")),
                    cx,
                );
                return;
            }
        };
        cx.spawn_in(window, async move |view, cx| {
            let result = blocking::unblock(move || {
                rx.recv_timeout(Duration::from_secs(8))
                    .map_err(|_| anyhow::anyhow!("The guest did not respond."))
                    .and_then(|result| result)
            })
            .await;
            let _ = view.update_in(cx, |view, window, cx| match result {
                Ok(value) => done(view, value, window, cx),
                Err(error) => window.push_notification(
                    crate::ui::primitives::plain_error(&format!("{error:#}")),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Asks before an action that discards guest state, in this window.
    fn confirm(
        &self,
        title: &'static str,
        action: &'static str,
        op: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let op = op.clone();
            crate::forms::frame(dialog, title, 400.)
                .child("Unsaved work inside the guest is lost.")
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-power")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("confirm-power").danger().label(action).on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let op = op.clone();
                                let _ = this
                                    .update(cx, |this, cx| this.run(|m| Job::Operation(m, op), cx));
                            },
                        )),
                )
        });
    }

    /// Input state and capture mode in one control: the label says where input goes, the
    /// menu says how it gets there.
    fn capture_control(
        &self,
        p: Palette,
        compact: bool,
        track_menu: impl Fn(&bool, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let on_click = self.capture_on_click;
        let this = cx.entity().downgrade();
        Button::new("capture-mode")
            .ghost()
            .small()
            .icon(
                glyph(if self.captured {
                    "mouse-pointer-click"
                } else {
                    "mouse-pointer-2"
                })
                .text_color(if self.captured {
                    p.text
                } else {
                    p.muted
                }),
            )
            .when(!compact, |button| {
                button.label(if self.captured {
                    "Captured"
                } else {
                    "Released"
                })
            })
            .dropdown_caret(true)
            .disabled(self.session.is_none())
            .accessibility_label("Input capture")
            .tooltip(if self.captured {
                "Ctrl Alt releases"
            } else {
                "Input capture"
            })
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let item = |label: &'static str, click: bool| {
                    let this = this.clone();
                    PopupMenuItem::new(label).checked(on_click == click).on_click(
                        move |_, _, cx| {
                            let _ = this.update(cx, |view, cx| view.set_capture_mode(click, cx));
                        },
                    )
                };
                menu.min_w(px(MENU_MIN_WIDTH))
                    .check_side(Side::Right)
                    .item(item("Capture when pointer enters", false))
                    .item(item("Capture on click", true))
            })
            .on_open_change(track_menu)
    }

    pub(super) fn top_bar(
        &self,
        p: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let compact = window.viewport_size().width < px(COMPACT_TOOLBAR_WIDTH);
        let state = self.state;
        let running = state == Some(State::Running);
        let paused = state == Some(State::Paused);
        let on = running || paused;
        let busy = self.busy;
        let icon = |id: &'static str, glyph: Glyph, label: &'static str| {
            Button::new(id)
                .ghost()
                .small()
                .icon(Icon::new(glyph))
                .accessibility_label(label)
                .tooltip(label)
        };
        let weak = cx.entity().downgrade();
        let track_menu = move |open: &bool, window: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |view, cx| {
                view.toolbar_menu_open = *open;
                if !open && window.mouse_position().y > px(TOOLBAR_HEIGHT) {
                    view.toolbar_revealed = false;
                }
                cx.notify();
            });
        };
        let track_menu_capture = track_menu.clone();
        let session = self.session.clone();
        let usb = session.as_ref().is_some_and(|session| session.can_toggle_usb());
        let view = cx.entity().downgrade();
        let keys = Button::new("special-keys")
            .ghost()
            .small()
            .icon(glyph("keyboard"))
            .label("Keys")
            .dropdown_caret(true)
            .disabled(session.is_none())
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                menu = menu.min_w(px(MENU_MIN_WIDTH));
                let mut group = None;
                for &(caps, keys, g) in SPECIAL_KEYS {
                    if group.is_some_and(|last| last != g) {
                        menu = menu.separator();
                    }
                    group = Some(g);
                    let session = session.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            h_flex().gap_1().children(caps.iter().map(|cap| keycap(cap, cx)))
                        })
                        .on_click(move |_, _, _| {
                            if let Some(s) = &session {
                                s.combo(keys);
                            }
                        }),
                    );
                }
                let copy = {
                    let view = view.clone();
                    PopupMenuItem::new("Copy guest clipboard")
                        .icon(glyph("clipboard-copy"))
                        .on_click(move |_, window, cx| {
                            let _ = view.update(cx, |view, cx| {
                                view.copy_guest_clipboard_now(window, cx);
                            });
                        })
                };
                let view = view.clone();
                menu.separator().item(copy).item(
                    PopupMenuItem::new("Type clipboard text")
                        .icon(glyph("clipboard-paste"))
                        .on_click(move |_, window, cx| {
                            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                            else {
                                window.push_notification("The clipboard has no text.", cx);
                                return;
                            };
                            let _ = view.update(cx, |view, cx| {
                                view.run(
                                    |m| {
                                        Job::Operation(
                                            m,
                                            Operation::Guest(crate::operations::GuestSetup::Type(
                                                text,
                                            )),
                                        )
                                    },
                                    cx,
                                )
                            });
                        }),
                )
            })
            .on_open_change(track_menu.clone());
        let scaling = {
            let current = self.scaling;
            let performance_enabled = self.performance.is_some();
            let this = cx.entity().downgrade();
            Button::new("scaling")
                .ghost()
                .small()
                .icon(glyph(scaling_icon(current)))
                .label(current.label())
                .accessibility_label(format!("Display options: {}", current.menu_label()))
                .tooltip("Display scaling")
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.min_w(px(MENU_MIN_WIDTH)).check_side(Side::Right);
                    for mode in Scaling::ALL {
                        let this = this.clone();
                        menu = menu.item(
                            PopupMenuItem::new(mode.menu_label())
                                .icon(glyph(scaling_icon(mode)))
                                .checked(mode == current)
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |view, cx| {
                                        view.scaling = mode;
                                        view.guest_resize = None;
                                        let _ = view.workspace.update(cx, |workspace, _| {
                                            workspace
                                                .preferences
                                                .vm_scaling
                                                .insert(view.machine.clone(), mode);
                                            workspace.save_preferences();
                                        });
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    let this = this.clone();
                    menu.separator().item(
                        PopupMenuItem::new("Performance overlay")
                            .checked(performance_enabled)
                            .on_click(move |_, window, cx| {
                                let _ =
                                    this.update(cx, |view, cx| view.toggle_performance(window, cx));
                            }),
                    )
                })
                .on_open_change(track_menu.clone())
        };
        let danger = p.danger;
        let power = {
            let this = cx.entity().downgrade();
            icon("power-menu", Glyph::Ellipsis, "More power options")
                .disabled(!on || busy)
                .dropdown_menu(move |menu, _, _| {
                    let item = |label: &'static str, f: fn(&mut DisplayView, &mut Window, &mut Context<DisplayView>)| {
                        let this = this.clone();
                        PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ = this.update(cx, |this, cx| f(this, window, cx));
                        })
                    };
                    menu.min_w(px(MENU_MIN_WIDTH))
                        .item(item("Reset…", |this, window, cx| {
                            this.confirm("Reset", "Reset", Operation::Reset, window, cx)
                        })
                        .icon(glyph("refresh-ccw-dot").text_color(danger)))
                        .item(item("Force stop…", |this, window, cx| {
                            this.confirm("Force stop", "Force stop", Operation::ForceStop, window, cx)
                        })
                        .icon(glyph("power-off").text_color(danger)))
                })
                .on_open_change(track_menu)
        };
        h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .bg(p.canvas)
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(p.border)
            // The machine's name leads, set off from its controls like the other groups.
            .when_some(
                self.machine(cx).filter(|_| !compact).map(|machine| machine.name),
                |toolbar, name| {
                    toolbar
                        .child(
                            div()
                                .px_2()
                                .max_w(px(240.))
                                .min_w_0()
                                .flex_shrink(1.)
                                .truncate()
                                .text_size(px(BODY_SIZE))
                                .font_weight(FontWeight::MEDIUM)
                                .child(name),
                        )
                        .child(div().w(px(1.)).h(px(20.)).mx_1().flex_shrink_0().bg(p.border))
                },
            )
            .when(!on, |t| {
                t.child(
                    icon("start", Glyph::Play, "Start")
                        .disabled(busy || state != Some(State::Stopped))
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.power(Action::Start, window, cx)
                            }),
                        ),
                )
            })
            .when(paused, |t| {
                t.child(icon("resume", Glyph::Play, "Resume").disabled(busy).on_click(
                    cx.listener(|this, _, window, cx| this.power(Action::Resume, window, cx)),
                ))
            })
            .when(running, |t| {
                t.child(icon("pause", Glyph::Pause, "Pause").disabled(busy).on_click(
                    cx.listener(|this, _, window, cx| this.power(Action::Pause, window, cx)),
                ))
            })
            .when(on, |toolbar| {
                toolbar
                    .child(icon("shutdown", Glyph::Power, "Shut down").disabled(busy).on_click(
                        cx.listener(|view, _, window, cx| view.power(Action::Shutdown, window, cx)),
                    ))
                    .child(
                        Button::new("restart")
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::RotateCcw))
                            .accessibility_label("Restart")
                            .tooltip("Restart")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.run(|m| Job::Operation(m, Operation::Reboot), cx)
                            })),
                    )
            })
            .child(power)
            .child(div().w(px(1.)).h(px(20.)).mx_1().bg(p.border))
            .child(keys)
            .child(scaling)
            .when(usb, |toolbar| {
                let session = self.session.clone();
                let attached = self.usb_attached.clone();
                let view = cx.entity().downgrade();
                toolbar.child(
                    icon("viewer-usb", Glyph::Usb, "Redirect USB devices")
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                            let devices = crate::operations::host_usb();
                            if devices.is_empty() {
                                return menu.label("No USB devices");
                            }
                            // A flat list that never scrolls; a check marks what the guest
                            // has, so choosing an entry reads as connect or disconnect.
                            menu = menu.min_w(px(MENU_MIN_WIDTH)).check_side(Side::Right);
                            for (address, label) in devices {
                                let Some((bus, address)) =
                                    address.split_once(':').and_then(|(bus, addr)| {
                                        Some((bus.parse::<u8>().ok()?, addr.parse::<u8>().ok()?))
                                    })
                                else {
                                    continue;
                                };
                                let session = session.clone();
                                let view = view.clone();
                                let name = label.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(attached.contains(&(bus, address)))
                                        .on_click(move |_, window, cx| {
                                            let Some(session) = &session else {
                                                return;
                                            };
                                            let name = name.clone();
                                            let _ = view.update(cx, |_, cx| {
                                                Self::receive_session_result(
                                                    session.toggle_usb(bus, address),
                                                    move |view, attached, window, cx| {
                                                        if attached {
                                                            view.usb_attached
                                                                .insert((bus, address));
                                                        } else {
                                                            view.usb_attached
                                                                .remove(&(bus, address));
                                                        }
                                                        window.push_notification(
                                                            if attached {
                                                                format!("{name} connected")
                                                            } else {
                                                                format!("{name} disconnected")
                                                            },
                                                            cx,
                                                        );
                                                        cx.notify();
                                                    },
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
            })
            .child(div().flex_1())
            .when(self.captured, |toolbar| {
                // While captured, the way out is the only thing worth saying.
                toolbar.child(
                    h_flex()
                        .gap_1()
                        .mr_1()
                        .text_size(px(CAPTION_SIZE))
                        .text_color(p.muted)
                        .child(keycap("Ctrl", cx))
                        .child(keycap("Alt", cx)),
                )
            })
            .child(self.capture_control(p, compact, track_menu_capture, cx))
            .when(window.is_fullscreen(), |toolbar| {
                toolbar.child(
                    icon(
                        "pin-toolbar",
                        Glyph::Pin,
                        if self.toolbar_pinned {
                            "Unpin toolbar"
                        } else {
                            "Pin toolbar"
                        },
                    )
                    .selected(self.toolbar_pinned)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.toolbar_pinned = !view.toolbar_pinned;
                        let pinned = view.toolbar_pinned;
                        let _ = view.workspace.update(cx, |workspace, _| {
                            workspace.preferences.toolbar_pinned = pinned;
                            workspace.save_preferences();
                        });
                        cx.notify();
                    })),
                )
            })
            .child(
                icon(
                    "fullscreen",
                    if window.is_fullscreen() {
                        Glyph::Minimize
                    } else {
                        Glyph::Maximize
                    },
                    if window.is_fullscreen() {
                        "Exit full screen"
                    } else {
                        "Full screen"
                    },
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    view.toolbar_revealed = false;
                    window.toggle_fullscreen();
                    cx.notify();
                })),
            )
    }
}
