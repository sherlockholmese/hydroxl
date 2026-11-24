use crate::backend::{Machine, State};
use crate::theme::motion::Slide;
use crate::theme::{
    BODY_SIZE, CAPTION_SIZE, FACT_HEIGHT, LABEL_WIDTH, NAV_GAP, NAV_ITEM, NAV_WIDTH, Palette,
    SETTING_HEIGHT,
};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};

use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};

/// Status color for a machine. Text always carries the state; color only reinforces it.
pub(crate) fn status_color(machine: &Machine, p: Palette) -> Hsla {
    match machine.state {
        State::Running => p.positive,
        State::Paused => p.attention,
        _ if machine.saved_state => p.attention,
        _ => p.muted,
    }
}

/// OS mark for a machine. Powered-off machines dim their mark; running ones show it fully.
pub(crate) fn logo_tile(machine: &Machine, size: f32, p: Palette, cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .when(!matches!(machine.state, State::Running | State::Paused), |d| d.opacity(0.7))
        .child(super::os_logo::os_logo(machine, size, p, cx))
}

/// The one label-and-value row. Labels share a fixed column so values form one reading
/// edge; values wrap rather than clip, and the label stays level with the first line.
fn label_row(label: SharedString, value: AnyElement, height: f32, p: Palette) -> Div {
    h_flex()
        .items_start()
        .gap_4()
        .w_full()
        .text_size(px(BODY_SIZE))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .min_h(px(height))
                .flex_shrink_0()
                .flex()
                .items_center()
                .text_color(p.muted)
                .child(label),
        )
        .child(div().flex_1().min_w_0().min_h(px(height)).flex().items_center().child(value))
}

/// A read-only fact.
pub(crate) fn ledger(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    p: Palette,
) -> Div {
    fact(label, div().child(value.into()), p)
}

/// A fact whose value is an element, such as a short inline action beside it.
pub(crate) fn fact(label: impl Into<SharedString>, value: impl IntoElement, p: Palette) -> Div {
    label_row(label.into(), value.into_any_element(), FACT_HEIGHT, p)
}

/// A row holding a control: taller than a fact so controls never crowd each other.
pub(crate) fn setting(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    p: Palette,
) -> Div {
    label_row(label.into(), control.into_any_element(), SETTING_HEIGHT, p)
}

/// Titled group with an optional trailing action.
pub(crate) fn section(title: impl Into<SharedString>, action: Option<AnyElement>) -> Div {
    v_flex().gap_1().child(
        h_flex()
            .h(px(FACT_HEIGHT))
            .justify_between()
            .gap_4()
            .child(
                div()
                    .text_size(px(BODY_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.into()),
            )
            .children(action),
    )
}

/// A line that is not ready or not possible yet, stated once in muted text.
pub(crate) fn notice(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_size(px(BODY_SIZE)).text_color(p.muted).child(text.into())
}

/// Figures that change in place (percentages, frame rates) keep their width.
pub(crate) fn tabular() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

pub(crate) fn caption(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_size(px(CAPTION_SIZE)).text_color(p.muted).child(text.into())
}

/// A Lucide icon by its file name, for registries that name icons as data.
pub(crate) fn glyph(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/{name}.svg")))
}

/// Shared menu geometry: wide enough that labels never crowd their check or chevron.
pub(crate) const MENU_MIN_WIDTH: f32 = 220.;

/// The one segmented control: a track with the chosen option raised one step above it.
/// Used for every short, fixed, mutually exclusive choice, machine tabs included, so they
/// all read as the same kind of control.
pub(crate) fn segmented(
    id: impl Into<SharedString>,
    options: &[&'static str],
    selected: Option<usize>,
    enabled: bool,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let pick = std::rc::Rc::new(on_pick);
    let p = Palette::get(cx);
    let id: SharedString = id.into();
    let mut track = h_flex().p_1().gap_1().rounded_lg().bg(p.control);
    for (i, label) in options.iter().enumerate() {
        let on = Some(i) == selected;
        let pick = pick.clone();
        track = track.child(
            Button::new(SharedString::from(format!("{id}-{i}")))
                .small()
                .label(*label)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(p.control)
                        .hover(if on {
                            p.control_selected
                        } else {
                            p.control_hover
                        })
                        .active(p.control_selected)
                        .foreground(if on {
                            p.text
                        } else {
                            p.control_muted
                        }),
                )
                .selected(on)
                .disabled(!enabled)
                .on_click(move |_, window, cx| pick(i, window, cx)),
        );
    }
    h_flex().child(track).into_any_element()
}

/// Hardware groups, shown the way System Settings shows them on macOS: a list of
/// categories beside the selected one. A narrow pane puts them in one row of icons on top.
pub(crate) struct Sections(pub Vec<(&'static str, &'static str, AnyElement)>);

impl Sections {
    pub(crate) fn add(
        mut self,
        title: &'static str,
        icon: &'static str,
        group: impl IntoElement,
    ) -> Self {
        self.0.push((title, icon, group.into_any_element()));
        self
    }

    pub(crate) fn layout(
        self,
        page: Div,
        split: bool,
        slide: Slide,
        cx: &mut gpui_kit::Context<crate::app::Workspace>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let last = self.0.len().saturating_sub(1);
        let selected = slide.target().min(last);
        let now = std::time::Instant::now();
        // Only a category change moves anything. A rebuilt view (new machine, tab switch)
        // arrives settled, so this pane never fades on top of the tab's own fade.
        let moving = slide.moving(now);
        let generation = slide.generation();
        const ITEM: f32 = NAV_ITEM;
        const GAP: f32 = NAV_GAP;
        // One highlight that travels between categories; the items themselves never fade.
        // Wide: a labelled list, travelling down. Narrow: one row of equal icon cells,
        // travelling across. The group's own heading names the selection, so the row
        // carries no labels and never wraps.
        let step = ITEM + GAP;
        let place = move |el: Div, at: f32| {
            let at = px(at.clamp(0., last as f32) * step);
            if split {
                el.top(at)
            } else {
                el.left(at)
            }
        };
        let highlight = div()
            .absolute()
            .when(split, |h| h.left_0().w_full())
            .when(!split, |h| h.top_0().w(px(ITEM)))
            .h(px(ITEM))
            .rounded_md()
            .bg(p.sidebar_accent);
        // The position comes from the slide itself, sampled each frame, so a click while
        // it travels continues from where it is.
        let highlight = if moving {
            place(highlight, slide.position(now))
                .with_animation(
                    SharedString::from(format!("hardware-highlight-{split}-{generation}")),
                    gpui_kit::Animation::new(crate::theme::motion::SWAP),
                    move |el, _| place(el, slide.position(std::time::Instant::now())),
                )
                .into_any_element()
        } else {
            place(highlight, selected as f32).into_any_element()
        };
        let mut nav = if split {
            v_flex().relative().w(px(NAV_WIDTH)).flex_shrink_0().gap(px(GAP)).child(highlight)
        } else {
            // Cells and highlight share one coordinate system, so they cannot drift apart.
            let count = self.0.len() as f32;
            div().relative().flex_shrink_0().h(px(ITEM)).w(px(count * step - GAP)).child(highlight)
        };
        let mut content = None;
        for (i, (title, icon, group)) in self.0.into_iter().enumerate() {
            let on = i == selected;
            let tone = if on {
                p.text
            } else {
                p.muted
            };
            // A real button, so categories take keyboard focus and announce themselves.
            // Its own fill stays clear: the travelling highlight is the selection.
            let clear = gpui_kit::transparent_black();
            let item = Button::new(("hardware-section", i))
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(clear)
                        .hover(if on {
                            clear
                        } else {
                            p.sidebar_hover
                        })
                        .active(if on {
                            clear
                        } else {
                            p.sidebar_hover
                        })
                        .foreground(tone),
                )
                .accessibility_label(title)
                .h(px(ITEM))
                .flex_shrink_0()
                .rounded_md()
                .text_size(px(BODY_SIZE))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.hardware_section.target() != i {
                        let reduce = cx.reduce_motion();
                        this.hardware_section.go(i, reduce, std::time::Instant::now());
                        cx.notify();
                    }
                }));
            nav = nav.child(if split {
                item.w_full().px_3().child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .child(glyph(icon).size_4().text_color(tone))
                        .child(title),
                )
            } else {
                item.absolute()
                    .top_0()
                    .left(px(i as f32 * step))
                    .w(px(ITEM))
                    .child(glyph(icon).size_4().text_color(tone))
                    .tooltip(title)
            });
            if on {
                content = Some(group);
            }
        }
        // The content fades in the same frame the highlight starts moving, on the same
        // clock and curve. The wrapper id is stable per change so child state survives.
        let pane = |content: Option<AnyElement>| {
            v_flex().relative().flex_1().min_w_0().children(content).with_animation(
                SharedString::from(format!("hardware-content-{generation}")),
                gpui_kit::Animation::new(crate::theme::motion::SWAP)
                    .with_easing(crate::theme::motion::emphasized),
                move |el, t| {
                    if moving {
                        el.opacity(t).top(px(crate::theme::motion::TRAVEL * (1. - t)))
                    } else {
                        el
                    }
                },
            )
        };
        if !split {
            return page.child(nav).child(pane(content)).into_any_element();
        }
        page.child(h_flex().items_start().gap_8().child(nav).child(pane(content)))
            .into_any_element()
    }
}

/// Repeated items (disks, cards, shares) in one inset group, divided by hairlines, so each
/// item reads as its own object while the group stays one surface.
pub(crate) fn item_list(rows: Vec<AnyElement>, p: Palette) -> Div {
    let mut list = v_flex().mt_1().rounded_lg().bg(p.well).px_4();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            list = list.child(div().h(px(1.)).w_full().bg(p.border));
        }
        list = list.child(row);
    }
    list
}

/// An error as the user reads it: what happened and what to do, without command chains,
/// socket paths or libvirt prefixes. Known causes get their recovery; anything else keeps
/// only its leading sentence, which the backend already writes for people.
pub(crate) fn plain_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("failed to connect socket")
        || lower.contains("failed to connect to the hypervisor")
    {
        return if lower.contains("permission denied") {
            "No permission for the system connection. Add your user to the libvirt group, then sign in again."
        } else {
            "Libvirt is not running. Start the libvirt service, then retry."
        }
        .into();
    }
    let line = raw
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("error: ").unwrap_or(line))
        .find(|line| !line.is_empty())
        .unwrap_or("The operation failed. Retry, or open the machine log.");
    let mut text: String = match line.chars().next() {
        Some(first) => first.to_uppercase().chain(line.chars().skip(1)).collect(),
        None => String::new(),
    };
    if !text.ends_with(['.', '?', '!']) {
        text.push('.');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::plain_error;

    #[test]
    fn names_recovery_for_connection_failures() {
        let raw = "failed to connect to the hypervisor\nFailed to connect socket to '/run/libvirt/virtqemud-sock': Permission denied";
        assert!(plain_error(raw).starts_with("No permission"));
        let raw = "failed to connect to the hypervisor\nFailed to connect socket to '/run/user/1000/libvirt/virtqemud-sock': No such file or directory";
        assert!(plain_error(raw).starts_with("Libvirt is not running"));
    }

    #[test]
    fn keeps_the_leading_sentence() {
        assert_eq!(plain_error("error: domain is not running\nmore"), "Domain is not running.");
        assert_eq!(
            plain_error("Wait for the current operation to finish."),
            "Wait for the current operation to finish."
        );
    }
}
