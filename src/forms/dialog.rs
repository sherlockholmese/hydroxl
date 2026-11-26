use super::editor::Control;
use super::{EditorForm, FormSpec};
use crate::{
    app::Workspace,
    theme::{BODY_SIZE, DIALOG_TITLE_SIZE, Palette},
};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};
use std::cell::Cell;

/// One dialog frame for every focused task: a title that leads, even padding on all sides,
/// no redundant close button (Cancel and Escape close it), and a short fade-and-settle entrance.
pub fn frame(dialog: Dialog, title: impl Into<SharedString>, width: f32) -> Dialog {
    dialog
        .title(div().pb_2().text_size(px(DIALOG_TITLE_SIZE)).child(title.into()))
        .width(px(width))
        .p_6()
        .close_button(false)
        .entrance(DialogEntrance::FadeSlide)
}

pub fn open(spec: FormSpec, parent: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let title = spec.title.clone();
    let note = spec.note.clone();
    let width = if spec.fields.iter().any(|f| f.multiline) {
        800.
    } else if !spec.steps.is_empty() {
        560.
    } else {
        460.
    };
    let form = cx.new(|cx| EditorForm::new(spec, parent, window, cx));
    let closing = form.downgrade();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false)
            && let Some(field) = form.read(cx).fields.first()
        {
            match &field.control {
                Control::Text(state) => state.focus_handle(cx).focus(window, cx),
                Control::Multiline(state) => state.focus_handle(cx).focus(window, cx),
                Control::Select(_) | Control::Checkbox(_) => {}
            }
        }
        frame(dialog, title.clone(), width)
            .when_some(note.clone(), |dialog, note| {
                dialog.child(
                    div()
                        .pb_2()
                        .max_w(px(420.))
                        .text_color(Palette::get(cx).muted)
                        .text_size(px(BODY_SIZE))
                        .child(note),
                )
            })
            .child(form.clone())
            .on_close({
                let closing = closing.clone();
                move |_, _, cx| {
                    let _ = closing.update(cx, |form, cx| {
                        form.open = false;
                        cx.notify();
                    });
                }
            })
    });
}
