use crate::QuitAction;
use crate::app::Workspace;
use crate::theme::{BODY_SIZE, NARROW_WIDTH, Palette};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{prelude::*, *};

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Workspace {
    /// Retargets the Save / Discard bar from the draft, and schedules the frame that
    /// unmounts it once it has left.
    fn sync_save_bar(&mut self, cx: &mut Context<Self>) {
        let want = self.tab == 1 && self.selected_machine().is_some() && self.hardware_dirty();
        let reduce = cx.reduce_motion();
        if let Some(duration) = self.save_bar.set(want, reduce, std::time::Instant::now())
            && !want
        {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_memory_input(window, cx);
        self.sync_save_bar(cx);
        let p = Palette::get(cx);
        let narrow = window.viewport_size().width < px(NARROW_WIDTH);
        let show_library = !narrow || self.library_open || self.selected.is_none();
        let show_detail = !narrow || !show_library || self.machines.is_empty();
        // Hardware splits into two columns when the machine pane can hold two
        // label-and-control columns side by side.
        let pane = f32::from(window.viewport_size().width)
            - if show_library && !narrow {
                crate::theme::SIDEBAR_WIDTH + 24.
            } else {
                0.
            };
        self.wide = pane - 64. >= crate::theme::TWO_COLUMN_WIDTH;
        let mut workspace = h_flex().items_start().size_full().min_h_0();
        // In the narrow layout library and machine pane replace each other. A swap the user
        // asked for slides the arriving pane in from the side it belongs to; a resize
        // across the breakpoint reflows without motion.
        let swapping =
            narrow && self.pane_swapped.is_some_and(|at| at.elapsed() < crate::theme::motion::SWAP);
        let arrive = move |pane: AnyElement, from_left: bool, id: &'static str| {
            if !swapping {
                return pane;
            }
            let travel = if from_left {
                -crate::theme::motion::TRAVEL
            } else {
                crate::theme::motion::TRAVEL
            };
            div()
                .relative()
                .size_full()
                .child(pane)
                .with_animation(
                    id,
                    Animation::new(crate::theme::motion::SWAP)
                        .with_easing(crate::theme::motion::emphasized),
                    move |el, t| el.opacity(t).left(px(travel * (1. - t))),
                )
                .into_any_element()
        };
        if show_library && !(narrow && self.machines.is_empty()) {
            let full = narrow;
            let library = self.sidebar(narrow, cx);
            workspace = workspace.child(if full {
                arrive(library, true, "narrow-library")
            } else {
                library
            });
        }
        if show_detail {
            let detail = v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.detail(narrow, cx))
                .into_any_element();
            workspace = workspace.child(if narrow && !show_library {
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(arrive(detail, false, "narrow-detail"))
                    .into_any_element()
            } else {
                detail
            });
        }
        h_flex()
            .size_full()
            .bg(p.canvas)
            .text_color(p.text)
            .text_size(px(BODY_SIZE))
            .track_focus(&self.focus)
            .key_context("Hydroxl")
            .on_action(cx.listener(Self::create))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::preferences))
            .on_action(cx.listener(Self::about))
            .on_action(|_: &QuitAction, _, cx| cx.quit())
            .child(workspace)
    }
}
