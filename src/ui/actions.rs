use super::primitives::{MENU_MIN_WIDTH, glyph};
use crate::app::{Job, Workspace};
use crate::backend::Machine;
use crate::theme::Palette;
use crate::workflows;
use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex};
use gpui_kit::{prelude::*, *};

impl Workspace {
    /// "+" menu: create or import. One entry point for every way a machine enters the library.
    pub(crate) fn add_menu(&self, trigger: Button, cx: &mut Context<Self>) -> impl IntoElement {
        self.entry_menu(trigger, true, Anchor::TopRight, cx)
    }

    pub(crate) fn import_menu(&self, trigger: Button, cx: &mut Context<Self>) -> impl IntoElement {
        self.entry_menu(trigger, false, Anchor::TopLeft, cx)
    }

    fn entry_menu(
        &self,
        trigger: Button,
        include_new: bool,
        anchor: Anchor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let busy = self.busy();
        trigger.dropdown_menu_with_anchor(anchor, move |mut menu, _, _| {
            let item = |label: &'static str, form: fn(&Workspace) -> crate::forms::FormSpec| {
                let weak = weak.clone();
                PopupMenuItem::new(label).disabled(busy).on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| this.open_form(form(this), window, cx));
                })
            };
            if include_new {
                menu = menu
                    .item(item("New machine", |w| workflows::create(w.preferences.machines_dir())))
                    .separator();
            }
            menu.item(item("Import OVA or OVF", |w| {
                workflows::import(w.preferences.machines_dir())
            }))
            .item(item("Import libvirt XML", |_| workflows::import_xml()))
            .separator()
            .item(item("Convert disk", |_| workflows::convert_disk()))
        })
    }

    /// One flat menu of the actions that apply right now, grouped by what they touch.
    pub(crate) fn machine_menu(
        &self,
        machine: &Machine,
        trigger: Button,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let busy = self.busy();
        let machine = machine.clone();
        trigger.dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, window, cx| {
            let danger = Palette::get(cx).danger;
            let entries = workflows::machine_actions(&machine);
            menu = menu.min_w(px(MENU_MIN_WIDTH));
            let mut group = None;
            for entry in entries {
                if group.is_some_and(|g| g != entry.group) {
                    menu = menu.separator();
                }
                group = Some(entry.group);
                let weak = weak.clone();
                let machine = machine.clone();
                let action = entry.action;
                let icon = glyph(entry.icon);
                let item = PopupMenuItem::new(entry.label)
                    .icon(if entry.danger {
                        icon.text_color(danger)
                    } else {
                        icon
                    })
                    .disabled(busy)
                    .on_click(move |_, window, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            if matches!(action, workflows::MachineAction::ViewLog) {
                                this.show_log(&machine, window, cx);
                            } else if let Some(form) = workflows::action_form(action, &machine) {
                                this.open_form(form, window, cx);
                            } else if let Some(op) = workflows::direct_action(action, &machine) {
                                this.begin(Job::Operation(machine.clone(), op), None, cx);
                            }
                        });
                    });
                menu = menu.item(item);
            }
            let _ = (window, cx);
            menu
        })
    }
}

impl Workspace {
    /// The machine's QEMU log, read in place. A viewer that never depends on which
    /// application the desktop associates with plain text.
    pub(crate) fn show_log(&self, machine: &Machine, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.preferences.connection;
        let text = if self.demo {
            Ok("2026-11-18 09:12:04.118+0000: starting up libvirt version: 11.9.0, qemu version: 10.1.0\n\
                LC_ALL=C PATH=/usr/bin HOME=/home/user /usr/bin/qemu-system-x86_64 -name guest=demo\n\
                char device redirected to /dev/pts/3 (label charserial0)\n\
                2026-11-18 09:40:51.003+0000: shutting down, reason=shutdown"
                .to_owned())
        } else {
            crate::operations::read_log(connection, machine, 256 * 1024)
        };
        let path = crate::operations::log_path(connection, machine).ok();
        let title = format!("Log for {}", machine.name);
        window.open_dialog(cx, move |dialog, _, cx| {
            let p = Palette::get(cx);
            let mono = cx.theme().mono_font_family.clone();
            let body = match &text {
                Ok(log) if !log.trim().is_empty() => div()
                    .id("log-text")
                    .h(px(420.))
                    .overflow_y_scroll()
                    .rounded_md()
                    .bg(p.well)
                    .p_3()
                    .font_family(mono)
                    .text_size(px(crate::theme::CAPTION_SIZE))
                    .line_height(px(18.))
                    .child(log.clone())
                    .into_any_element(),
                Ok(_) => div().text_color(p.muted).child("The log is empty.").into_any_element(),
                Err(error) => div().text_color(p.muted).child(error.to_string()).into_any_element(),
            };
            let folder = path.as_ref().and_then(|p| p.parent()).map(std::path::Path::to_path_buf);
            let copy = text.as_ref().ok().cloned().unwrap_or_default();
            crate::forms::frame(dialog, title.clone(), 760.).child(body).footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .when_some(folder, |row, folder| {
                        row.child(
                            Button::new("log-folder")
                                .ghost()
                                .label("Show in folder")
                                .icon(glyph("folder-open"))
                                .on_click(move |_, window, cx| {
                                    if let Err(error) = crate::operations::open_path(&folder) {
                                        window.push_notification(error.to_string(), cx);
                                    }
                                }),
                        )
                    })
                    .child(div().flex_1())
                    .when(!copy.is_empty(), |row| {
                        row.child(Button::new("log-copy").outline().label("Copy").on_click(
                            move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                window.push_notification("Log copied", cx);
                            },
                        ))
                    })
                    .child(
                        Button::new("log-done")
                            .primary()
                            .label("Done")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
        });
    }
}
