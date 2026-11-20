use super::primitives::caption;
use crate::app::{Job, Workspace};
use crate::backend::Machine;
use crate::forms::FormSpec;
use crate::operations::{Device, Operation};
use crate::theme::{BODY_SIZE, Palette};
use crate::workflows::{self, DeviceAction};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

/// What a device row's menu can do. Forms open a dialog; direct operations run at once.
#[derive(Clone)]
enum RowAction {
    Form(fn(&Machine, &Device) -> FormSpec),
    Direct(fn(&Device) -> Operation),
}

fn row_actions(device: &Device) -> Vec<(&'static str, RowAction, bool)> {
    // (label, action, requires powered-off machine)
    match device.kind.as_str() {
        "cdrom" => vec![
            ("Mount ISO", RowAction::Form(|m, d| workflows::mount(m, d.target.clone())), false),
            (
                "Install guest tools",
                RowAction::Form(|m, d| workflows::guest_tools(m, Some(d.target.clone()))),
                false,
            ),
            ("Eject", RowAction::Direct(|d| Operation::EjectIso(d.target.clone())), false),
            ("Edit drive…", RowAction::Form(workflows::edit_drive), true),
        ],
        "disk" => vec![
            ("Edit drive…", RowAction::Form(workflows::edit_drive), true),
            ("Resize", RowAction::Form(|m, d| workflows::resize(m, d.target.clone())), true),
            ("Compact", RowAction::Form(|m, d| workflows::compact(m, d.target.clone())), true),
            (
                "Detach",
                RowAction::Form(|m, d| {
                    workflows::confirm(
                        "Detach disk",
                        "Detach",
                        "The disk file is kept.",
                        m.clone(),
                        Operation::DetachDisk(d.target.clone()),
                    )
                }),
                true,
            ),
        ],
        kind => {
            let remove: fn(&Machine, &Device) -> FormSpec = if kind == "forward" {
                |m, d| {
                    workflows::confirm(
                        "Remove port forward",
                        "Remove",
                        "The rule is removed at the next start.",
                        m.clone(),
                        Operation::RemovePortForward(d.xml.clone()),
                    )
                }
            } else {
                |m, d| {
                    workflows::confirm(
                        "Remove device",
                        "Remove",
                        "The device is disconnected at the next start.",
                        m.clone(),
                        Operation::DetachDevice(d.xml.clone()),
                    )
                }
            };
            vec![("Remove", RowAction::Form(remove), true)]
        }
    }
}

fn glyph(kind: &str) -> Glyph {
    match kind {
        "cdrom" => Glyph::Disc3,
        "network" | "forward" => Glyph::Network,
        "usb" => Glyph::Usb,
        _ => Glyph::HardDrive,
    }
}

impl Workspace {
    /// "Add" menu for one hardware section. Each entry opens its focused form.
    pub(crate) fn add_device_menu(
        &self,
        id: &'static str,
        machine: &Machine,
        actions: &'static [DeviceAction],
        extra: &'static [(&'static str, fn() -> FormSpec)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let busy = self.busy();
        let editable = machine.can_edit();
        let m = machine.clone();
        Button::new(id)
            .ghost()
            .small()
            .label("Add")
            .icon(Icon::new(Glyph::Plus))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                menu = menu.min_w(px(super::primitives::MENU_MIN_WIDTH));
                for &action in actions {
                    let weak = weak.clone();
                    let m = m.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{}…", action.label()))
                            .icon(super::primitives::glyph(action.icon()))
                            .disabled(busy || (action.requires_power_off() && !editable))
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    if let Some(form) = workflows::device_form(action, &m) {
                                        this.open_form(form, window, cx);
                                    }
                                });
                            }),
                    );
                }
                if !extra.is_empty() {
                    menu = menu.separator();
                }
                for &(label, form) in extra {
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(label).disabled(busy).on_click(
                        move |_, window, cx| {
                            let _ = weak.update(cx, |this, cx| this.open_form(form(), window, cx));
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    /// Ledger rows for the devices a section owns, each with its own action menu.
    pub(crate) fn device_rows(
        &self,
        machine: &Machine,
        kinds: &[&str],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let p = Palette::get(cx);
        let busy = self.busy();
        let editable = machine.can_edit();
        let weak = cx.entity().downgrade();
        let mut rows = Vec::new();
        for (index, device) in
            self.details.devices.iter().filter(|d| kinds.contains(&d.kind.as_str())).enumerate()
        {
            let d = device.clone();
            let m = machine.clone();
            let weak = weak.clone();
            let size = self
                .details
                .disk_sizes
                .iter()
                .find(|(target, ..)| *target == device.target && device.kind == "disk")
                .map(|(_, capacity, used)| {
                    format!("{} GB, {} used", gib(*capacity), human_bytes(*used))
                });
            let menu = Button::new((SharedString::from(format!("device-{}", kinds[0])), index))
                .ghost()
                .small()
                .icon(Icon::new(Glyph::Ellipsis))
                .accessibility_label(format!("{} {} actions", d.label, d.target))
                .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                    for (label, action, needs_off) in row_actions(&d) {
                        let disabled = busy
                            || (needs_off && !editable)
                            || (label == "Eject" && d.source == "Empty");
                        let weak = weak.clone();
                        let m = m.clone();
                        let d = d.clone();
                        menu = menu.item(PopupMenuItem::new(label).disabled(disabled).on_click(
                            move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| match &action {
                                    RowAction::Form(form) => {
                                        this.open_form(form(&m, &d), window, cx)
                                    }
                                    RowAction::Direct(op) => {
                                        this.begin(Job::Operation(m.clone(), op(&d)), None, cx);
                                    }
                                });
                            },
                        ));
                    }
                    menu
                });
            let source = if device.source.contains('/') {
                std::path::Path::new(&device.source)
                    .file_name()
                    .map_or(device.source.clone(), |n| n.to_string_lossy().into_owned())
            } else {
                device.source.clone()
            };
            rows.push(
                h_flex()
                    .gap_3()
                    .min_h(px(48.))
                    .child(
                        Icon::new(glyph(&device.kind)).size_4().flex_shrink_0().text_color(p.muted),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_size(px(BODY_SIZE))
                                    .child(div().truncate().child(source))
                                    .when_some(size, |row, size| {
                                        row.child(
                                            div().flex_shrink_0().text_color(p.muted).child(size),
                                        )
                                    }),
                            )
                            .child(
                                caption(
                                    if device.target.is_empty() {
                                        device.label.clone()
                                    } else {
                                        format!("{} {}", device.label, device.target)
                                    },
                                    p,
                                )
                                .truncate(),
                            ),
                    )
                    .child(menu)
                    .into_any_element(),
            );
        }
        rows
    }
}

fn gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1u64 << 30) as f64;
    if gib.fract() < 0.05 {
        format!("{gib:.0}")
    } else {
        format!("{gib:.1}")
    }
}

/// Bytes as a short size, in GB above 1 GB and MB below.
pub(crate) fn human_bytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{} GB", gib(bytes))
    } else {
        format!("{} MB", bytes >> 20)
    }
}
