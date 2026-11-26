use crate::app::{Job, Workspace};
use crate::backend::{Machine, State};
use crate::operations::Operation;
use crate::theme::{BODY_SIZE, CAPTION_SIZE, Palette};
use crate::ui::primitives::{caption, notice, section, setting as setting_row};
use crate::workflows::{self, HardwareDraft};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};

impl Workspace {
    pub(super) fn sharing(
        &self,
        machine: &Machine,
        editable: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::operations::{GuestOs, ShareDriver};
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
                        if let Some(form) = workflows::device_form(
                            workflows::DeviceAction::Share,
                            &m,
                            this.preferences.connection,
                        ) {
                            this.open_form(form, window, cx);
                        }
                    }))
                    .into_any_element(),
            ),
        );
        let shares: Vec<_> = self.details.devices.iter().filter(|d| d.kind == "share").collect();
        if shares.is_empty() {
            return group.child(notice("No shared folders", p));
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
                                // The warning is worded, not only colored.
                                caption(
                                    if unreadable {
                                        format!(
                                            "{}, {}, not readable by Windows",
                                            share.target,
                                            driver.label()
                                        )
                                    } else {
                                        format!("{}, {}", share.target, driver.label())
                                    },
                                    p,
                                )
                                .when(unreadable, |c| c.text_color(p.attention)),
                            ),
                    )
                    .child({
                        let weak = cx.entity().downgrade();
                        let busy = self.busy();
                        let device = (*share).clone();
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
                                        .icon(crate::ui::primitives::glyph("pencil"))
                                        .disabled(busy || !editable)
                                        .on_click(move |_, window, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                if let Some(m) = this.selected_machine().cloned() {
                                                    this.open_form(
                                                        workflows::edit_share(
                                                            &m,
                                                            &device,
                                                            this.preferences.connection,
                                                        ),
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
                                        .icon(crate::ui::primitives::glyph("trash"))
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
                                menu.min_w(px(crate::ui::primitives::MENU_MIN_WIDTH))
                                    .item(edit)
                                    .separator()
                                    .item(remove)
                            })
                    })
                    .into_any_element(),
            );
        }
        group = group.child(crate::ui::primitives::item_list(share_rows, p));

        // In the guest: the command that mounts the folders, beside the button that runs it.
        let running = machine.state == State::Running;
        // Linux mounts each folder with its own command, so each is labelled with its
        // folder. Windows maps every folder with one command.
        let commands: Vec<(Option<String>, String)> = match os {
            GuestOs::Linux => shares
                .iter()
                .map(|share| {
                    let driver = ShareDriver::from_xml(&share.xml);
                    (
                        Some(share.target.clone()),
                        crate::operations::linux_command(&share.target, driver, true),
                    )
                })
                .collect(),
            GuestOs::Windows => vec![(None, crate::operations::WINDOWS_COMMAND.into())],
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
                    .label("Mount…")
                    .disabled(self.busy() || machine.state == State::Other)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(m) = this.selected_machine().cloned() {
                            this.open_form(workflows::guest_tools(&m, None), window, cx);
                        }
                    }))
                    .into_any_element(),
            ));
        }
        // One row: what runs (each command, copyable where it reads) and the button that
        // runs it. Mounting is one task, so it is one row, not a setting plus a recipe.
        let mut lines = v_flex().flex_1().min_w_0().gap_4();
        for (i, (folder, command)) in commands.into_iter().enumerate() {
            lines = lines.child(
                v_flex()
                    .gap_1()
                    .when_some(folder, |block, folder| {
                        block.child(
                            div()
                                .text_size(px(BODY_SIZE))
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(folder),
                        )
                    })
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(mono.clone())
                                    .text_size(px(CAPTION_SIZE))
                                    .line_height(relative(1.6))
                                    .text_color(p.muted)
                                    .child(command.clone()),
                            )
                            .child(
                                Clipboard::new(("copy-mount", i))
                                    .value(command)
                                    .accessibility_label("Copy command")
                                    .tooltip("Copy"),
                            ),
                    ),
            );
        }
        rows.push(
            h_flex()
                .items_start()
                .gap_6()
                .py_3()
                .child(lines)
                .child({
                    // Opposite actions on the same folders, side by side; the one that adds
                    // is the stronger of the two.
                    let unmount_shares = setup_shares.clone();
                    h_flex()
                        .gap_2()
                        .flex_shrink_0()
                        .child(
                            Button::new("unmount-shares")
                                .ghost()
                                .small()
                                .label("Unmount")
                                .disabled(!running || self.busy())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(m) = this.selected_machine().cloned() {
                                        let op = Operation::Guest(
                                            crate::operations::GuestSetup::Unmount {
                                                os,
                                                shares: unmount_shares.clone(),
                                            },
                                        );
                                        this.begin(Job::Operation(m, op), None, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("setup-shares")
                                .outline()
                                .small()
                                .label("Mount")
                                .disabled(!running || self.busy())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(m) = this.selected_machine().cloned() {
                                        let op = Operation::Guest(
                                            crate::operations::GuestSetup::Shares {
                                                os,
                                                shares: setup_shares.clone(),
                                            },
                                        );
                                        this.begin(Job::Operation(m, op), None, cx);
                                    }
                                })),
                        )
                })
                .into_any_element(),
        );
        group
            .child(div().h_4())
            .child(section("In the guest", Some(picker)))
            .child(crate::ui::primitives::item_list(rows, p))
    }

    /// Windows drivers, the guest agent, and — when the guest can use it — the Triton setup
    /// as an ordered checklist whose steps tick off from machine state.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn guest_tools(
        &self,
        machine: &Machine,
        original: &HardwareDraft,
        d: &HardwareDraft,
        triton: bool,
        editable: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let windows =
            crate::operations::GuestOs::of(machine) == crate::operations::GuestOs::Windows;
        let can_mount = !self.busy() && machine.state != State::Other;
        let disc = |matches: fn(&str) -> bool| {
            self.details.devices.iter().any(|device| {
                device.kind == "cdrom"
                    && std::path::Path::new(&device.source)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(matches)
            })
        };
        let row = |label: &'static str, control: AnyElement| setting_row(label, control, p);

        let drivers = if disc(|name| name == "virtio-win.iso") {
            div().child("Mounted").into_any_element()
        } else {
            Button::new("install-guest-tools")
                .outline()
                .small()
                .label("Mount…")
                .disabled(!can_mount)
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(m) = this.selected_machine().cloned() {
                        this.open_form(workflows::guest_tools(&m, None), window, cx);
                    }
                }))
                .into_any_element()
        };

        let agent = if crate::operations::has_agent_channel(&self.details.xml) {
            div().child("Added").into_any_element()
        } else {
            Button::new("add-agent")
                .outline()
                .small()
                .label("Add channel")
                .disabled(!editable)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(m) = this.selected_machine().cloned() {
                        this.begin(Job::Operation(m, Operation::AddAgentChannel), None, cx);
                    }
                }))
                .into_any_element()
        };

        let section = section("Guest tools", None)
            .when(windows, |s| s.child(row("VirtIO drivers", drivers)))
            .child(row("Guest agent", agent));
        if !triton {
            return section;
        }

        let triton =
            if original.accel3d {
                h_flex().h(px(40.)).child("DirectX on").into_any_element()
            } else {
                let mounted = disc(|name| name.starts_with("triton-") && name.ends_with(".iso"));
                let neptune = self.details.neptune_ready;
                let action = |button: Button| Some(button.outline().small().into_any_element());
                let mount = (!mounted).then(|| {
                    Button::new("mount-triton-driver").label("Mount").disabled(!can_mount).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(m) = this.selected_machine().cloned() {
                                this.begin(Job::Operation(m, Operation::TritonDriver), None, cx);
                            }
                        }),
                    )
                });
                let bundled = Button::new("triton-bundled-qemu")
                    .label("Use bundled")
                    .disabled(!editable)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(m) = this.selected_machine().cloned() {
                            this.begin(Job::Operation(m, Operation::SwitchToBundledQemu), None, cx);
                        }
                    }));
                let directx = Button::new("triton-directx")
                    .label("Turn on")
                    .disabled(!editable || !neptune)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_hardware(
                            |d| {
                                d.graphics = "virtio".into();
                                d.accel3d = true;
                            },
                            cx,
                        )
                    }));
                let mut steps = vec![
                    ("Mount the driver disc", mounted, mount.and_then(action)),
                    ("Run Install-Triton.cmd in Windows", false, None),
                ];
                if !neptune {
                    steps.push(("Use bundled QEMU", false, action(bundled)));
                }
                steps.push(("Turn on DirectX", d.accel3d, action(directx)));
                v_flex()
                    .children(steps.into_iter().enumerate().map(|(i, (label, done, action))| {
                        triton_step(i + 1, label, done, action, p)
                    }))
                    .into_any_element()
            };
        section.child(row("Triton driver", triton))
    }
}

/// One Triton setup step: its number, or a check once machine state shows it done.
fn triton_step(
    number: usize,
    label: &'static str,
    done: bool,
    action: Option<AnyElement>,
    p: Palette,
) -> Div {
    let marker = div()
        .size(px(20.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if done {
            p.positive
        } else {
            p.border
        })
        .text_size(px(CAPTION_SIZE))
        .text_color(p.muted)
        .map(|m| {
            if done {
                m.child(Icon::new(Glyph::Check).size_3().text_color(p.positive))
            } else {
                m.child(number.to_string())
            }
        });
    h_flex()
        .h(px(40.))
        .gap_3()
        .child(marker)
        .child(
            div().flex_1().min_w_0().truncate().when(done, |l| l.text_color(p.muted)).child(label),
        )
        .children(action)
}
