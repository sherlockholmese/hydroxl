use super::devices::human_bytes;
use super::primitives::caption;
use crate::app::{Job, Workspace};
use crate::backend::{Machine, State};
use crate::operations::Operation;
use crate::theme::{BODY_SIZE, LABEL_WIDTH, Palette, WELL_HEIGHT};
use crate::workflows;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};

/// A ledger row whose value can be any element, so editable facts sit in the same grid
/// as read-only ones.
fn row(label: &'static str, value: impl IntoElement, p: Palette) -> Div {
    h_flex()
        .gap_4()
        .w_full()
        .min_h(px(34.))
        .items_center()
        .text_size(px(BODY_SIZE))
        .child(div().w(px(LABEL_WIDTH)).flex_shrink_0().text_color(p.muted).child(label))
        .child(div().flex_1().min_w_0().child(value))
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    div().truncate().child(value.into()).into_any_element()
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
                .child(caption(machine.status(), p))
                .into_any_element(),
        };
        let well = div()
            .id("display-well")
            .h(px(WELL_HEIGHT))
            .w_full()
            .rounded_lg()
            .bg(p.well)
            .overflow_hidden()
            .child(well)
            .when(on, |w| {
                w.cursor_pointer().on_click(cx.listener(|this, _, window, cx| {
                    this.open_display(window, cx);
                }))
            });

        let details = &self.details;
        let xml = roxmltree::Document::parse(&details.xml).ok();
        let config = workflows::HardwareDraft::read(machine, &details.xml);

        // System
        let os_name = crate::backend::os_name(&machine.os)
            .map(str::to_owned)
            .unwrap_or_else(|| "Not set".to_owned());
        let os_button = h_flex().child({
            let m = machine.clone();
            Button::new("change-os")
                .outline()
                .small()
                .label(os_name)
                .dropdown_caret(true)
                .disabled(self.busy())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_form(workflows::operating_system(&m), window, cx)
                }))
        });
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
            "KVM"
        } else {
            "Emulated"
        };
        let system = v_flex()
            .child(row("Operating system", os_button, p))
            .child(row("Architecture", self.architecture_control(machine, cx), p))
            .child(row("Processor", text(processor), p))
            .child(row("Memory", text(machine.memory()), p))
            .child(row("Firmware", text(firmware), p))
            .child(row("Execution", text(acceleration), p));

        // Storage and connections
        let disks: Vec<String> = details
            .devices
            .iter()
            .filter(|d| d.kind == "disk")
            .map(|d| {
                details.disk_sizes.iter().find(|(t, ..)| *t == d.target).map_or_else(
                    || file_name(&d.source),
                    |(_, capacity, used)| {
                        format!("{} of {}", human_bytes(*used), human_bytes(*capacity))
                    },
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
                .accessibility_label("Start with this computer")
                .disabled(self.busy())
                .on_change(cx.listener(move |this, on: &bool, _, cx| {
                    this.begin(Job::Operation(m.clone(), Operation::Autostart(*on)), None, cx);
                }))
        };
        let connections = v_flex()
            .child(row(
                "Storage",
                text(if disks.is_empty() {
                    "No disk".to_owned()
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

        let heading = |title: &'static str| {
            div().pb_2().text_size(px(BODY_SIZE)).font_weight(FontWeight::SEMIBOLD).child(title)
        };
        let column =
            |title: &'static str, body: Div| v_flex().w_full().child(heading(title)).child(body);
        v_flex()
            .gap_8()
            .child(well)
            .child(column("System", system))
            .child(column("Storage and connections", connections))
            .when(!machine.notes.trim().is_empty(), |this| {
                this.child(
                    v_flex().gap_2().child(heading("Notes")).child(
                        div().max_w(px(640.)).text_color(p.muted).child(machine.notes.clone()),
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
