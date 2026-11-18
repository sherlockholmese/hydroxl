use super::primitives::{caption, ledger, section};
use crate::app::{Job, Workspace};
use crate::backend::{Action, Machine, State};
use crate::operations::Operation;
use crate::theme::{BODY_SIZE, CAPTION_SIZE, LABEL_WIDTH, Palette};
use crate::workflows::{self, DeviceAction, HardwareDraft, label};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    /// Saved hardware and the values being edited (the draft, or the saved values).
    pub(crate) fn hardware_values(&self) -> Option<(HardwareDraft, HardwareDraft)> {
        let machine = self.selected_machine()?;
        let original = HardwareDraft::read(machine, &self.details.xml)?;
        let current = self
            .hardware_draft
            .clone()
            .filter(|d| d.id == original.id)
            .unwrap_or_else(|| original.clone());
        Some((original, current))
    }

    /// Typed memory in MB, applied to the draft as the field changes.
    pub(crate) fn memory_typed(&mut self, cx: &mut Context<Self>) {
        let typed = self.memory_input.read(cx).value().trim().parse::<u64>();
        if let Ok(mib) = typed {
            if self.hardware_values().is_some_and(|(_, d)| d.memory_mib != mib) {
                self.edit_hardware(|d| d.memory_mib = mib, cx);
            }
        }
    }

    /// Keeps the memory field showing the draft when it changes elsewhere.
    pub(crate) fn sync_memory_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, d)) = self.hardware_values() else {
            return;
        };
        let input = self.memory_input.clone();
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let shown = input.read(cx).value().trim().parse::<u64>().ok();
        if !focused && shown != Some(d.memory_mib) {
            input.update(cx, |s, cx| s.set_value(d.memory_mib.to_string(), window, cx));
        }
    }

    pub(crate) fn hardware_dirty(&self) -> bool {
        self.hardware_values().is_some_and(|(original, current)| original != current)
    }

    fn edit_hardware(&mut self, edit: impl FnOnce(&mut HardwareDraft), cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        if let Some((_, mut current)) = self.hardware_values() {
            edit(&mut current);
            self.hardware_draft = Some(current);
            self.saved_hardware_for = None;
            cx.notify();
        }
    }

    pub(crate) fn discard_hardware(&mut self, cx: &mut Context<Self>) {
        self.hardware_draft = None;
        self.saved_hardware_for = None;
        cx.notify();
    }

    pub(crate) fn save_hardware(&mut self, cx: &mut Context<Self>) {
        let Some((original, current)) = self.hardware_values() else {
            return;
        };
        let Some(machine) = self.selected_machine().cloned() else {
            return;
        };
        let ops = current.changes(&original);
        if ops.is_empty() {
            return;
        }
        self.begin(Job::Operation(machine, Operation::Sequence(ops)), None, cx);
    }

    /// Settings grouped the way UTM groups them, edited in place.
    pub(crate) fn hardware(&self, machine: &Machine, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::get(cx);
        let editable = machine.can_edit() && !self.busy();
        let values = self.hardware_values();
        let mut page = v_flex().gap_6();
        if !machine.can_edit() {
            page = page.child(self.locked_notice(machine, p, cx));
        }
        let Some((original, d)) = values else {
            return page
                .child(
                    section("System", None, p)
                        .border_t_0()
                        .pt_0()
                        .child(ledger("Architecture", machine.architecture().to_owned(), p))
                        .child(ledger("CPU", vcpus(machine.cpus), p))
                        .child(ledger("Memory", machine.memory(), p)),
                )
                .child(caption(
                    if self.details_for != self.selected && self.library_error.is_none() {
                        "Loading"
                    } else {
                        "Configuration unavailable"
                    },
                    p,
                ))
                .into_any_element();
        };
        let one_socket = crate::backend::Topology::one_socket(d.topology.vcpus());
        let machine_types = workflows::machine_types(&machine.arch);
        let current_type = workflows::machine_type_family(&d.machine_type);
        let setting = |label: &'static str, control: AnyElement| {
            h_flex()
                .gap_4()
                .w_full()
                .min_h(px(40.))
                .text_size(px(BODY_SIZE))
                .child(div().w(px(LABEL_WIDTH)).flex_shrink_0().text_color(p.muted).child(label))
                .child(div().flex_1().min_w_0().child(control))
        };

        let advanced_row = |label: &'static str, control: AnyElement| {
            h_flex()
                .justify_between()
                .items_center()
                .gap_4()
                .min_h(px(40.))
                .child(div().flex_1().min_w_0().text_size(px(BODY_SIZE)).child(label))
                .child(control)
        };

        let system = section("System", None, p)
            .when(machine.can_edit(), |s| s.border_t_0().pt_0())
            .child(setting("Architecture", self.architecture_control(machine, cx)))
            .child(
                h_flex()
                    .gap_4()
                    .w_full()
                    .min_h(px(48.))
                    .child(
                        div().w(px(LABEL_WIDTH)).flex_shrink_0().text_color(p.muted).child("CPU"),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(self.stepper(
                                "Sockets",
                                d.topology.sockets,
                                1..=8,
                                editable,
                                |d, v| d.topology.sockets = v,
                                cx,
                            ))
                            .child(self.stepper(
                                "Cores",
                                d.topology.cores,
                                1..=128,
                                editable,
                                |d, v| d.topology.cores = v,
                                cx,
                            ))
                            .child(self.stepper(
                                "Threads",
                                d.topology.threads,
                                1..=4,
                                editable,
                                |d, v| d.topology.threads = v,
                                cx,
                            ))
                            .child(
                                v_flex().gap_1().child(caption("Total", p)).child(
                                    div()
                                        .h(px(32.))
                                        .flex()
                                        .items_center()
                                        .child(vcpus(d.topology.vcpus())),
                                ),
                            )
                            .into_any_element(),
                    ),
            )
            .when(original.implicit_topology && original.topology.sockets > 1, |system| {
                system.child(setting(
                    "",
                    Button::new("cpu-single-socket")
                        .outline()
                        .small()
                        .label("Use 1 socket")
                        .tooltip("Keep the total vCPU count")
                        .disabled(!editable || d.topology == one_socket || one_socket.cores > 256)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_hardware(
                                |d| {
                                    d.topology =
                                        crate::backend::Topology::one_socket(d.topology.vcpus());
                                    d.implicit_topology = false;
                                },
                                cx,
                            );
                        }))
                        .into_any_element(),
                ))
            })
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
        let display = section("Display and sound", None, p)
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
            .child(setting("Viewer", {
                if machine.display == "D-Bus" {
                    return div().child("D-Bus").into_any_element();
                }
                let id = machine.id.clone();
                let viewer = self.segmented_with(
                    "viewer",
                    &["VNC", "SPICE"],
                    usize::from(machine.spice_viewer || d.accel3d),
                    !self.busy() && (!d.accel3d || editable),
                    move |this, i, cx| {
                        if i == 0 && this.hardware_values().is_some_and(|(_, d)| d.accel3d) {
                            if !this.selected_machine().is_some_and(Machine::can_edit) {
                                return;
                            }
                            this.edit_hardware(|d| d.accel3d = false, cx);
                        }
                        this.set_spice_viewer(id.clone(), i == 1, cx);
                    },
                    cx,
                );
                viewer
            }))
            .child(setting("Displays", self.heads_toggle(d.heads, editable, cx)))
            .child(setting(
                "3D acceleration",
                v_flex()
                    .gap_1()
                    .child(self.switch(
                        "accel3d",
                        "3D acceleration",
                        d.accel3d,
                        // Windows has no VirtIO GPU 3D driver: with GL on it draws nothing.
                        // An existing setting can still be turned off.
                        editable
                            && machine.display != "D-Bus"
                            && (d.accel3d
                                || (d.graphics == "virtio"
                                    && crate::operations::GuestOs::of(machine)
                                        != crate::operations::GuestOs::Windows)),
                        |d, on| d.accel3d = on,
                        cx,
                    ))
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
                    "clipboard",
                    d.clipboard,
                    editable,
                    |d, on| d.clipboard = on,
                    cx,
                ),
            ));

        super::primitives::Sections(Vec::new())
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
                    p,
                )
                .map(|s| {
                    let rows = self.device_rows(machine, &["usb"], cx);
                    if rows.is_empty() {
                        s
                    } else {
                        s.child(super::primitives::item_list(rows, p))
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
                section(
                    "Guest tools",
                    Some(
                        Button::new("install-guest-tools")
                            .outline()
                            .small()
                            .label("Install")
                            .disabled(self.busy() || machine.state == State::Other)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(m) = this.selected_machine().cloned() {
                                    this.open_form(workflows::guest_tools(&m, None), window, cx);
                                }
                            }))
                            .into_any_element(),
                    ),
                    p,
                )
                .child(setting(
                    "Guest agent",
                    if crate::operations::has_agent_channel(&self.details.xml) {
                        div().child("Configured").into_any_element()
                    } else {
                        Button::new("add-agent")
                            .outline()
                            .small()
                            .label("Add channel")
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(m) = this.selected_machine().cloned() {
                                    this.begin(
                                        Job::Operation(m, Operation::AddAgentChannel),
                                        None,
                                        cx,
                                    );
                                }
                            }))
                            .into_any_element()
                    },
                )),
            )
            .add(
                "Advanced",
                "settings-2",
                section(
                    "Advanced",
                    Some(
                        Button::new("edit-xml")
                            .outline()
                            .small()
                            .label("Edit XML")
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(m) = this.selected_machine().cloned() {
                                    let xml = this.details.xml.clone();
                                    this.open_form(workflows::xml(&m, xml), window, cx);
                                }
                            }))
                            .into_any_element(),
                    ),
                    p,
                )
                .child(
                    v_flex()
                        .max_w(px(480.))
                        .w_full()
                        .gap_1()
                        .child(advanced_row(
                            "Reclaim unused guest memory",
                            self.switch(
                                "balloon",
                                "Reclaim unused guest memory",
                                d.platform.balloon,
                                editable,
                                |d, on| d.platform.balloon = on,
                                cx,
                            ),
                        ))
                        .child(advanced_row(
                            "Provide hardware randomness",
                            self.switch(
                                "rng",
                                "Provide hardware randomness",
                                d.platform.rng,
                                editable,
                                |d, on| d.platform.rng = on,
                                cx,
                            ),
                        ))
                        .child(advanced_row(
                            "Optimize Windows virtualization",
                            self.switch(
                                "hyperv",
                                "Optimize Windows virtualization",
                                d.platform.hyperv,
                                editable,
                                |d, on| d.platform.hyperv = on,
                                cx,
                            ),
                        ))
                        .child(advanced_row(
                            "Use local time for the guest clock",
                            self.switch(
                                "localtime",
                                "Use local time for the guest clock",
                                d.platform.localtime,
                                editable,
                                |d, on| d.platform.localtime = on,
                                cx,
                            ),
                        )),
                ),
            )
            .layout(page, self.wide, self.hardware_section, self.hardware_section_from, cx)
    }

    /// Network cards: model, MAC address, isolation.
    fn network(
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
        let mut group = section("Network", Some(new), p);
        if d.nics.is_empty() {
            return group.child(caption("No network cards", p)).when(!forwards.is_empty(), |g| {
                g.child(super::primitives::item_list(forwards, p))
            });
        }
        for (i, nic) in d.nics.iter().enumerate() {
            let setting = |label: &'static str, control: AnyElement| {
                h_flex()
                    .gap_4()
                    .w_full()
                    .min_h(px(40.))
                    .child(
                        div().w(px(LABEL_WIDTH)).flex_shrink_0().text_color(p.muted).child(label),
                    )
                    .child(div().flex_1().min_w_0().child(control))
            };
            let card_xml = self
                .details
                .devices
                .iter()
                .find(|dev| dev.kind == "network" && dev.target == nic.mac)
                .map(|dev| dev.xml.clone());
            // The card itself reads like a Storage row: what it is, its address, and a
            // menu for actions on the card. Its settings follow, indented to the label column.
            let mode = match nic.kind.as_str() {
                "network" => "Virtual network",
                "bridge" => "Bridged network",
                _ => "Shared network",
            };
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
                            .icon(super::primitives::glyph("refresh-cw"))
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
                    let menu = menu.min_w(px(super::primitives::MENU_MIN_WIDTH)).item(renew);
                    match card_xml.clone() {
                        Some(xml) => {
                            let weak = weak.clone();
                            menu.separator().item(
                                PopupMenuItem::new("Remove card…")
                                    .icon(super::primitives::glyph("trash"))
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
                            v_flex().flex_1().min_w_0().child(mode).child(
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
            group = group.child(super::primitives::item_list(vec![card.into_any_element()], p));
        }
        group.when(!forwards.is_empty(), |g| g.child(super::primitives::item_list(forwards, p)))
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
            return caption("None available", p).into_any_element();
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

    /// Memory in MB: type an exact value, or step by 1 GB.
    fn memory_control(&self, mib: u64, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::get(cx);
        h_flex()
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
                            .accessibility_label("1 GB less memory")
                            .disabled(!enabled || mib <= 1024)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_hardware(
                                    |d| d.memory_mib = ((d.memory_mib - 1) / 1024 * 1024).max(1024),
                                    cx,
                                )
                            })),
                    )
                    .child(
                        div().w(px(72.)).h(px(30.)).flex().items_center().child(
                            Input::new(&self.memory_input)
                                .appearance(false)
                                .small()
                                .aria_label("Memory in MB")
                                .disabled(!enabled),
                        ),
                    )
                    .child(
                        Button::new("memory-up")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Glyph::Plus))
                            .accessibility_label("1 GB more memory")
                            .disabled(!enabled || mib + 1024 > 1024 * 1024)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_hardware(
                                    |d| d.memory_mib = (d.memory_mib / 1024 + 1) * 1024,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(div().text_color(p.muted).child("MB"))
            .into_any_element()
    }

    /// Joined number stepper, the same height and border as an input, with its unit after.
    fn stepper(
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
                    .child(div().w(px(32.)).text_center().child(value.to_string()))
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

    fn choice_menu(
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

    fn switch(
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
    fn segmented(
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
    fn segmented_with(
        &self,
        id: impl Into<SharedString>,
        options: &[&'static str],
        selected: usize,
        enabled: bool,
        on_pick: impl Fn(&mut Self, usize, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        super::primitives::segmented(
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

    fn firmware_toggle(&self, uefi: bool, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        self.segmented(
            "firmware",
            &["BIOS", "UEFI"],
            uefi as usize,
            enabled,
            |d, i| d.uefi = i == 1,
            cx,
        )
    }

    fn heads_toggle(&self, heads: u32, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        self.segmented(
            "heads",
            &["1", "2", "3", "4"],
            heads.clamp(1, 4) as usize - 1,
            enabled,
            |d, i| d.heads = i as u32 + 1,
            cx,
        )
    }

    /// Bootable devices in order. Switches choose which are tried; arrows reorder.
    fn boot_order(
        &self,
        d: &HardwareDraft,
        enabled: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = d.boot.len();
        let list = section("Boot order", None, p);
        if count == 0 {
            return list.child(caption("No bootable devices", p));
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
                    .min_h(px(52.))
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
        list.child(super::primitives::item_list(rows, p))
    }

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
        section("Storage", Some(add), p)
            .when(!empty, |s| s.child(super::primitives::item_list(rows, p)))
            .when(empty, |s| s.child(caption("No drives", p)))
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
        if !machine.can_edit() || self.busy() || options.len() < 2 {
            return div().child(machine.architecture().to_owned()).into_any_element();
        }
        let weak = cx.entity().downgrade();
        let m = machine.clone();
        h_flex()
            .child(
                Button::new("architecture")
                    .outline()
                    .small()
                    .label(machine.architecture().to_owned())
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.min_w(px(super::primitives::MENU_MIN_WIDTH));
                        let host = std::env::consts::ARCH;
                        for arch in &options {
                            let weak = weak.clone();
                            let m = m.clone();
                            let value = arch.clone();
                            let text = label(workflows::ARCHITECTURES, arch).to_owned();
                            let native = arch == host || (host == "x86_64" && arch == "i686");
                            let item = PopupMenuItem::element(move |_, cx| {
                                h_flex()
                                    .w_full()
                                    .gap_4()
                                    .justify_between()
                                    .child(text.clone())
                                    .child(
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
                    }),
            )
            .into_any_element()
    }

    /// Shared folders, and how to reach them from inside the guest. The guest family
    /// defaults to the detected system and can be switched, since detection can be wrong.
    fn sharing(
        &self,
        machine: &Machine,
        editable: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::operations::{GuestOs, GuestSetup, ShareDriver};
        let m = machine.clone();
        let mut group = section(
            "Shared folders",
            Some(
                Button::new("add-share")
                    .ghost()
                    .small()
                    .label("Add")
                    .icon(Icon::new(Glyph::Plus))
                    .disabled(!editable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(form) =
                            workflows::device_form(workflows::DeviceAction::Share, &m)
                        {
                            this.open_form(form, window, cx);
                        }
                    }))
                    .into_any_element(),
            ),
            p,
        );
        let shares: Vec<_> = self.details.devices.iter().filter(|d| d.kind == "share").collect();
        if shares.is_empty() {
            return group.child(caption("No shared folders", p));
        }
        let os = self.share_os.unwrap_or_else(|| GuestOs::of(machine));
        let mono = cx.theme().mono_font_family.clone();
        let mut share_rows = Vec::new();
        for (i, share) in shares.iter().enumerate() {
            let driver = ShareDriver::from_xml(&share.xml);
            let unreadable = os == GuestOs::Windows && driver == ShareDriver::NineP;
            let xml = share.xml.clone();
            share_rows.push(
                h_flex()
                    .gap_3()
                    .min_h(px(48.))
                    .child(Icon::new(Glyph::Folder).size_4().flex_shrink_0().text_color(p.muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(share.source.clone()))
                            .child(
                                caption(format!("{}  {}", share.target, driver.label()), p)
                                    .when(unreadable, |c| c.text_color(p.attention)),
                            ),
                    )
                    .child({
                        let weak = cx.entity().downgrade();
                        let busy = self.busy();
                        let device = (*share).clone();
                        let running = machine.state == State::Running;
                        let tag = share.target.clone();
                        Button::new(("share-actions", i))
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::Ellipsis))
                            .accessibility_label(format!("{} actions", share.source))
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                let edit = {
                                    let weak = weak.clone();
                                    let device = device.clone();
                                    PopupMenuItem::new("Edit…")
                                        .icon(super::primitives::glyph("pencil"))
                                        .disabled(busy || !editable)
                                        .on_click(move |_, window, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                if let Some(m) = this.selected_machine().cloned() {
                                                    this.open_form(
                                                        workflows::edit_share(&m, &device),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            });
                                        })
                                };
                                let remove = {
                                    let weak = weak.clone();
                                    let xml = xml.clone();
                                    PopupMenuItem::new("Stop sharing…")
                                        .icon(super::primitives::glyph("trash"))
                                        .disabled(busy || !editable)
                                        .on_click(move |_, window, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                if let Some(m) = this.selected_machine().cloned() {
                                                    this.open_form(
                                                        workflows::confirm(
                                                            "Stop sharing this folder",
                                                            "Stop sharing",
                                                            "The folder stays on this computer.",
                                                            m,
                                                            Operation::DetachDevice(xml.clone()),
                                                        ),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            });
                                        })
                                };
                                let unmount = {
                                    let weak = weak.clone();
                                    let share = (tag.clone(), driver);
                                    PopupMenuItem::new("Unmount in guest")
                                        .icon(super::primitives::glyph("folder-x"))
                                        .disabled(busy || !running)
                                        .on_click(move |_, _, cx| {
                                            let share = share.clone();
                                            let _ = weak.update(cx, |this, cx| {
                                                if let Some(m) = this.selected_machine().cloned() {
                                                    let op =
                                                        Operation::Guest(GuestSetup::Unmount {
                                                            os,
                                                            shares: vec![share],
                                                        });
                                                    this.begin(Job::Operation(m, op), None, cx);
                                                }
                                            });
                                        })
                                };
                                let copy = {
                                    let command = match os {
                                        GuestOs::Linux => {
                                            crate::operations::linux_unmount_command(&tag, true)
                                        }
                                        GuestOs::Windows => {
                                            crate::operations::WINDOWS_UNMOUNT_COMMAND.to_owned()
                                        }
                                    };
                                    PopupMenuItem::new("Copy unmount command")
                                        .icon(super::primitives::glyph("copy"))
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                command.clone(),
                                            ))
                                        })
                                };
                                menu.min_w(px(super::primitives::MENU_MIN_WIDTH))
                                    .item(edit)
                                    .item(unmount)
                                    .item(copy)
                                    .separator()
                                    .item(remove)
                            })
                    })
                    .into_any_element(),
            );
        }
        group = group.child(super::primitives::item_list(share_rows, p));

        // In the guest: one command per system, copied or typed straight into the guest.
        let running = machine.state == State::Running;
        let commands: Vec<(String, String)> = match os {
            GuestOs::Linux => shares
                .iter()
                .map(|share| {
                    let driver = ShareDriver::from_xml(&share.xml);
                    (
                        crate::operations::linux_mount_point(&share.target),
                        crate::operations::linux_command(&share.target, driver, true),
                    )
                })
                .collect(),
            GuestOs::Windows => {
                vec![("Administrator prompt".into(), crate::operations::WINDOWS_COMMAND.into())]
            }
        };
        let id = machine.id.clone();
        let picker = self.segmented_with(
            "share-os",
            &["Linux", "Windows"],
            (os == GuestOs::Windows) as usize,
            true,
            move |this, i, cx| {
                if this.selected.as_deref() == Some(id.as_str()) {
                    this.share_os = Some(if i == 1 {
                        GuestOs::Windows
                    } else {
                        GuestOs::Linux
                    });
                    cx.notify();
                }
            },
            cx,
        );
        let setup_shares: Vec<(String, ShareDriver)> = shares
            .iter()
            .map(|share| (share.target.clone(), ShareDriver::from_xml(&share.xml)))
            .collect();
        // Same anatomy as the folders above: a heading with its control, then one inset
        // group of rows. Unmounting lives in each folder's menu.
        let row = |label: SharedString, control: AnyElement| {
            h_flex()
                .gap_3()
                .min_h(px(48.))
                .child(div().flex_1().min_w_0().child(label))
                .child(control)
                .into_any_element()
        };
        let mut rows = Vec::new();
        if os == GuestOs::Windows {
            rows.push(row(
                "Guest tools".into(),
                Button::new("share-guest-tools")
                    .outline()
                    .small()
                    .label("Install")
                    .disabled(self.busy() || machine.state == State::Other)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(m) = this.selected_machine().cloned() {
                            this.open_form(workflows::guest_tools(&m, None), window, cx);
                        }
                    }))
                    .into_any_element(),
            ));
        }
        rows.push(row(
            "Mount automatically".into(),
            Button::new("setup-shares")
                .outline()
                .small()
                .label("Run")
                .disabled(!running || self.busy())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(m) = this.selected_machine().cloned() {
                        let op = Operation::Guest(crate::operations::GuestSetup::Shares {
                            os,
                            shares: setup_shares.clone(),
                        });
                        this.begin(Job::Operation(m, op), None, cx);
                    }
                }))
                .into_any_element(),
        ));
        for (i, (title, command)) in commands.into_iter().enumerate() {
            let typed = command.clone();
            rows.push(
                v_flex()
                    .py_3()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().min_w_0().truncate().child(title))
                            .child(
                                Button::new(("type-mount", i))
                                    .ghost()
                                    .xsmall()
                                    .icon(super::primitives::glyph("keyboard"))
                                    .accessibility_label("Type in guest")
                                    .tooltip("Type in guest")
                                    .disabled(!running || self.busy())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(m) = this.selected_machine().cloned() {
                                            let op = Operation::Guest(
                                                crate::operations::GuestSetup::Type(typed.clone()),
                                            );
                                            this.begin(Job::Operation(m, op), None, cx);
                                        }
                                    })),
                            )
                            .child(
                                Clipboard::new(("copy-mount", i))
                                    .value(command.clone())
                                    .accessibility_label("Copy command")
                                    .tooltip("Copy"),
                            ),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(CAPTION_SIZE))
                            .line_height(relative(1.6))
                            .text_color(p.muted)
                            .child(command),
                    )
                    .into_any_element(),
            );
        }
        group
            .child(div().h(px(16.)))
            .child(section("In the guest", Some(picker), p))
            .child(super::primitives::item_list(rows, p))
    }

    fn locked_notice(&self, machine: &Machine, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let running = matches!(machine.state, State::Running | State::Paused);
        h_flex()
            .gap_3()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(p.border)
            .child(Icon::new(Glyph::Lock).size_4().flex_shrink_0().text_color(p.muted))
            .child(div().flex_1().min_w_0().child(if machine.saved_state && !running {
                "Discard the saved state to edit"
            } else {
                "Shut down to edit"
            }))
            .when(running, |this| {
                this.child(
                    Button::new("locked-shutdown")
                        .outline()
                        .small()
                        .label("Shut down")
                        .disabled(self.busy())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.machine_action(Action::Shutdown, cx)),
                        ),
                )
            })
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
