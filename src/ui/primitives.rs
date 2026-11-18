use crate::backend::{Machine, State};
use crate::theme::{BODY_SIZE, CAPTION_SIZE, LABEL_WIDTH, Palette};

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

/// One label/value pair. Labels share a fixed column so values form one reading edge.
pub(crate) fn ledger(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    p: Palette,
) -> Div {
    h_flex()
        .gap_4()
        .w_full()
        .min_h(px(32.))
        .items_center()
        .text_size(px(BODY_SIZE))
        .child(div().w(px(LABEL_WIDTH)).flex_shrink_0().text_color(p.muted).child(label.into()))
        .child(div().flex_1().min_w_0().child(value.into()))
}

/// Titled group with an optional trailing action. Pages place hairlines between groups.
pub(crate) fn section(
    title: impl Into<SharedString>,
    action: Option<AnyElement>,
    _p: Palette,
) -> Div {
    v_flex().gap_1().child(
        h_flex()
            .h(px(32.))
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

pub(crate) fn caption(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_size(px(CAPTION_SIZE)).text_color(p.muted).child(text.into())
}

/// A Lucide icon by its file name, for registries that name icons as data.
pub(crate) fn glyph(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/{name}.svg")))
}

/// Shared menu geometry: wide enough that labels never crowd their check or chevron.
pub(crate) const MENU_MIN_WIDTH: f32 = 220.;

/// The one segmented control: a muted track with the chosen option raised. Used for every
/// short, fixed, mutually exclusive choice so they all read as the same kind of control.
pub(crate) fn segmented(
    id: impl Into<SharedString>,
    options: &[&'static str],
    selected: Option<usize>,
    enabled: bool,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
    let pick = std::rc::Rc::new(on_pick);
    let p = Palette::get(cx);
    let id: SharedString = id.into();
    let mut track = h_flex().p(px(3.)).gap_1().rounded_lg().bg(p.well);
    for (i, label) in options.iter().enumerate() {
        let on = Some(i) == selected;
        let pick = pick.clone();
        track = track.child(
            Button::new(SharedString::from(format!("{id}-{i}")))
                .small()
                .label(*label)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(if on {
                            p.canvas
                        } else {
                            p.well
                        })
                        .hover(if on {
                            p.canvas
                        } else {
                            p.sidebar_accent
                        })
                        .active(p.canvas)
                        .foreground(if on {
                            p.text
                        } else {
                            p.muted
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
/// categories beside the selected one. A narrow pane puts the categories in a bar on top.
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
        selected: usize,
        from: usize,
        cx: &mut gpui_kit::Context<crate::app::Workspace>,
    ) -> AnyElement {
        let p = Palette::get(cx);
        let last = self.0.len().saturating_sub(1);
        let selected = selected.min(last);
        let from = from.min(last);
        const ITEM: f32 = 34.;
        const GAP: f32 = 2.;
        // One highlight that travels between categories; the items themselves never fade.
        let highlight = div()
            .absolute()
            .left_0()
            .w_full()
            .h(px(ITEM))
            .rounded_md()
            .bg(p.sidebar_accent)
            .with_animation(
                SharedString::from(format!("hardware-highlight-{from}-{selected}")),
                gpui_kit::Animation::new(crate::theme::motion::SWAP)
                    .with_easing(crate::theme::motion::ease_out),
                move |el, t| {
                    let y = |i: usize| i as f32 * (ITEM + GAP);
                    el.top(px(y(from) + (y(selected) - y(from)) * t))
                },
            );
        let mut nav = if split {
            v_flex().relative().w(px(196.)).flex_shrink_0().gap(px(GAP)).child(highlight)
        } else {
            // Too narrow for a side list: the categories wrap into a bar on top, all visible.
            h_flex().flex_wrap().gap_1()
        };
        let mut content = None;
        for (i, (title, icon, group)) in self.0.into_iter().enumerate() {
            let on = i == selected;
            nav = nav.child(
                h_flex()
                    .id(("hardware-section", i))
                    .relative()
                    .h(px(ITEM))
                    .px_2p5()
                    .gap_2p5()
                    .rounded_md()
                    .cursor_pointer()
                    .flex_shrink_0()
                    .text_size(px(BODY_SIZE))
                    .when(!split && on, |item| item.bg(p.sidebar_accent))
                    .text_color(if on {
                        p.text
                    } else {
                        p.muted
                    })
                    .when(!on, |item| item.hover(|style| style.text_color(p.text)))
                    .child(glyph(icon).size_4().text_color(if on {
                        p.text
                    } else {
                        p.muted
                    }))
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.hardware_section != i {
                            this.hardware_section_from = this.hardware_section;
                            this.hardware_section = i;
                            cx.notify();
                        }
                    })),
            );
            if on {
                content = Some(group);
            }
        }
        let pane = |content: Option<AnyElement>| {
            v_flex().relative().flex_1().min_w_0().children(content).with_animation(
                SharedString::from(format!("hardware-content-{selected}")),
                gpui_kit::Animation::new(crate::theme::motion::SWAP)
                    .with_easing(crate::theme::motion::ease_out),
                |el, t| el.opacity(t).top(px(crate::theme::motion::TRAVEL * (1. - t))),
            )
        };
        if !split {
            return page.child(nav).child(pane(content)).into_any_element();
        }
        page.child(h_flex().items_start().gap_10().child(nav).child(pane(content)))
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
