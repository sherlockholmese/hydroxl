use super::primitives::caption;
use crate::app::Workspace;
use crate::backend::{Machine, State};
use crate::theme::{BODY_SIZE, Palette};
use crate::workflows::{self, SnapshotAction};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    pub(crate) fn snapshots(&self, machine: &Machine, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::get(cx);
        let busy = self.busy();
        let restorable = machine.state == State::Stopped && !machine.saved_state;
        let weak = cx.entity().downgrade();
        let m = machine.clone();
        let mut rows = v_flex();
        for (i, snapshot) in self.details.snapshots.iter().enumerate() {
            let s = snapshot.clone();
            let weak = weak.clone();
            let m = machine.clone();
            let menu = Button::new(("snapshot-actions", i))
                .ghost()
                .small()
                .icon(Icon::new(Glyph::Ellipsis))
                .accessibility_label(format!("{} actions", s.label))
                .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                    for action in SnapshotAction::ALL {
                        let weak = weak.clone();
                        let m = m.clone();
                        let s = s.clone();
                        if action == SnapshotAction::Delete {
                            menu = menu.separator();
                        }
                        menu = menu.item(
                            PopupMenuItem::new(action.label())
                                .disabled(
                                    busy || (action == SnapshotAction::Restore && !restorable),
                                )
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.open_form(
                                            workflows::snapshot_action(action, &m, &s),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        );
                    }
                    menu
                });
            let detail = match &snapshot.parent {
                Some(parent) => format!("{}, from {parent}", snapshot.state),
                None => snapshot.state.clone(),
            };
            rows = rows.child(
                h_flex()
                    .gap_3()
                    .py_3()
                    .border_b_1()
                    .border_color(p.border)
                    .child(Icon::new(Glyph::Camera).size_4().flex_shrink_0().text_color(p.muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(BODY_SIZE))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(snapshot.label.clone()),
                            )
                            .child(caption(detail, p)),
                    )
                    .child(menu),
            );
        }
        v_flex()
            .gap_2()
            .child(
                h_flex().justify_end().child(
                    Button::new("new-snapshot")
                        .outline()
                        .small()
                        .label("Take snapshot")
                        .icon(Icon::new(Glyph::Camera))
                        .disabled(busy || self.demo)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_form(workflows::snapshot(&m), window, cx)
                        })),
                ),
            )
            .when_some(self.details.snapshot_error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_color(p.danger)
                        .child(Icon::new(Glyph::CircleAlert).size_4())
                        .child(error),
                )
            })
            .when(self.details.snapshots.is_empty(), |this| {
                this.child(div().py_6().text_color(p.muted).child(if busy {
                    "Loading"
                } else {
                    "No snapshots"
                }))
            })
            .child(rows)
            .into_any_element()
    }
}
