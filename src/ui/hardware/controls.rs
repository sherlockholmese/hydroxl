use super::vcpus;
use crate::app::Workspace;
use crate::theme::Palette;
use crate::ui::primitives::{caption, notice, setting, tabular};
use crate::workflows::{HardwareDraft, label};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    /// Memory in MB: type an exact value, or step by 1 GB.
    pub(super) fn memory_control(
        &self,
        mib: u64,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let field = h_flex()
            .gap_2()
            .child(
                h_flex()
                    .h(px(32.))
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().input)
                    .overflow_hidden()
                    .child(
                        Button::new("memory-down")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Glyph::Minus))
                            .accessibility_label("1 GiB less memory")
                            .disabled(!enabled || mib <= 1024)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_hardware(
                                    |d| {
                                        d.memory_mib = ((d.memory_mib.saturating_sub(1)) / 1024
                                            * 1024)
                                            .max(1024)
                                    },
                                    cx,
                                )
                            })),
                    )
                    .child(
                        div().w(px(72.)).h(px(30.)).flex().items_center().child(
                            Input::new(&self.memory_input)
                                .appearance(false)
                                .small()
                                .aria_label("Memory in MiB")
                                .disabled(!enabled),
                        ),
                    )
                    .child(
                        Button::new("memory-up")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Glyph::Plus))
                            .accessibility_label("1 GiB more memory")
                            .disabled(!enabled || mib >= 1024 * 1024)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_hardware(
                                    |d| {
                                        d.memory_mib = (d.memory_mib / 1024 + 1)
                                            .saturating_mul(1024)
                                            .min(1024 * 1024)
                                    },
                                    cx,
                                )
                            })),
                    ),
            )
            .child(div().text_color(p.muted).child("MiB"));
        v_flex()
            .gap_2()
            .child(field)
            .when_some(self.memory_error().filter(|_| enabled), |column, error| {
                column.child(crate::ui::error_line(error, p))
            })
            .into_any_element()
    }

    /// CPU topology can wrap within its row. On narrow windows it uses the full pane.
    pub(super) fn cpu_controls(
        &self,
        original: &HardwareDraft,
        draft: &HardwareDraft,
        enabled: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let topology = draft.topology;
        let one_socket = crate::backend::Topology::one_socket(topology.vcpus());
        let steppers =
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_3()
                .child(self.stepper(
                    "Sockets",
                    topology.sockets,
                    1..=8,
                    enabled,
                    |d, v| d.topology.sockets = v,
                    cx,
                ))
                .child(self.stepper(
                    "Cores",
                    topology.cores,
                    1..=128,
                    enabled,
                    |d, v| d.topology.cores = v,
                    cx,
                ))
                .child(self.stepper(
                    "Threads",
                    topology.threads,
                    1..=4,
                    enabled,
                    |d, v| d.topology.threads = v,
                    cx,
                ))
                .child(
                    v_flex().flex_shrink_0().gap_1().child(caption("Total", p)).child(
                        div().h(px(32.)).flex().items_center().child(vcpus(topology.vcpus())),
                    ),
                );
        let controls = v_flex().w_full().gap_3().py_2().child(steppers).when(
            original.implicit_topology && original.topology.sockets > 1,
            |group| {
                group.child(
                    h_flex().child(
                        Button::new("cpu-single-socket")
                            .outline()
                            .small()
                            .label("Use 1 socket")
                            .disabled(!enabled || topology == one_socket || one_socket.cores > 256)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_hardware(
                                    |draft| {
                                        draft.topology = crate::backend::Topology::one_socket(
                                            draft.topology.vcpus(),
                                        );
                                        draft.implicit_topology = false;
                                    },
                                    cx,
                                );
                            })),
                    ),
                )
            },
        );
        if narrow {
            return v_flex().pt_2().child(notice("CPU", p)).child(controls).into_any_element();
        }
        setting("CPU", controls, p).into_any_element()
    }

    /// Joined number stepper, the same height and border as an input, with its unit after.
    pub(super) fn stepper(
        &self,
        unit: &'static str,
        value: u32,
        range: std::ops::RangeInclusive<u32>,
        enabled: bool,
        set: fn(&mut HardwareDraft, u32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let (min, max) = (*range.start(), *range.end());
        v_flex()
            .flex_shrink_0()
            .gap_1()
            .child(caption(unit, p))
            .child(
                h_flex()
                    .h(px(32.))
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().input)
                    .child(
                        Button::new((unit, 0usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Glyph::Minus))
                            .accessibility_label(format!("Fewer {unit}"))
                            .disabled(!enabled || value <= min)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_hardware(|d| set(d, value - 1), cx)
                            })),
                    )
                    .child(
                        div()
                            .w(px(32.))
                            .text_center()
                            .font_features(tabular())
                            .child(value.to_string()),
                    )
                    .child(
                        Button::new((unit, 1usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Glyph::Plus))
                            .accessibility_label(format!("More {unit}"))
                            .disabled(!enabled || value >= max)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_hardware(|d| set(d, value + 1), cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn choice_menu(
        &self,
        id: impl Into<ElementId>,
        registry: &'static [(&'static str, &'static str)],
        current: &str,
        enabled: bool,
        apply: impl Fn(&mut HardwareDraft, String) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let apply = std::rc::Rc::new(apply);
        let weak = cx.entity().downgrade();
        let current = current.to_owned();
        let shown = label(registry, &current).to_owned();
        h_flex()
            .child(
                Button::new(id)
                    .outline()
                    .small()
                    .label(shown)
                    .dropdown_caret(true)
                    .disabled(!enabled)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (value, text) in registry {
                            let weak = weak.clone();
                            let apply = apply.clone();
                            menu = menu.item(
                                PopupMenuItem::new(*text).checked(*value == current).on_click(
                                    move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.edit_hardware(
                                                |d| apply(d, (*value).to_owned()),
                                                cx,
                                            )
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            )
            .into_any_element()
    }

    pub(super) fn switch(
        &self,
        id: impl Into<ElementId>,
        name: &'static str,
        on: bool,
        enabled: bool,
        apply: impl Fn(&mut HardwareDraft, bool) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let apply = std::rc::Rc::new(apply);
        Switch::new(id)
            .checked(on)
            .accessibility_label(name)
            .disabled(!enabled)
            .on_change(move |value, _, cx| {
                let value = *value;
                let apply = apply.clone();
                let _ = weak.update(cx, |this, cx| this.edit_hardware(|d| apply(d, value), cx));
            })
            .into_any_element()
    }

    /// Segmented control in the same material as the tabs: muted track, raised selection.
    pub(super) fn segmented(
        &self,
        id: impl Into<SharedString>,
        options: &[&'static str],
        selected: usize,
        enabled: bool,
        pick: impl Fn(&mut HardwareDraft, usize) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.segmented_with(
            id,
            options,
            selected,
            enabled,
            move |this, i, cx| this.edit_hardware(|d| pick(d, i), cx),
            cx,
        )
    }

    /// Segmented control whose choice runs `on_pick` directly, for immediate settings.
    pub(super) fn segmented_with(
        &self,
        id: impl Into<SharedString>,
        options: &[&'static str],
        selected: usize,
        enabled: bool,
        on_pick: impl Fn(&mut Self, usize, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        crate::ui::primitives::segmented(
            id,
            options,
            Some(selected),
            enabled,
            move |i, _, cx| {
                let _ = weak.update(cx, |this, cx| on_pick(this, i, cx));
            },
            cx,
        )
    }

    pub(super) fn firmware_toggle(
        &self,
        uefi: bool,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.segmented(
            "firmware",
            &["BIOS", "UEFI"],
            uefi as usize,
            enabled,
            |d, i| d.uefi = i == 1,
            cx,
        )
    }

    pub(super) fn heads_toggle(
        &self,
        heads: u32,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.segmented(
            "heads",
            &["1", "2", "3", "4"],
            heads.clamp(1, 4) as usize - 1,
            enabled,
            |d, i| d.heads = i as u32 + 1,
            cx,
        )
    }
}
