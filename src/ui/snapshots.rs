use super::primitives::{caption, item_list, notice, plain_error, section};
use crate::app::Workspace;
use crate::backend::{Machine, State};
use crate::theme::{BODY_SIZE, Palette, ROW_HEIGHT};
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
        let mut rows = Vec::new();
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
                    menu = menu.min_w(px(super::primitives::MENU_MIN_WIDTH));
                    for action in SnapshotAction::ALL {
                        let weak = weak.clone();
                        let m = m.clone();
                        let s = s.clone();
                        if action == SnapshotAction::Delete {
                            menu = menu.separator();
                        }
                        menu = menu.item(
                            PopupMenuItem::new(format!("{}…", action.label()))
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
            rows.push(
                h_flex()
                    .gap_3()
                    .min_h(px(ROW_HEIGHT))
                    .py_2()
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
                    .child(menu)
                    .into_any_element(),
            );
        }
        let loading = self.details_for != self.selected;
        let take = Button::new("new-snapshot")
            .ghost()
            .small()
            .label("Take snapshot…")
            .icon(Icon::new(Glyph::Plus))
            .disabled(busy || self.demo)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_form(workflows::snapshot(&m), window, cx)
            }))
            .into_any_element();
        let has_snapshots = !rows.is_empty();
        v_flex()
            .gap_6()
            // Restore needs a powered-off machine; say so once, with the way to get there.
            .when(has_snapshots && !restorable, |this| {
                this.child(self.locked_notice(machine, "restore", p, cx))
            })
            .child(
                section("Snapshots", Some(take))
                    .when_some(self.details.snapshot_error.clone(), |this, error| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .text_color(p.danger)
                                .child(Icon::new(Glyph::CircleAlert).size_4().flex_shrink_0())
                                .child(div().flex_1().min_w_0().child(plain_error(&error))),
                        )
                    })
                    .when(!has_snapshots, |this| {
                        this.child(notice(
                            if loading {
                                "Loading"
                            } else {
                                "No snapshots"
                            },
                            p,
                        ))
                    })
                    .when(has_snapshots, |this| this.child(item_list(rows, p))),
            )
            .into_any_element()
    }
}
