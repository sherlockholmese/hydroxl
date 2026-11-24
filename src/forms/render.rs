use super::editor::{Control, EditorForm};
use crate::theme::{CAPTION_SIZE, LABEL_SIZE, Palette};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::{
    button::*,
    input::{Input, Textarea},
    progress::Progress,
    select::Select,
    *,
};
use gpui_kit::{prelude::*, *};

/// Field anatomy shared by every dialog: a quiet label 6px above its control.
fn field_label(text: &'static str, p: Palette) -> Div {
    div().text_size(px(LABEL_SIZE)).font_weight(FontWeight::MEDIUM).text_color(p.muted).child(text)
}

/// A validation message in the one error anatomy: danger icon and text.
fn error_line(text: &str, p: Palette) -> Div {
    h_flex()
        .items_start()
        .gap_2()
        .text_color(p.danger)
        .child(Icon::new(Glyph::CircleAlert).size_4().mt(px(2.)).flex_shrink_0())
        .child(div().flex_1().min_w_0().child(crate::ui::plain_error(text)))
}

impl EditorForm {
    /// A field with its validation message directly beneath it, when it has one.
    fn field(&self, index: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let error = self.error.as_deref().filter(|_| self.error_field == Some(index));
        let control = self.field_control(index, p, cx);
        match error {
            Some(error) => {
                v_flex().gap(px(6.)).child(control).child(error_line(error, p)).into_any_element()
            }
            None => control,
        }
    }

    fn field_control(&self, index: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let field = &self.fields[index];
        if let Control::Checkbox(checked) = &field.control {
            return checkbox::Checkbox::new(("checkbox", index))
                .label(field.spec.label)
                .checked(*checked)
                .disabled(self.pending)
                .on_change(cx.listener(move |this, checked: &bool, window, cx| {
                    if this.pending {
                        return;
                    }
                    this.fields[index].control = Control::Checkbox(*checked);
                    this.changed(window, cx);
                }))
                .into_any_element();
        }
        let control = match &field.control {
            Control::Checkbox(_) => unreachable!(),
            Control::Text(state) => {
                // A path field keeps its picker inside the input, so the field stays one
                // control wide and aligns with every other field in the dialog.
                let browse = field.spec.picker.is_some().then(|| {
                    Button::new(("browse", index))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Glyph::FolderOpen))
                        .accessibility_label(format!("Browse for {}", field.spec.label))
                        .tooltip("Browse")
                        .disabled(self.pending)
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.browse(index, window, cx)),
                        )
                });
                Input::new(state)
                    .aria_label(field.spec.label)
                    .disabled(self.pending)
                    .when_some(field.spec.suffix, |input, suffix| {
                        input.suffix(div().text_color(p.muted).child(suffix))
                    })
                    .when_some(browse, |input, browse| input.suffix(browse))
                    .w_full()
                    .into_any_element()
            }
            Control::Select(state) => Select::new(state)
                .accessibility_label(field.spec.label)
                .disabled(self.pending)
                .w_full()
                .into_any_element(),
            Control::Multiline(state) => Textarea::new(state)
                .aria_label(field.spec.label)
                .disabled(self.pending)
                .h(px(320.))
                .into_any_element(),
        };
        v_flex()
            .gap(px(6.))
            .child(field_label(field.spec.label, p))
            .child(control)
            .into_any_element()
    }

    /// Consecutive fields sharing a row id, in order.
    fn rows(&self, indices: &[usize]) -> Vec<Vec<usize>> {
        let mut rows: Vec<Vec<usize>> = Vec::new();
        for &i in indices {
            let row = self.fields[i].spec.row;
            match rows.last_mut() {
                Some(last) if row != 0 && self.fields[last[0]].spec.row == row => last.push(i),
                _ => rows.push(vec![i]),
            }
        }
        rows
    }

    fn row_element(&self, row: Vec<usize>, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        if row.len() == 1 {
            return self.field(row[0], p, cx);
        }
        h_flex()
            .items_start()
            .gap_3()
            .children(row.into_iter().map(|i| div().flex_1().min_w_0().child(self.field(i, p, cx))))
            .into_any_element()
    }

    /// Connected step rail: one segment per step, filled up to the current one.
    fn rail(&self, p: Palette) -> impl IntoElement {
        let mut titles = self.steps.clone();
        titles.push("Review");
        h_flex().gap_2().w_full().children(titles.into_iter().enumerate().map(|(i, title)| {
            let reached = i <= self.step;
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(div().h(px(2.)).rounded_full().bg(if reached {
                    p.text
                } else {
                    p.border
                }))
                .child(
                    div()
                        .text_size(px(CAPTION_SIZE))
                        .font_weight(if i == self.step {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if reached {
                            p.text
                        } else {
                            p.muted
                        })
                        .child(title),
                )
        }))
    }

    fn review(&self, p: Palette, cx: &App) -> impl IntoElement {
        let values = self.values(cx);
        let shown: Vec<bool> = (0..self.fields.len()).map(|i| self.shown(i, &values)).collect();
        v_flex().gap_1().children(
            self.fields.iter().zip(values).zip(shown).filter(|(_, s)| *s).map(
                |((field, value), _)| {
                    let mut shown = field.spec.display(&value);
                    if field.spec.picker.is_some() {
                        shown = std::path::Path::new(&shown)
                            .file_name()
                            .map_or(shown.clone(), |n| n.to_string_lossy().into_owned());
                    }
                    if shown.is_empty() {
                        shown = "None".into();
                    }
                    if let Some(suffix) = field.spec.suffix {
                        shown = format!("{shown} {suffix}");
                    }
                    crate::ui::fact(field.spec.label, div().child(shown), p)
                },
            ),
        )
    }
}

impl Render for EditorForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::get(cx);
        let wizard = !self.steps.is_empty();
        let review = self.is_review();
        let body = if review {
            self.review(p, cx).into_any_element()
        } else {
            let values = self.values(cx);
            let on_step = |i: &usize| {
                (!wizard || self.fields[*i].spec.step == self.step) && self.shown(*i, &values)
            };
            let basic: Vec<usize> = (0..self.fields.len())
                .filter(|i| on_step(i) && !self.fields[*i].spec.advanced)
                .collect();
            let advanced: Vec<usize> = (0..self.fields.len())
                .filter(|i| on_step(i) && self.fields[*i].spec.advanced)
                .collect();
            let mut body = v_flex().gap_4().w_full();
            for row in self.rows(&basic) {
                body = body.child(self.row_element(row, p, cx));
            }
            if !advanced.is_empty() {
                let open = self.advanced_open;
                body = body.child(
                    h_flex().child(
                        Button::new("advanced")
                            .ghost()
                            .small()
                            .label("Advanced")
                            .icon(Icon::new(if open {
                                Glyph::ChevronDown
                            } else {
                                Glyph::ChevronRight
                            }))
                            .text_color(p.muted)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.advanced_open = !this.advanced_open;
                                cx.notify();
                            })),
                    ),
                );
                if open {
                    for row in self.rows(&advanced) {
                        body = body.child(self.row_element(row, p, cx));
                    }
                }
            }
            body.into_any_element()
        };

        let back = (wizard && self.step > 0).then(|| {
            Button::new("back-form")
                .ghost()
                .label("Back")
                .icon(Icon::new(Glyph::ChevronLeft))
                .disabled(self.pending)
                .on_click(cx.listener(Self::back))
        });
        // While work runs, closing does not cancel it, so the button says Hide.
        let cancel = Button::new("cancel-form")
            .outline()
            .label(if self.pending {
                "Hide"
            } else {
                "Cancel"
            })
            .on_click(|_, window, cx| window.close_dialog(cx));
        let trailing = if wizard && !review {
            Button::new("next-form").primary().label("Next").on_click(cx.listener(Self::next))
        } else {
            Button::new("submit-form")
                .primary()
                .label(self.action)
                .loading(self.pending)
                .when(self.destructive, |b| b.danger())
                .disabled(self.pending || self.unavailable)
                .on_click(cx.listener(Self::submit))
        };

        let progress = self.pending.then(|| {
            self.parent
                .upgrade()
                .and_then(|parent| {
                    parent.read(cx).pending.as_ref().map(|pending| pending.progress.clone())
                })
                .unwrap_or_default()
        });
        v_flex()
            .gap_6()
            .text_color(p.text)
            .when(wizard, |this| this.child(div().pt_1().child(self.rail(p))))
            .when(!self.fields.is_empty(), |this| {
                this.child(
                    div()
                        .id("form-fields")
                        .max_h(
                            (window.viewport_size().height - px(260.)).max(px(160.)).min(px(520.)),
                        )
                        .overflow_y_scroll()
                        // Room for focus rings, which sit outside each control.
                        .p(px(3.))
                        .m(px(-3.))
                        .child(body),
                )
            })
            // Errors that name no field (or a field not on screen) stay above the buttons.
            .when_some(
                self.error.clone().filter(|_| {
                    self.error_field.is_none_or(|i| review || !self.shown(i, &self.values(cx)))
                }),
                |this, error| this.child(error_line(&error, p)),
            )
            // The submit button already spins; a bar appears only with real progress to show.
            .when_some(
                progress.filter(|p| p.percent.is_some() || !p.stage.is_empty()),
                |this, progress| {
                    let stage = progress.stage;
                    this.child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_between()
                                    .text_size(px(CAPTION_SIZE))
                                    .child(div().text_color(p.muted).child(stage))
                                    .when_some(progress.percent, |row, percent| {
                                        row.child(
                                            div()
                                                .font_features(crate::ui::tabular())
                                                .child(format!("{percent}%")),
                                        )
                                    }),
                            )
                            .child(
                                Progress::new("form-progress")
                                    .small()
                                    .accessibility_label("Operation progress")
                                    .loading(progress.percent.is_none())
                                    .value(f32::from(progress.percent.unwrap_or_default())),
                            ),
                    )
                },
            )
            .child(
                h_flex().gap_2().children(back).child(div().flex_1()).child(cancel).child(trailing),
            )
    }
}
