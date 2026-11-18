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
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_memory_input(window, cx);
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
        if show_library && !(narrow && self.machines.is_empty()) {
            workspace = workspace.child(self.sidebar(narrow, cx));
        }
        if show_detail {
            workspace = workspace
                .child(v_flex().flex_1().min_w_0().h_full().child(self.detail(narrow, cx)));
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
            .on_action(|_: &QuitAction, _, cx| cx.quit())
            .child(workspace)
    }
}
