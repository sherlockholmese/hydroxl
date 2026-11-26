mod boot;
mod controls;
mod guest;
mod network;

use crate::app::Workspace;
use crate::backend::{Action, Machine, State};
use crate::theme::{CAPTION_SIZE, Palette};
use crate::ui::primitives::{caption, ledger, notice, section, setting as setting_row};
use crate::workflows::{self, DeviceAction, label};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    /// Settings grouped the way UTM groups them, edited in place.
    pub(crate) fn hardware(
        &self,
        machine: &Machine,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let editable = machine.can_edit() && !self.busy() && !self.awaiting_hardware_readback();
        let values = self.hardware_values();
        let mut page = v_flex().gap_6();
        if !machine.can_edit() {
            page = page.child(self.locked_notice(machine, "edit", p, cx));
        }
        let Some((original, d)) = values else {
            return page
                .child(
                    section("System", None)
                        .child(ledger("Architecture", machine.architecture().to_owned(), p))
                        .child(ledger("CPU", vcpus(machine.cpus), p))
                        .child(ledger("Memory", machine.memory(), p)),
                )
                .child(notice(
                    if self.details_for != self.selected && self.library_error.is_none() {
                        "Loading"
                    } else {
                        "Configuration unavailable"
                    },
                    p,
                ))
                .into_any_element();
        };
        let machine_types = workflows::machine_types(&machine.arch);
        let current_type = workflows::machine_type_family(&d.machine_type);
        let setting = |label: &'static str, control: AnyElement| setting_row(label, control, p);

        // Settings here collect in the draft behind Save. The few that apply on their own
        // (operating system, architecture) read as a value plus a Change… action, so a
        // control never looks like part of the draft when it is not.
        let system = section("System", None)
            .child(setting("Operating system", {
                let m = machine.clone();
                let os = crate::backend::os_name(&machine.os).unwrap_or("Not set").to_owned();
                h_flex()
                    .gap_3()
                    .child(div().min_w_0().child(os))
                    .child(
                        Button::new("change-os")
                            .ghost()
                            .small()
                            .label("Change…")
                            .disabled(self.busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_form(workflows::operating_system(&m), window, cx)
                            })),
                    )
                    .into_any_element()
            }))
            .child(setting("Architecture", self.architecture_control(machine, cx)))
            .child(self.cpu_controls(&original, &d, editable, narrow, cx))
            .child(setting(
                "Memory",
                self.memory_control(d.memory_mib, editable, cx).into_any_element(),
            ))
            .child(setting(
                "CPU model",
                self.choice_menu(
                    "cpu-model",
                    workflows::CPU_MODELS,
                    &d.cpu_model,
                    editable,
                    |d, v| {
                        if v != "host-passthrough" {
                            d.nested_virtualization = false;
                        }
                        d.cpu_model = v;
                    },
                    cx,
                ),
            ))
            .when(
                machine.acceleration == "KVM" && matches!(machine.arch.as_str(), "x86_64" | "i686"),
                |section| {
                    section.child(setting(
                        "Nested virtualization",
                        self.switch(
                            "nested-virtualization",
                            "Nested virtualization",
                            d.nested_virtualization,
                            editable,
                            |draft, enabled| {
                                draft.nested_virtualization = enabled;
                                if enabled {
                                    draft.cpu_model = "host-passthrough".into();
                                }
                            },
                            cx,
                        ),
                    ))
                },
            )
            .child(setting("Firmware", self.firmware_toggle(d.uefi, editable, cx)))
            .child(setting(
                "Machine type",
                self.choice_menu(
                    "machine-type",
                    machine_types,
                    current_type,
                    editable && machine_types.iter().any(|(kind, _)| *kind != current_type),
                    |d, v| {
                        if workflows::machine_type_family(&d.machine_type) != v {
                            d.machine_type = v;
                        }
                    },
                    cx,
                ),
            ))
            .child(setting(
                "TPM 2.0",
                self.switch(
                    "tpm",
                    "TPM 2.0",
                    d.platform.tpm,
                    editable,
                    |d, on| d.platform.tpm = on,
                    cx,
                ),
            ));

        let video_memory = ["qxl", "vga", "bochs"].contains(&d.graphics.as_str());
        let vram = d.vram.to_string();
        let windows =
            crate::operations::GuestOs::of(machine) == crate::operations::GuestOs::Windows;
        let triton_arch = crate::backend::triton_arch(&machine.arch);
        let accel_label = if windows {
            "DirectX"
        } else {
            "3D acceleration"
        };
        let display = section("Display and sound", None)
            .child(setting(
                "Graphics",
                self.choice_menu(
                    "graphics",
                    workflows::GRAPHICS,
                    &d.graphics,
                    editable,
                    |d, v| {
                        if v != "virtio" {
                            d.accel3d = false;
                        }
                        d.graphics = v;
                    },
                    cx,
                ),
            ))
            .when(video_memory, |s| {
                s.child(setting(
                    "Video memory",
                    self.choice_menu(
                        "vram",
                        workflows::VIDEO_MEMORY,
                        &vram,
                        editable,
                        |d, v| d.vram = v.parse().unwrap_or(0),
                        cx,
                    ),
                ))
            })
            // One choice of how the machine is viewed. VNC and SPICE pick the viewer on this
            // computer and apply at once, even while it runs; D-Bus changes the machine's
            // configuration, so choosing it (or leaving it) goes into the draft and brings up
            // the Save bar, which is what tells the two apart.
            .child(setting("Viewer", {
                let id = machine.id.clone();
                let selected = if d.local_dbus {
                    2
                } else {
                    usize::from(machine.spice_viewer || d.accel3d)
                };
                let options: &[_] = if editable || d.local_dbus {
                    &["VNC", "SPICE", "D-Bus"]
                } else {
                    &["VNC", "SPICE"]
                };
                self.segmented_with(
                    "viewer",
                    options,
                    selected,
                    editable || (!self.busy() && !d.local_dbus && !d.accel3d),
                    move |this, i, cx| {
                        if editable {
                            this.edit_hardware(
                                |d| {
                                    d.local_dbus = i == 2;
                                    if i == 0 {
                                        d.accel3d = false;
                                    }
                                },
                                cx,
                            );
                        }
                        if i != 2 {
                            this.set_spice_viewer(id.clone(), i == 1, cx);
                        }
                    },
                    cx,
                )
            }))
            .child(setting("Displays", self.heads_toggle(d.heads, editable, cx)))
            .child(setting(
                accel_label,
                v_flex()
                    .gap_1()
                    .child(self.switch(
                        "accel3d",
                        if windows {
                            "DirectX acceleration"
                        } else {
                            accel_label
                        },
                        d.accel3d,
                        editable
                            && (d.accel3d
                                || (d.graphics == "virtio"
                                    && if windows {
                                        triton_arch && self.details.neptune_ready
                                    } else {
                                        machine.display != "D-Bus" || original.local_dbus
                                    })),
                        |d, on| d.accel3d = on,
                        cx,
                    ))
                    .when(
                        windows
                            && !d.accel3d
                            && (!triton_arch
                                || !self.details.neptune_ready
                                || d.graphics != "virtio"),
                        |row| {
                            let hint = if !triton_arch {
                                crate::backend::TRITON_ARCH_UNSUPPORTED
                            } else if !self.details.neptune_ready {
                                crate::backend::NEPTUNE_HOST_MISSING
                            } else {
                                "Choose VirtIO graphics to enable DirectX."
                            };
                            row.child(caption(hint, p))
                        },
                    )
                    .into_any_element(),
            ))
            .child(setting(
                "Sound",
                self.choice_menu(
                    "sound",
                    workflows::SOUND,
                    &d.sound,
                    editable,
                    |d, v| d.sound = v,
                    cx,
                ),
            ))
            .child(setting(
                "Clipboard",
                self.switch(
                    "clipboard",
                    "Clipboard",
                    d.clipboard,
                    editable,
                    |d, on| d.clipboard = on,
                    cx,
                ),
            ));

        crate::ui::primitives::Sections(Vec::new())
            .add("System", "cpu", system)
            .add("Boot order", "list-ordered", self.boot_order(&d, editable, p, cx))
            .add("Storage", "hard-drive", self.storage(machine, p, cx))
            .add("Display and sound", "monitor", display)
            .add("Network", "network", self.network(machine, &d, editable, p, cx))
            .add(
                "USB and input",
                "usb",
                section(
                    "USB and input",
                    Some(self.add_device_menu(
                        "add-usb",
                        machine,
                        &[DeviceAction::Usb, DeviceAction::Serial],
                        &[],
                        cx,
                    )),
                )
                .map(|s| {
                    let rows = self.device_rows(machine, &["usb"], cx);
                    if rows.is_empty() {
                        s
                    } else {
                        s.child(crate::ui::primitives::item_list(rows, p))
                    }
                })
                .child(setting(
                    "USB controller",
                    self.choice_menu(
                        "usb",
                        workflows::USB_CONTROLLERS,
                        &d.platform.usb,
                        editable,
                        |d, v| {
                            if v == "none" {
                                d.platform.tablet = false;
                            }
                            d.platform.usb = v
                        },
                        cx,
                    ),
                ))
                .child(setting(
                    "USB tablet",
                    self.switch(
                        "tablet",
                        "USB tablet",
                        d.platform.tablet,
                        editable && d.platform.usb != "none",
                        |d, on| d.platform.tablet = on,
                        cx,
                    ),
                )),
            )
            .add("Shared folders", "folder", self.sharing(machine, editable, p, cx))
            .add(
                "Guest tools",
                "package",
                self.guest_tools(machine, &original, &d, windows && triton_arch, editable, p, cx),
            )
            .add(
                "Advanced",
                "settings-2",
                section(
                    "Advanced",
                    Some(
                        Button::new("edit-xml")
                            .ghost()
                            .small()
                            .label("Edit XML…")
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(m) = this.selected_machine().cloned() {
                                    let xml = this.details.xml.clone();
                                    this.open_form(workflows::xml(&m, xml), window, cx);
                                }
                            }))
                            .into_any_element(),
                    ),
                )
                .child(setting(
                    "Reclaim memory",
                    self.switch(
                        "balloon",
                        "Reclaim unused guest memory",
                        d.platform.balloon,
                        editable,
                        |d, on| d.platform.balloon = on,
                        cx,
                    ),
                ))
                .child(setting(
                    "Hardware RNG",
                    self.switch(
                        "rng",
                        "Provide hardware randomness",
                        d.platform.rng,
                        editable,
                        |d, on| d.platform.rng = on,
                        cx,
                    ),
                ))
                .child(setting(
                    "Hyper-V tuning",
                    self.switch(
                        "hyperv",
                        "Optimize Windows virtualization",
                        d.platform.hyperv,
                        editable,
                        |d, on| d.platform.hyperv = on,
                        cx,
                    ),
                ))
                .child(setting(
                    "Guest clock",
                    self.segmented(
                        "localtime",
                        &["UTC", "Local time"],
                        usize::from(d.platform.localtime),
                        editable,
                        |d, i| d.platform.localtime = i == 1,
                        cx,
                    ),
                )),
            )
            .layout(page, self.wide, self.hardware_section, cx)
    }

    /// Network cards: model, MAC address, isolation.
    /// Disks and optical drives, with their own add menu.
    fn storage(&self, machine: &Machine, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let add = self.add_device_menu(
            "add-storage",
            machine,
            &[DeviceAction::NewDisk, DeviceAction::AttachDisk, DeviceAction::Cdrom],
            &[],
            cx,
        );
        let rows = self.device_rows(machine, &["disk", "cdrom"], cx);
        let empty = rows.is_empty();
        section("Storage", Some(add))
            .when(!empty, |s| s.child(crate::ui::primitives::item_list(rows, p)))
            .when(empty, |s| s.child(notice("No drives", p)))
    }

    /// Architecture as a menu of what libvirt can run here. Choosing one confirms first,
    /// because the machine type, firmware and controllers are reset with it.
    pub(crate) fn architecture_control(
        &self,
        machine: &Machine,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = machine.arch.clone();
        let mut options = self.architectures.clone();
        if !options.contains(&current) {
            options.insert(0, current.clone());
        }
        // Same anatomy as Operating system: the value, then Change… when it can change.
        let value = div().min_w_0().child(machine.architecture().to_owned());
        if !machine.can_edit() || self.busy() || options.len() < 2 {
            return value.into_any_element();
        }
        let weak = cx.entity().downgrade();
        let m = machine.clone();
        h_flex()
            .gap_3()
            .child(value)
            .child(Button::new("architecture").ghost().small().label("Change…").dropdown_menu(
                move |mut menu, _, _| {
                    menu = menu.min_w(px(crate::ui::primitives::MENU_MIN_WIDTH));
                    let host = std::env::consts::ARCH;
                    for arch in &options {
                        let weak = weak.clone();
                        let m = m.clone();
                        let value = arch.clone();
                        let text = label(workflows::ARCHITECTURES, arch).to_owned();
                        let native = arch == host || (host == "x86_64" && arch == "i686");
                        let item = PopupMenuItem::element(move |_, cx| {
                            h_flex().w_full().gap_4().justify_between().child(text.clone()).child(
                                div()
                                    .text_size(px(CAPTION_SIZE))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if native {
                                        "Virtualized"
                                    } else {
                                        "Emulated"
                                    }),
                            )
                        })
                        .checked(*arch == m.arch)
                        .on_click(move |_, window, cx| {
                            if value == m.arch {
                                return;
                            }
                            let _ = weak.update(cx, |this, cx| {
                                this.open_form(workflows::architecture(&m, &value), window, cx)
                            });
                        });
                        menu = menu.item(item);
                    }
                    menu
                },
            ))
            .into_any_element()
    }

    /// Shared folders, and how to reach them from inside the guest. The guest family
    /// defaults to the detected system and can be switched, since detection can be wrong.
    /// Why a page cannot change the machine yet, with the action that gets it there.
    /// `goal` names what is blocked: "edit" on Hardware, "restore" on Snapshots.
    pub(crate) fn locked_notice(
        &self,
        machine: &Machine,
        goal: &'static str,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = matches!(machine.state, State::Running | State::Paused);
        let suspended = machine.saved_state && !running;
        let action = if running {
            Some(
                Button::new("locked-shutdown")
                    .outline()
                    .small()
                    .label("Shut down")
                    .disabled(self.busy())
                    .on_click(
                        cx.listener(|this, _, _, cx| this.machine_action(Action::Shutdown, cx)),
                    )
                    .into_any_element(),
            )
        } else if suspended {
            let m = machine.clone();
            Some(
                Button::new("locked-discard")
                    .outline()
                    .small()
                    .label("Discard…")
                    .disabled(self.busy())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(form) = workflows::action_form(
                            workflows::MachineAction::DiscardState,
                            &m,
                            this.preferences.connection,
                        ) {
                            this.open_form(form, window, cx);
                        }
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };
        h_flex()
            .gap_3()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(p.border)
            .child(Icon::new(Glyph::Lock).size_4().flex_shrink_0().text_color(p.muted))
            .child(div().flex_1().min_w_0().child(if suspended {
                format!("Discard the suspended state to {goal}")
            } else {
                format!("Shut down to {goal}")
            }))
            .children(action)
            .into_any_element()
    }
}

fn vcpus(n: u32) -> String {
    if n == 1 {
        "1 vCPU".into()
    } else {
        format!("{n} vCPUs")
    }
}
