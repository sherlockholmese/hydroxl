use super::{DisplayView, toolbar::TOOLBAR_HEIGHT};
use crate::backend::State;
use crate::display::{geometry::GuestViewport, vnc};
use crate::theme::Palette;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};
use std::time::Instant;

impl Render for DisplayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::get(cx);
        self.device_scale = window.scale_factor();
        self.sync_system_cursor(window);
        let bounds = self.bounds.clone();
        let (dimensions, scaling) = (self.size, self.scaling);
        let performance = self.performance.clone().filter(|_| self.state == Some(State::Running));
        let revision = self.frame_revision;
        let surface = div()
            .id("guest-display")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(gpui_kit::black())
            .track_focus(&self.focus)
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .when_some(self.image, |this, image| {
                this.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            if let Some(viewport) = GuestViewport::new(
                                dimensions,
                                bounds,
                                scaling,
                                window.scale_factor(),
                            ) {
                                match window.paint_texture_in(image, bounds, viewport.bounds()) {
                                    Ok(()) => {
                                        if let Some(stats) = &performance {
                                            stats.borrow_mut().painted(
                                                Instant::now(),
                                                revision,
                                                dimensions,
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        eprintln!("Could not draw guest display: {error}")
                                    }
                                }
                            }
                        },
                    )
                    .size_full(),
                )
            })
            .when(self.performance.is_some(), |surface| surface.child(self.performance_overlay(p)))
            .children(self.cursor_overlay())
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    h_flex()
                        .absolute()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .px_6()
                        .text_color(p.muted)
                        .when(status == "Connecting", |t| t.child(Spinner::new().small()))
                        .child(div().min_w_0().max_w(px(480.)).child(status))
                        .when(
                            self.session.is_none() && !self.connecting && self.machine_is_active(),
                            |t| {
                                t.child(
                                    Button::new("retry-display")
                                        .outline()
                                        .small()
                                        .label("Reconnect")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.ensure_session(window, cx)
                                        })),
                                )
                            },
                        ),
                )
            })
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if std::env::var_os("HYDROXL_DEBUG_POINTER").is_some() {
                    eprintln!(
                        "viewport {:?} scale {} fullscreen {}",
                        window.viewport_size(),
                        window.scale_factor(),
                        window.is_fullscreen()
                    );
                }
                if !this.input.locked() {
                    let over = this.over_guest(e.position);
                    if !over {
                        this.hover_suppressed = false;
                    }
                    if this.hover_capture()
                        && over
                        && !this.captured
                        && !this.hover_suppressed
                        && !this.toolbar_menu_open
                        && e.pressed_button.is_none()
                        && window.is_window_active()
                    {
                        this.capture(e.position, e.modifiers, window, cx);
                    } else if !over && this.captured && this.buttons == 0 && this.absolute_pointer()
                    {
                        // Leaving the guest image hands input back, so the toolbar and
                        // the rest of the desktop are always one move away.
                        this.release(window);
                        cx.notify();
                    }
                    this.send_pointer(e.position);
                }
                // Only the in-window overlay needs a redraw to follow the pointer.
                if this.captured && this.cursor.is_some() && this.system_cursor.is_none() {
                    cx.notify();
                }
            }))
            .on_mouse_exit(cx.listener(|this, _: &MouseExitEvent, window, cx| {
                // The pointer physically leaving the window hands input back in hover
                // mode. Hover state is not used: GPUI clears it on any key press.
                if this.captured && this.absolute_pointer() && !this.input.locked() {
                    this.release(window);
                    cx.notify();
                }
            }))
            .on_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, window, cx| {
                this.focus.focus(window, cx);
                if this.session.is_some() && !this.captured {
                    // Only the guest image captures; the letterbox around it does not.
                    if (e.modifiers.control && e.modifiers.alt) || !this.over_guest(e.position) {
                        return;
                    }
                    // The capturing click only captures; it is not sent to the guest.
                    this.hover_suppressed = false;
                    this.capture(e.position, e.modifiers, window, cx);
                    return;
                }
                this.buttons |= Self::mask(e.button);
                this.send_pointer(e.position);
            }))
            .capture_any_mouse_up(cx.listener(|this, e: &MouseUpEvent, _, _| {
                if this.buttons & Self::mask(e.button) != 0 {
                    this.buttons &= !Self::mask(e.button);
                    this.send_pointer(e.position);
                }
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, window, _| {
                if !this.captured {
                    return;
                }
                let dy = f32::from(e.delta.pixel_delta(window.line_height()).y);
                if dy == 0. {
                    return;
                }
                if let (Some((x, y)), Some(s)) = (this.guest_point(e.position), &this.session) {
                    let buttons = this.buttons
                        | if dy > 0. {
                            8
                        } else {
                            16
                        };
                    if this.input.locked() {
                        s.relative_pointer(0, 0, buttons);
                        s.relative_pointer(0, 0, this.buttons);
                    } else {
                        s.pointer(x, y, buttons);
                        s.pointer(x, y, this.buttons);
                    }
                }
            }))
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if !this.captured {
                    return;
                }
                if let Some(s) = this.session.clone() {
                    let key = &e.keystroke.key;
                    let modifiers = e.keystroke.modifiers;
                    if (key.eq_ignore_ascii_case("v") && (modifiers.control || modifiers.platform))
                        || (key == "insert" && modifiers.shift)
                    {
                        this.sync_clipboard(&s, true, window, cx);
                    }
                    if let Some(sym) = vnc::keysym(&e.keystroke.key) {
                        s.key(sym, true);
                    }
                    // The guest owns the keyboard while captured.
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|this, e: &KeyUpEvent, _, _| {
                if !this.captured {
                    return;
                }
                if let (Some(s), Some(sym)) = (&this.session, vnc::keysym(&e.keystroke.key)) {
                    s.key(sym, false);
                }
            }))
            .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, window, cx| {
                let now = e.modifiers;
                if !this.captured {
                    this.modifiers = now;
                    return;
                }
                if now.control && now.alt {
                    this.release(window);
                    this.hover_suppressed = true;
                    this.modifiers = now;
                    cx.notify();
                    return;
                }
                let Some(s) = this.session.clone() else {
                    return;
                };
                let was = this.modifiers;
                let pairs = [
                    (was.shift, now.shift),
                    (was.control, now.control),
                    (was.alt, now.alt),
                    (was.platform, now.platform),
                ];
                for ((before, after), sym) in pairs.into_iter().zip(vnc::MODIFIERS) {
                    if before != after {
                        s.key(sym, after);
                    }
                }
                this.modifiers = now;
            }));

        let fullscreen = window.is_fullscreen();
        let show_toolbar =
            !fullscreen || self.toolbar_pinned || self.toolbar_revealed || self.toolbar_menu_open;
        // In fullscreen the toolbar is a sheet owned by the top edge: it slides down to
        // reveal and back up to hide, reversing from mid-flight if the pointer changes its
        // mind, and stops taking input as soon as it starts to leave.
        let now = Instant::now();
        let reduce = cx.reduce_motion();
        if let Some(duration) = self.toolbar_presence.set(fullscreen && show_toolbar, reduce, now)
            && !self.toolbar_presence.shown()
        {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let presence = self.toolbar_presence;
        v_flex()
            .relative()
            .size_full()
            .bg(p.canvas)
            .text_color(p.text)
            .when(!fullscreen, |root| root.child(self.top_bar(p, window, cx)))
            .child(surface)
            .when(fullscreen && presence.mounted(now), |root| {
                let at = |el: Div, v: f32| el.top(px(-TOOLBAR_HEIGHT * (1. - v)));
                root.child(
                    at(div().absolute().left_0().w_full(), presence.value(now))
                        .when(presence.shown(), |bar| bar.occlude())
                        .child(self.top_bar(p, window, cx))
                        .with_animation(
                            SharedString::from(format!(
                                "fullscreen-toolbar-{}",
                                presence.generation()
                            )),
                            Animation::new(presence.remaining(now).max(crate::theme::motion::EXIT)),
                            move |el, _| at(el, presence.value(Instant::now())),
                        ),
                )
            })
            .id("display-window")
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if !view.captured && event.keystroke.key == "f11" && !event.is_held {
                    view.toolbar_revealed = false;
                    window.toggle_fullscreen();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                // A fast move from the guest straight onto the toolbar skips the display's
                // own move events; release here so the toolbar takes the click.
                if view.captured
                    && view.buttons == 0
                    && !view.input.locked()
                    && view.absolute_pointer()
                    && !view.over_guest(event.position)
                {
                    view.release(window);
                    cx.notify();
                }
                if window.is_fullscreen() && !view.toolbar_pinned && !view.toolbar_menu_open {
                    let threshold = if view.toolbar_revealed {
                        TOOLBAR_HEIGHT + 8.
                    } else {
                        8.
                    };
                    let reveal = f32::from(event.position.y) <= threshold && !view.captured;
                    if reveal != view.toolbar_revealed {
                        view.toolbar_revealed = reveal;
                        cx.notify();
                    }
                }
            }))
    }
}
