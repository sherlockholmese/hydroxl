use super::devices::disk_usage;
use super::primitives::{fact as row, section};
use crate::app::{Job, Workspace};
use crate::backend::{Machine, QemuBackend, State};
use crate::operations::Operation;
use crate::theme::{Palette, READABLE_WIDTH, WELL_HEIGHT};
use crate::workflows;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};

/// A value that wraps instead of clipping, so long lists (addresses, forwards) stay readable.
fn text(value: impl Into<SharedString>) -> AnyElement {
    div().child(value.into()).into_any_element()
}

impl Workspace {
    pub(crate) fn overview(
        &self,
        machine: &Machine,
        _narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let on = matches!(machine.state, State::Running | State::Paused);
        // The display well: the guest's screen when a frame was captured, otherwise
        // the display protocol over an icon that matches power state. Never fakes live video.
        let live = on
            .then_some(self.live_preview.as_ref())
            .flatten()
            .filter(|(id, _)| id == &machine.id)
            .map(|(_, preview)| preview.clone());
        let preview = live
            .map(|frame| img(frame).size_full().object_fit(ObjectFit::Contain).into_any_element())
            .or_else(|| {
                self.details.preview.clone().map(|frame| {
                    img(frame).size_full().object_fit(ObjectFit::Contain).into_any_element()
                })
            });
        let well = match preview {
            Some(frame) => div().size_full().child(frame).into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    Icon::new(if machine.state == State::Running {
                        Glyph::Monitor
                    } else {
                        Glyph::MonitorOff
                    })
                    .size(px(40.))
                    .text_color(p.muted),
                )
                .into_any_element(),
        };
        let surface =
            div().h(px(WELL_HEIGHT)).w_full().rounded_lg().bg(p.well).overflow_hidden().child(well);
        // While the machine is on, the well is a real button: focusable, named, and the
        // same action as the header's Open display.
        let well = if on {
            let clear = gpui_kit::transparent_black();
            Button::new("display-well")
                .custom(ButtonCustomVariant::new(cx).color(clear).hover(clear).active(clear))
                .accessibility_label("Open display")
                .w_full()
                .h(px(WELL_HEIGHT))
                .p_0()
                .rounded_lg()
                .child(surface)
                .on_click(cx.listener(|this, _, window, cx| this.open_display(window, cx)))
                .into_any_element()
        } else {
            surface.into_any_element()
        };

        let details = &self.details;
        let xml = roxmltree::Document::parse(&details.xml).ok();
        let config = workflows::HardwareDraft::read(machine, &details.xml);

        // System
        let os_name = crate::backend::os_name(&machine.os)
            .map(str::to_owned)
            .unwrap_or_else(|| "Not set".to_owned());
        let processor = vcpus(machine.cpus);
        let firmware = match &config {
            Some(c) => {
                let base = if c.uefi {
                    "UEFI"
                } else {
                    "BIOS"
                };
                if c.platform.tpm {
                    format!("{base} with TPM 2.0")
                } else {
                    base.to_owned()
                }
            }
            None => "Unknown".to_owned(),
        };
        let acceleration = if machine.acceleration == "KVM" {
            "Virtualized"
        } else {
            "Emulated"
        };
        let system = v_flex()
            .child(row("Operating system", text(os_name), p))
            .child(row("Architecture", text(machine.architecture().to_owned()), p))
            .child(row("Processor", text(processor), p))
            .child(row("Memory", text(machine.memory()), p))
            .child(row("Firmware", text(firmware), p))
            .child(row("Execution", text(acceleration), p))
            .child(row(
                "QEMU backend",
                h_flex().items_center().gap_3().child(text(machine.qemu_backend.label())).when(
                    machine.qemu_backend != QemuBackend::Bundled && machine.can_edit(),
                    |row| {
                        let machine = machine.clone();
                        row.child(
                            Button::new("switch-to-bundled-qemu")
                                .outline()
                                .small()
                                .label("Use bundled")
                                .disabled(self.busy())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.begin(
                                        Job::Operation(
                                            machine.clone(),
                                            Operation::SwitchToBundledQemu,
                                        ),
                                        None,
                                        cx,
                                    );
                                })),
                        )
                    },
                ),
                p,
            ));

        // Storage and connections
        let disks: Vec<String> = details
            .devices
            .iter()
            .filter(|d| d.kind == "disk")
            .map(|d| {
                details.disk_sizes.iter().find(|(t, ..)| *t == d.target).map_or_else(
                    || file_name(&d.source),
                    |(_, capacity, used)| disk_usage(*used, *capacity),
                )
            })
            .collect();
        let media = details
            .devices
            .iter()
            .find(|d| d.kind == "cdrom" && d.source != "Empty")
            .map(|d| file_name(&d.source));
        let network = if !details.addresses.is_empty() {
            format!("{}, {}", machine.network, details.addresses.join(", "))
        } else {
            machine.network.clone()
        };
        let graphics =
            config.as_ref().map(|c| workflows::label(workflows::GRAPHICS, &c.graphics).to_owned());
        let display = match graphics {
            Some(g) => format!("{}, {g}", machine.display),
            None => machine.display.clone(),
        };
        let shares: Vec<&str> = details
            .devices
            .iter()
            .filter(|d| d.kind == "share")
            .map(|d| d.target.as_str())
            .collect();
        let forwards: Vec<String> = xml
            .as_ref()
            .map(|doc| {
                doc.descendants()
                    .filter(|n| n.has_tag_name("portForward"))
                    .filter_map(|n| {
                        let range = n.children().find(|c| c.has_tag_name("range"))?;
                        let host = range.attribute("start")?;
                        Some(format!("{host} → {}", range.attribute("to").unwrap_or(host)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let autostart = {
            let m = machine.clone();
            Switch::new("overview-autostart")
                .checked(machine.autostart)
                .accessibility_label("Start with computer")
                .disabled(self.busy())
                .on_change(cx.listener(move |this, on: &bool, _, cx| {
                    this.begin(Job::Operation(m.clone(), Operation::Autostart(*on)), None, cx);
                }))
        };
        let connections = v_flex()
            .child(row(
                "Storage",
                text(if disks.is_empty() {
                    "None".to_owned()
                } else {
                    disks.join(", ")
                }),
                p,
            ))
            .when_some(media, |c, media| c.child(row("CD/DVD", text(media), p)))
            .child(row("Network", text(network), p))
            .when(!forwards.is_empty(), |c| {
                c.child(row("Forwarded ports", text(forwards.join(", ")), p))
            })
            .child(row("Display", text(display), p))
            .child(row(
                "Shared folders",
                text(if shares.is_empty() {
                    "None".to_owned()
                } else {
                    shares.join(", ")
                }),
                p,
            ))
            .child(row(
                "Snapshots",
                text(match details.snapshots.len() {
                    0 => "None".to_owned(),
                    n => n.to_string(),
                }),
                p,
            ))
            .child(row("Start with computer", autostart, p));

        let column = |title: &'static str, body: Div| section(title, None).w_full().child(body);
        v_flex()
            .gap_8()
            .child(well)
            .child(column("System", system))
            .child(column("Storage and connections", connections))
            .when(!machine.notes.trim().is_empty(), |this| {
                this.child(
                    section("Notes", None).child(
                        div()
                            .max_w(px(READABLE_WIDTH))
                            .text_color(p.muted)
                            .child(machine.notes.clone()),
                    ),
                )
            })
            .into_any_element()
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or(path.to_owned(), |n| n.to_string_lossy().into_owned())
}

pub(crate) fn vcpus(n: u32) -> String {
    if n == 1 {
        "1 vCPU".into()
    } else {
        format!("{n} vCPUs")
    }
}
