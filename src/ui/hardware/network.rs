use crate::app::Workspace;
use crate::backend::Machine;
use crate::operations::Operation;
use crate::theme::Palette;
use crate::ui::primitives::{caption, notice, section, setting as setting_row};
use crate::workflows::{self, DeviceAction, HardwareDraft};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    pub(super) fn network(
        &self,
        machine: &Machine,
        d: &HardwareDraft,
        editable: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let new = self.add_device_menu(
            "add-network",
            machine,
            &[DeviceAction::Network, DeviceAction::PortForward],
            &[
                ("Create virtual network…", workflows::virtual_network),
                ("Create host bridge…", workflows::host_bridge),
            ],
            cx,
        );
        let forwards = self.device_rows(machine, &["forward"], cx);
        let mut group = section("Network", Some(new));
        if d.nics.is_empty() {
            return group.child(notice("No network cards", p)).when(!forwards.is_empty(), |g| {
                g.child(crate::ui::primitives::item_list(forwards, p))
            });
        }
        for (i, nic) in d.nics.iter().enumerate() {
            let setting = |label: &'static str, control: AnyElement| setting_row(label, control, p);
            let card_xml = self
                .details
                .devices
                .iter()
                .find(|dev| dev.kind == "network" && dev.target == nic.mac)
                .map(|dev| dev.xml.clone());
            // The card itself reads like a Storage row: which card, its address, and a
            // menu for actions on the card. Its mode is the setting just below, not repeated.
            let title = format!("Network card {}", i + 1);
            let weak = cx.entity().downgrade();
            let busy = self.busy();
            let card_menu = Button::new(("nic-actions", i))
                .ghost()
                .small()
                .icon(Icon::new(Glyph::Ellipsis))
                .accessibility_label(format!("Network card {} actions", i + 1))
                .disabled(!editable)
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let renew = {
                        let weak = weak.clone();
                        PopupMenuItem::new("New MAC address")
                            .icon(crate::ui::primitives::glyph("refresh-cw"))
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.edit_hardware(
                                        |d| d.nics[i].new_mac = crate::operations::random_mac(),
                                        cx,
                                    )
                                });
                            })
                    };
                    let menu = menu.min_w(px(crate::ui::primitives::MENU_MIN_WIDTH)).item(renew);
                    match card_xml.clone() {
                        Some(xml) => {
                            let weak = weak.clone();
                            menu.separator().item(
                                PopupMenuItem::new("Remove card…")
                                    .icon(crate::ui::primitives::glyph("trash"))
                                    .disabled(busy)
                                    .on_click(move |_, window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            if let Some(m) = this.selected_machine().cloned() {
                                                this.open_form(
                                                    workflows::confirm(
                                                        "Remove network card",
                                                        "Remove",
                                                        "The guest loses this connection.",
                                                        m,
                                                        Operation::DetachDevice(xml.clone()),
                                                    ),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        });
                                    }),
                            )
                        }
                        None => menu,
                    }
                });
            let card = v_flex()
                .pb_2()
                .child(
                    h_flex()
                        .gap_3()
                        .min_h(px(48.))
                        .child(
                            Icon::new(Glyph::Network).size_4().flex_shrink_0().text_color(p.muted),
                        )
                        .child(
                            v_flex().flex_1().min_w_0().child(title).child(
                                caption(nic.new_mac.clone(), p)
                                    .font_family(cx.theme().mono_font_family.clone()),
                            ),
                        )
                        .child(card_menu),
                )
                .child(setting(
                    "Mode",
                    self.segmented(
                        format!("nic-mode-{i}"),
                        &["Shared", "Virtual network", "Bridged"],
                        match nic.kind.as_str() {
                            "network" => 1,
                            "bridge" => 2,
                            _ => 0,
                        },
                        editable,
                        move |d, pick| {
                            let nic = &mut d.nics[i];
                            nic.kind = ["user", "network", "bridge"][pick].into();
                            nic.source.clear();
                        },
                        cx,
                    ),
                ))
                .when(nic.kind != "user", |g| {
                    let options = if nic.kind == "network" {
                        self.details.networks.clone()
                    } else {
                        self.details.bridges.clone()
                    };
                    g.child(setting(
                        if nic.kind == "network" {
                            "Network"
                        } else {
                            "Bridge"
                        },
                        self.list_menu(
                            ("nic-source", i),
                            options,
                            &nic.source,
                            editable,
                            move |d, v| d.nics[i].source = v,
                            cx,
                        ),
                    ))
                })
                .child(setting(
                    "Model",
                    self.choice_menu(
                        ("nic-model", i),
                        workflows::NIC_MODELS,
                        &nic.model,
                        editable,
                        move |d, v| d.nics[i].model = v,
                        cx,
                    ),
                ))
                .when(nic.kind != "user", |g| {
                    g.child(setting(
                        "Isolated",
                        self.switch(
                            ("nic-isolated", i),
                            "Isolate from other guests",
                            nic.isolated,
                            editable,
                            move |d, on| d.nics[i].isolated = on,
                            cx,
                        ),
                    ))
                });
            group = group.child(crate::ui::primitives::item_list(vec![card.into_any_element()], p));
        }
        group.when(!forwards.is_empty(), |g| g.child(crate::ui::primitives::item_list(forwards, p)))
    }

    /// Compact menu over a runtime list, such as host networks or bridges.
    fn list_menu(
        &self,
        id: impl Into<ElementId>,
        options: Vec<String>,
        current: &str,
        enabled: bool,
        apply: impl Fn(&mut HardwareDraft, String) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        if options.is_empty() {
            return div().text_color(p.muted).child("None").into_any_element();
        }
        let apply = std::rc::Rc::new(apply);
        let weak = cx.entity().downgrade();
        let current = current.to_owned();
        let shown = if current.is_empty() {
            "Choose".to_owned()
        } else {
            current.clone()
        };
        h_flex()
            .child(
                Button::new(id)
                    .outline()
                    .small()
                    .label(shown)
                    .dropdown_caret(true)
                    .disabled(!enabled)
                    .dropdown_menu(move |mut menu, _, _| {
                        for option in &options {
                            let weak = weak.clone();
                            let apply = apply.clone();
                            let value = option.clone();
                            menu = menu.item(
                                PopupMenuItem::new(option.clone())
                                    .checked(*option == current)
                                    .on_click(move |_, _, cx| {
                                        let value = value.clone();
                                        let apply = apply.clone();
                                        let _ = weak.update(cx, |this, cx| {
                                            this.edit_hardware(|d| apply(d, value), cx)
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .into_any_element()
    }
}
