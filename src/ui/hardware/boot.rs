use crate::app::Workspace;
use crate::theme::{Palette, ROW_HEIGHT};
use crate::ui::primitives::{caption, notice, section};
use crate::workflows::HardwareDraft;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    /// Bootable devices in order. Switches choose which are tried; arrows reorder.
    pub(super) fn boot_order(
        &self,
        d: &HardwareDraft,
        enabled: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = d.boot.len();
        let list = section("Boot order", None);
        if count == 0 {
            return list.child(notice("No bootable devices", p));
        }
        let mut rows = Vec::new();
        for (i, entry) in d.boot.iter().enumerate() {
            let weak = cx.entity().downgrade();
            let drop_target = cx.entity().downgrade();
            let drag = BootDrag {
                from: i,
                label: entry.label.clone(),
                palette: p,
            };
            rows.push(
                h_flex()
                    .id(("boot-row", i))
                    .gap_3()
                    .min_h(px(ROW_HEIGHT))
                    .rounded_md()
                    .border_t_2()
                    .border_color(gpui_kit::transparent_black())
                    .when(enabled, |row| {
                        row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                            .drag_over::<BootDrag>(move |style, _, _, _| style.border_color(p.text))
                            .on_drop(move |dragged: &BootDrag, _, cx| {
                                let from = dragged.from;
                                let _ = drop_target.update(cx, |this, cx| {
                                    this.edit_hardware(
                                        |d| {
                                            let moved = d.boot.remove(from);
                                            d.boot.insert(i, moved);
                                        },
                                        cx,
                                    )
                                });
                            })
                    })
                    .child(Icon::new(Glyph::GripVertical).size_4().flex_shrink_0().text_color(
                        if enabled {
                            p.muted
                        } else {
                            p.border
                        },
                    ))
                    .child(
                        Switch::new(("boot-on", i))
                            .checked(entry.enabled)
                            .accessibility_label(format!("Boot from {}", entry.label))
                            .disabled(!enabled)
                            .on_change(move |on, _, cx| {
                                let on = *on;
                                let _ = weak.update(cx, |this, cx| {
                                    this.edit_hardware(|d| d.boot[i].enabled = on, cx)
                                });
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(if entry.enabled {
                                        p.text
                                    } else {
                                        p.muted
                                    })
                                    .child(entry.label.clone()),
                            )
                            .child(caption(entry.detail.clone(), p).truncate()),
                    )
                    .child(
                        Button::new(("boot-up", i))
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::ArrowUp))
                            .accessibility_label(format!("Move {} up", entry.label))
                            .disabled(!enabled || i == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_hardware(|d| d.boot.swap(i, i - 1), cx)
                            })),
                    )
                    .child(
                        Button::new(("boot-down", i))
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::ArrowDown))
                            .accessibility_label(format!("Move {} down", entry.label))
                            .disabled(!enabled || i + 1 == count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_hardware(|d| d.boot.swap(i, i + 1), cx)
                            })),
                    )
                    .into_any_element(),
            );
        }
        list.child(crate::ui::primitives::item_list(rows, p))
    }
}

/// A boot device being dragged to a new position; also its drag preview.
#[derive(Clone)]
struct BootDrag {
    from: usize,
    label: String,
    palette: Palette,
}
impl Render for BootDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .bg(p.sidebar)
            .shadow_md()
            .text_color(p.text)
            .child(Icon::new(Glyph::GripVertical).size_4().text_color(p.muted))
            .child(self.label.clone())
    }
}
