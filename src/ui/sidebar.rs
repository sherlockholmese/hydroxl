use super::primitives::{logo_tile, status_color};
use crate::PreferencesAction;
use crate::app::{Job, Workspace};
use crate::theme::{BODY_SIZE, CAPTION_SIZE, HEADER_HEIGHT, Palette, ROW_HEIGHT, SIDEBAR_WIDTH};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    fn activity(&self, p: Palette, _cx: &mut Context<Self>) -> AnyElement {
        let Some(pending) = self.pending.as_ref().filter(|pending| !pending.refresh) else {
            return div()
                .truncate()
                .child(if self.demo {
                    "Preview"
                } else {
                    self.preferences.connection.label()
                })
                .into_any_element();
        };
        // Status, not a control: what is happening, and how far along when that is known.
        // The stage replaces the label when the operation reports one.
        let text = if pending.progress.stage.is_empty() {
            pending.label.to_owned()
        } else {
            pending.progress.stage.clone()
        };
        let percent = pending.progress.percent;
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(Spinner::new().small().color(p.muted))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().child(text))
                            .when_some(percent, |row, percent| {
                                row.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(CAPTION_SIZE))
                                        .text_color(p.muted)
                                        .child(format!("{percent}%")),
                                )
                            }),
                    )
                    .when_some(percent, |column, percent| {
                        column.child(
                            div().h(px(2.)).w_full().rounded_full().bg(p.border).child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(p.text)
                                    .w(relative(f32::from(percent.min(100)) / 100.)),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(crate) fn sidebar(&self, full: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::get(cx);
        let query = self.search.read(cx).value().to_lowercase();
        let filtered: Vec<_> =
            self.machines.iter().filter(|m| m.name.to_lowercase().contains(&query)).collect();
        let mut list = v_flex().gap_px();
        for machine in &filtered {
            let id = machine.id.clone();
            let selected = self.selected.as_ref() == Some(&machine.id);
            let menu = self.machine_context_menu(machine, cx);
            let row_id = machine.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("machine-row-{}", machine.id)))
                    .w_full()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| this.select(row_id.clone(), cx)),
                    )
                    .context_menu(menu)
                    .child(
                        Button::new(SharedString::from(format!("machine-{}", machine.id)))
                            .accessibility_label(format!("{}, {}", machine.name, machine.status()))
                            .custom(
                                ButtonCustomVariant::new(cx)
                                    .color(if selected {
                                        p.sidebar_accent
                                    } else {
                                        p.sidebar
                                    })
                                    .hover(if selected {
                                        p.sidebar_accent
                                    } else {
                                        p.sidebar_accent.opacity(0.5)
                                    })
                                    .active(p.sidebar_accent)
                                    .foreground(p.text),
                            )
                            .selected(selected)
                            .w_full()
                            .h(px(ROW_HEIGHT))
                            .px_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .w_full()
                                    .min_w_0()
                                    .child(logo_tile(machine, 20., p, cx))
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .flex_1()
                                            .child(
                                                div()
                                                    .text_size(px(BODY_SIZE))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .truncate()
                                                    .child(machine.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(CAPTION_SIZE))
                                                    .text_color(status_color(machine, p))
                                                    .child(machine.status()),
                                            ),
                                    ),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)),
                            ),
                    ),
            );
        }
        if filtered.is_empty() && !query.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_3()
                    .text_size(px(BODY_SIZE))
                    .text_color(p.muted)
                    .child("No results"),
            );
        }
        let status = self.activity(p, cx);
        let panel = v_flex()
            .size_full()
            .bg(p.sidebar)
            .when(!full, |e| e.rounded_lg().border_1().border_color(p.border).shadow_sm())
            .child(
                h_flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_shrink_0()
                    .pl_4()
                    .pr_2()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(BODY_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Machines"),
                    )
                    .child(
                        self.add_menu(
                            Button::new("add-machine")
                                .ghost()
                                .small()
                                .icon(Icon::new(Glyph::Plus))
                                .accessibility_label("Add machine")
                                .tooltip("Add machine"),
                            cx,
                        ),
                    ),
            )
            .when(!self.machines.is_empty(), |this| {
                this.child(
                    div().px_2().pb_2().child(
                        Input::new(&self.search)
                            .small()
                            .aria_label("Search machines")
                            .prefix(Icon::new(Glyph::Search).size_4().text_color(p.muted))
                            .cleanable(true),
                    ),
                )
            })
            .child(
                div()
                    .id("machine-library")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .child(list),
            )
            .child(
                h_flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_shrink_0()
                    .pl_4()
                    .pr_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(p.border)
                    .text_size(px(CAPTION_SIZE))
                    .text_color(p.muted)
                    .child(div().flex_1().min_w_0().child(status))
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::RefreshCw))
                            .accessibility_label("Refresh")
                            .tooltip("Refresh  F5")
                            .disabled(self.busy() || self.demo)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.begin(Job::Refresh, None, cx);
                            })),
                    )
                    .child(
                        Button::new("preferences")
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::Settings2))
                            .accessibility_label("Preferences")
                            .tooltip("Preferences  Ctrl ,")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.preferences(&PreferencesAction, window, cx)
                            })),
                    ),
            );
        // Floating plane: inset from the window edges with its own surface and depth.
        div()
            .h_full()
            .flex_shrink_0()
            .when(full, |e| e.w_full())
            .when(!full, |e| e.w(px(SIDEBAR_WIDTH + 16.)).p_2())
            .child(panel)
            .into_any_element()
    }
}
