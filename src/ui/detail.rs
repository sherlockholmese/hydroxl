use super::primitives::{logo_tile, status_color};
use crate::NewMachineAction;
use crate::app::Workspace;
use crate::backend::{Action, State};
use crate::theme::{BODY_SIZE, CONTENT_WIDTH, Palette, TITLE_SIZE, WIDE_CONTENT_WIDTH, motion};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

pub(crate) const TABS: [&str; 3] = ["Overview", "Hardware", "Snapshots"];

impl Workspace {
    pub(crate) fn detail(&self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::get(cx);
        let Some(machine) = self.selected_machine() else {
            return self.placeholder(p, cx);
        };
        let gutter = if narrow {
            px(16.)
        } else {
            px(32.)
        };
        let busy = self.busy();
        let content_width = if self.tab == 1 && self.wide {
            WIDE_CONTENT_WIDTH
        } else {
            CONTENT_WIDTH
        };

        let mut controls = h_flex().gap_2().flex_shrink_0().child(
            self.machine_menu(
                machine,
                Button::new("machine-actions")
                    .outline()
                    .icon(Icon::new(Glyph::Ellipsis))
                    .accessibility_label("Machine actions")
                    .tooltip("Machine actions"),
                cx,
            ),
        );
        if machine.state == State::Running {
            controls = controls.child(
                Button::new("pause")
                    .outline()
                    .icon(Icon::new(Glyph::Pause))
                    .accessibility_label("Pause")
                    .tooltip("Pause")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.machine_action(Action::Pause, cx))),
            );
        }
        if matches!(machine.state, State::Running | State::Paused) {
            controls = controls.child(
                Button::new("shutdown")
                    .outline()
                    .icon(Icon::new(Glyph::Power))
                    .accessibility_label("Shut down")
                    .tooltip("Shut down")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.machine_action(Action::Shutdown, cx)),
                    ),
            );
        }
        let (label, action, glyph) = match machine.state {
            State::Stopped if machine.saved_state => ("Resume", Action::Start, Glyph::Play),
            State::Stopped | State::Other => ("Start", Action::Start, Glyph::Play),
            State::Paused => ("Resume", Action::Resume, Glyph::Play),
            State::Running => ("Open display", Action::Display, Glyph::MonitorPlay),
        };
        controls = controls.child(
            Button::new("primary-machine-action")
                .primary()
                .label(label)
                .icon(Icon::new(glyph))
                .disabled(busy || machine.state == State::Other)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if matches!(action, Action::Display) {
                        this.open_display(window, cx);
                    } else {
                        this.machine_action(action, cx);
                    }
                })),
        );

        let identity =
            h_flex().gap_3().min_w_0().flex_1().child(logo_tile(machine, 28., p, cx)).child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(TITLE_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(machine.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(BODY_SIZE - 1.))
                            .text_color(status_color(machine, p))
                            .child(machine.status()),
                    ),
            );

        let content = match self.tab {
            1 => self.hardware(machine, cx),
            2 => self.snapshots(machine, cx),
            _ => self.overview(machine, narrow, cx),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_5()
                    .px(gutter)
                    .pt(if narrow {
                        px(12.)
                    } else {
                        px(28.)
                    })
                    .pb_4()
                    .when(narrow, |this| {
                        this.child(
                            h_flex().child(
                                Button::new("back-to-machines")
                                    .ghost()
                                    .small()
                                    .label("Machines")
                                    .icon(Icon::new(Glyph::ChevronLeft))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library_open = true;
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .child(div().max_w(px(content_width)).w_full().mx_auto().child(if narrow {
                        v_flex().gap_4().child(identity).child(controls).into_any_element()
                    } else {
                        h_flex()
                            .gap_6()
                            .justify_between()
                            .child(identity)
                            .child(controls)
                            .into_any_element()
                    }))
                    .when(true, |this| {
                        this.child(
                            div().w_full().max_w(px(content_width)).mx_auto().child(
                                TabBar::new("machine-tabs")
                                    .segmented()
                                    .selected_index(self.tab)
                                    .children(TABS.into_iter().map(|title| Tab::new().label(title)))
                                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                                        this.tab = *index;
                                        cx.notify();
                                    })),
                            ),
                        )
                    }),
            )
            .when(true, |this| {
                this.child(
                    div()
                        .id("machine-content")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(gutter)
                        .pt_2()
                        .pb_8()
                        .child(
                            v_flex()
                                .max_w(px(content_width))
                                .mx_auto()
                                .w_full()
                                .gap_6()
                                .when_some(
                                    self.details_error.clone().filter(|_| self.tab != 0),
                                    |this, error| {
                                        this.child(
                                            h_flex()
                                                .gap_2()
                                                .text_color(p.danger)
                                                .child(Icon::new(Glyph::CircleAlert).size_4())
                                                .child(error),
                                        )
                                    },
                                )
                                .child(
                                    // A new tab or machine settles in place: one plane, one
                                    // short fade with a few pixels of travel.
                                    div().relative().w_full().child(content).with_animation(
                                        SharedString::from(format!(
                                            "content-{}-{}",
                                            machine.id, self.tab
                                        )),
                                        Animation::new(motion::SWAP).with_easing(motion::ease_out),
                                        |el, t| el.opacity(t).top(px(motion::TRAVEL * (1. - t))),
                                    ),
                                ),
                        ),
                )
            })
            .when(self.tab == 1 && self.hardware_dirty(), |this| {
                this.child(
                    h_flex()
                        .relative()
                        .flex_shrink_0()
                        .px(gutter)
                        .py_3()
                        .border_t_1()
                        .border_color(p.border)
                        .child(
                            h_flex()
                                .w_full()
                                .max_w(px(content_width))
                                .mx_auto()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("discard-hardware")
                                        .outline()
                                        .label("Discard")
                                        .disabled(busy)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.discard_hardware(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("save-hardware")
                                        .primary()
                                        .label(
                                            if self
                                                .pending
                                                .as_ref()
                                                .is_some_and(|job| job.hardware_save.is_some())
                                            {
                                                "Saving…"
                                            } else {
                                                "Save changes"
                                            },
                                        )
                                        .disabled(busy)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.save_hardware(cx)),
                                        ),
                                ),
                        )
                        .with_animation(
                            "save-bar",
                            Animation::new(motion::REVEAL).with_easing(motion::ease_out),
                            |el, t| el.opacity(t).top(px(8. * (1. - t))),
                        ),
                )
            })
            .into_any_element()
    }

    /// Main pane before a machine exists or while the library connects.
    fn placeholder(&self, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let center = v_flex().flex_1().size_full().items_center().justify_center().gap_4().p_6();
        if !self.loaded {
            return center
                .child(
                    h_flex()
                        .gap_2()
                        .text_color(p.muted)
                        .child(Spinner::new().small())
                        .child("Connecting"),
                )
                .into_any_element();
        }
        if self.library_error.is_some() && self.machines.is_empty() {
            return center
                .child(Icon::new(Glyph::ServerOff).size(px(32.)).text_color(p.muted))
                .child(
                    div()
                        .text_size(px(BODY_SIZE + 2.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("No connection"),
                )
                .child(Button::new("retry-connection").outline().label("Retry").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.begin(crate::app::Job::Refresh, None, cx);
                    }),
                ))
                .into_any_element();
        }
        center
            .child(
                div()
                    .size(px(48.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .bg(p.well)
                    .child(Icon::new(Glyph::Monitor).size_6().text_color(p.muted)),
            )
            .child(
                div()
                    .text_size(px(BODY_SIZE + 2.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No machines"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("create-first")
                            .primary()
                            .label("New machine")
                            .icon(Icon::new(Glyph::Plus))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create(&NewMachineAction, window, cx)
                            })),
                    )
                    .child(self.import_menu(
                        Button::new("import-first").outline().label("Import").dropdown_caret(true),
                        cx,
                    )),
            )
            .into_any_element()
    }
}
