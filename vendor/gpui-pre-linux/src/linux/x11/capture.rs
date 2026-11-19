// Additive guest-input APIs. The normal XI2 dispatch path remains unchanged.
use x11rb::protocol::xfixes::ConnectionExt as _;

#[derive(Default)]
struct X11Capture {
    hidden: bool,
    /// Shown instead of nothing while hidden, so the server moves the guest's cursor.
    image: Option<gpui::CursorImage>,
    /// The X cursor made from `image`, while it is this window's cursor.
    image_cursor: Option<xproto::Cursor>,
    /// Whether this client's XFixes hide is in effect; hide and show must balance.
    xfixes_hidden: bool,
    locked: bool,
    shortcuts: bool,
    delta: Point<f64>,
    position: Point<Pixels>,
}
impl X11ClientState {
    fn release_capture(&mut self, window: xproto::Window) {
        if let Some(capture) = self.captures.remove(&window) {
            if capture.locked {
                let _ = self.xcb_connection.ungrab_pointer(x11rb::CURRENT_TIME);
                let root = self.xcb_connection.setup().roots[self.x_root_index].root;
                let _ = self.xcb_connection.xinput_xi_select_events(
                    root,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICE_GROUPS,
                        mask: vec![],
                    }],
                );
            }
            if capture.shortcuts {
                let _ = self.xcb_connection.ungrab_keyboard(x11rb::CURRENT_TIME);
            }
            if capture.xfixes_hidden {
                let _ = self.xcb_connection.xfixes_show_cursor(window);
            }
            if let Some(cursor) = capture.image_cursor {
                self.restore_style_cursor(window);
                let _ = self.xcb_connection.free_cursor(cursor);
            }
            self.xcb_connection.flush().log_err();
        }
    }

    /// Whether `window` currently shows a guest cursor image.
    fn shows_cursor_image(&self, window: xproto::Window) -> bool {
        self.captures.get(&window).is_some_and(|capture| capture.image_cursor.is_some())
    }

    fn restore_style_cursor(&mut self, window: xproto::Window) {
        let style = self.cursor_styles.get(&window).copied().unwrap_or(CursorStyle::Arrow);
        if let Some(cursor) = self.get_cursor_icon(style) {
            let _ = self.xcb_connection.change_window_attributes(
                window,
                &ChangeWindowAttributesAux {
                    cursor: Some(cursor),
                    ..Default::default()
                },
            );
        }
    }

    /// Applies a hidden capture's cursor: its guest image when one is set, else
    /// XFixes hiding.
    fn apply_hidden_cursor(&mut self, window: xproto::Window) -> anyhow::Result<()> {
        let image = self.captures.get(&window).and_then(|capture| capture.image.clone());
        let created = match image {
            Some(image) => self.create_image_cursor(&image).log_err(),
            None => None,
        };
        let capture = self.captures.entry(window).or_default();
        let previous = capture.image_cursor.take();
        let hide = created.is_none();
        let toggle = capture.xfixes_hidden != hide;
        capture.xfixes_hidden = hide;
        capture.image_cursor = created;
        if toggle {
            if hide {
                // XFixes requires version negotiation before HideCursor (v4).
                let version = self.xcb_connection.xfixes_query_version(4, 0)?.reply()?;
                anyhow::ensure!(version.major_version >= 4, "XFixes cursor hiding is unsupported");
                self.xcb_connection.xfixes_hide_cursor(window)?.check()?;
            } else {
                self.xcb_connection.xfixes_show_cursor(window)?.check()?;
            }
        }
        if let Some(cursor) = created {
            self.xcb_connection.change_window_attributes(
                window,
                &ChangeWindowAttributesAux {
                    cursor: Some(cursor),
                    ..Default::default()
                },
            )?;
        } else if previous.is_some() {
            self.restore_style_cursor(window);
        }
        if let Some(previous) = previous {
            self.xcb_connection.free_cursor(previous)?;
        }
        Ok(())
    }

    /// An ARGB cursor through XRender, sized for the screen's scale factor.
    fn create_image_cursor(&self, image: &gpui::CursorImage) -> anyhow::Result<xproto::Cursor> {
        use x11rb::protocol::render::{self, ConnectionExt as _};
        let scaled = crate::linux::cursor_image::scale_cursor(image, self.scale_factor)
            .context("invalid cursor image")?;
        let connection = &self.xcb_connection;
        let formats = connection.render_query_pict_formats()?.reply()?;
        let format = formats
            .formats
            .iter()
            .find(|format| {
                format.type_ == render::PictType::DIRECT
                    && format.depth == 32
                    && format.direct.alpha_shift == 24
                    && format.direct.alpha_mask == 0xff
                    && format.direct.red_shift == 16
                    && format.direct.green_shift == 8
                    && format.direct.blue_shift == 0
            })
            .context("no ARGB32 picture format")?
            .id;
        let root = connection.setup().roots[self.x_root_index].root;
        let (width, height) = (scaled.width as u16, scaled.height as u16);
        let pixmap = connection.generate_id()?;
        connection.create_pixmap(32, pixmap, root, width, height)?;
        let gc = connection.generate_id()?;
        connection.create_gc(gc, pixmap, &xproto::CreateGCAux::default())?;
        connection.put_image(
            xproto::ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            width,
            height,
            0,
            0,
            0,
            32,
            &scaled.bgra,
        )?;
        let picture = connection.generate_id()?;
        connection.render_create_picture(picture, pixmap, format, &render::CreatePictureAux::default())?;
        let cursor = connection.generate_id()?;
        connection.render_create_cursor(cursor, picture, scaled.hot_x as u16, scaled.hot_y as u16)?;
        connection.render_free_picture(picture)?;
        connection.free_gc(gc)?;
        connection.free_pixmap(pixmap)?;
        Ok(cursor)
    }
    fn capture_raw_motion(&mut self, event: &xinput::RawMotionEvent) {
        let Some(window) = self.keyboard_focused_window else {
            return;
        };
        let Some(capture) = self.captures.get_mut(&window).filter(|c| c.locked) else {
            return;
        };
        let mut values = event.axisvalues_raw.iter();
        // XI2 values are packed for set mask bits; X/Y need not both be present.
        for (word_index, word) in event.valuator_mask.iter().enumerate() {
            for bit in 0..32 {
                if word & (1 << bit) == 0 {
                    continue;
                }
                let Some(value) = values.next() else {
                    return;
                };
                let value = value.integral as f64 + value.frac as f64 / 4294967296.;
                match word_index * 32 + bit {
                    0 => capture.delta.x += value,
                    1 => capture.delta.y += value,
                    _ => {}
                }
            }
        }
    }
}
impl X11Client {
    // A core grab routes buttons as core events, even though normal GPUI input is
    // XI2. Keep their logical position fixed at the lock origin, like Wayland lock.
    fn capture_core_button(&self, event: xproto::ButtonPressEvent, down: bool) {
        let Some(window) = self.get_window(event.event) else {
            return;
        };
        let state = self.0.borrow();
        let Some(capture) = state.captures.get(&event.event).filter(|c| c.locked) else {
            return;
        };
        let position = capture.position;
        let modifiers = modifiers_from_state(event.state);
        drop(state);
        match button_or_scroll_from_event_detail(event.detail.into()) {
            Some(ButtonOrScroll::Button(button)) => {
                if down {
                    window.handle_input(PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button,
                        position,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }));
                } else {
                    window.handle_input(PlatformInput::MouseUp(gpui::MouseUpEvent {
                        button,
                        position,
                        modifiers,
                        click_count: 1,
                    }));
                }
            }
            Some(ButtonOrScroll::Scroll(direction)) if down => {
                let delta = match direction {
                    ScrollDirection::Up => Point::new(0., SCROLL_LINES),
                    ScrollDirection::Down => Point::new(0., -SCROLL_LINES),
                    ScrollDirection::Left => Point::new(SCROLL_LINES, 0.),
                    ScrollDirection::Right => Point::new(-SCROLL_LINES, 0.),
                };
                window.handle_input(PlatformInput::Scroll(make_scroll_wheel_event(
                    position, delta, modifiers,
                )));
            }
            _ => {}
        }
    }

    pub(super) fn release_guest_capture(&self, window: xproto::Window) {
        self.0.borrow_mut().release_capture(window);
    }
    pub(super) fn capture_cursor(
        &self,
        window: xproto::Window,
        hidden: bool,
    ) -> anyhow::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.captures.get(&window).is_some_and(|c| c.hidden) == hidden {
            return Ok(());
        }
        if hidden {
            state.captures.entry(window).or_default().hidden = true;
            state.apply_hidden_cursor(window)?;
        } else if let Some(capture) = state.captures.get_mut(&window) {
            capture.hidden = false;
            let shown = capture.xfixes_hidden;
            capture.xfixes_hidden = false;
            let image_cursor = capture.image_cursor.take();
            if shown {
                state.xcb_connection.xfixes_show_cursor(window)?.check()?;
            }
            if let Some(cursor) = image_cursor {
                state.restore_style_cursor(window);
                state.xcb_connection.free_cursor(cursor)?;
            }
            if state.cursor_hidden_window == Some(window) {
                state.restore_cursor_after_hide();
            }
        }
        state.xcb_connection.flush()?;
        Ok(())
    }
    pub(super) fn capture_cursor_image(
        &self,
        window: xproto::Window,
        image: Option<gpui::CursorImage>,
    ) -> anyhow::Result<()> {
        let mut state = self.0.borrow_mut();
        let capture = state.captures.entry(window).or_default();
        capture.image = image;
        if capture.hidden {
            state.apply_hidden_cursor(window)?;
        }
        state.xcb_connection.flush()?;
        Ok(())
    }
    pub(super) fn capture_pointer(&self, window: xproto::Window, lock: bool) -> anyhow::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.captures.get(&window).is_some_and(|c| c.locked) == lock {
            return Ok(());
        }
        let root = state.xcb_connection.setup().roots[state.x_root_index].root;
        if lock {
            anyhow::ensure!(
                state.keyboard_focused_window == Some(window),
                "window has no keyboard focus"
            );
            anyhow::ensure!(
                !state.captures.iter().any(|(&other, c)| other != window && c.locked),
                "another window owns pointer lock"
            );
            state
                .xcb_connection
                .xinput_xi_select_events(
                    root,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICE_GROUPS,
                        mask: vec![xinput::XIEventMask::RAW_MOTION],
                    }],
                )?
                .check()?;
            let reply = state
                .xcb_connection
                .grab_pointer(
                    false,
                    window,
                    xproto::EventMask::BUTTON_PRESS
                        | xproto::EventMask::BUTTON_RELEASE
                        | xproto::EventMask::POINTER_MOTION,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                    window,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )?
                .reply()?;
            if reply.status != xproto::GrabStatus::SUCCESS {
                state
                    .xcb_connection
                    .xinput_xi_select_events(
                        root,
                        &[xinput::EventMask {
                            deviceid: XINPUT_ALL_DEVICE_GROUPS,
                            mask: vec![],
                        }],
                    )?
                    .check()?;
                anyhow::bail!("X11 refused pointer grab: {:?}", reply.status);
            }
        } else {
            state.xcb_connection.ungrab_pointer(x11rb::CURRENT_TIME)?.check()?;
            state
                .xcb_connection
                .xinput_xi_select_events(
                    root,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICE_GROUPS,
                        mask: vec![],
                    }],
                )?
                .check()?;
        }
        let position = state.last_location;
        let capture = state.captures.entry(window).or_default();
        capture.position = position;
        capture.locked = lock;
        capture.delta = Point::default();
        state.xcb_connection.flush()?;
        Ok(())
    }
    pub(super) fn capture_locked(&self, window: xproto::Window) -> bool {
        self.0.borrow().captures.get(&window).is_some_and(|c| c.locked)
    }
    pub(super) fn capture_motion(&self, window: xproto::Window) -> Point<f64> {
        self.0
            .borrow_mut()
            .captures
            .get_mut(&window)
            .map(|c| std::mem::take(&mut c.delta))
            .unwrap_or_default()
    }
    pub(super) fn capture_shortcuts(
        &self,
        window: xproto::Window,
        inhibit: bool,
    ) -> anyhow::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.captures.get(&window).is_some_and(|c| c.shortcuts) == inhibit {
            return Ok(());
        }
        if inhibit {
            anyhow::ensure!(
                state.keyboard_focused_window == Some(window),
                "window has no keyboard focus"
            );
            anyhow::ensure!(
                !state.captures.iter().any(|(&other, c)| other != window && c.shortcuts),
                "another window owns the keyboard grab"
            );
            let reply = state
                .xcb_connection
                .grab_keyboard(
                    true,
                    window,
                    x11rb::CURRENT_TIME,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )?
                .reply()?;
            anyhow::ensure!(
                reply.status == xproto::GrabStatus::SUCCESS,
                "X11 refused keyboard grab: {:?}",
                reply.status
            );
        } else {
            state.xcb_connection.ungrab_keyboard(x11rb::CURRENT_TIME)?.check()?;
        }
        state.captures.entry(window).or_default().shortcuts = inhibit;
        state.xcb_connection.flush()?;
        Ok(())
    }
}
