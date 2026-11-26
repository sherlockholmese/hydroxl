//! The guest display window renders QEMU displays, captures keyboard and pointer on
//! click (Ctrl+Alt releases), and carries power controls and special keys in a top bar.
mod clipboard;
mod frames;
mod input;
mod render;
mod toolbar;
mod window;

use crate::app::Workspace;
use crate::backend::{Connection, DisplayTarget, Machine, State};
use crate::display::frame_stats::{FrameRates, FrameStats};
use crate::display::geometry::Scaling;
use crate::display::scanout;
use crate::display::session::Session;
use crate::display::{frame::Cursor, vnc::Endpoint};
use gpui_kit::{prelude::*, *};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::{Arc, mpsc},
    task::Poll,
    time::{Duration, Instant},
};

const RECONNECT_DELAYS: [u64; 4] = [1, 2, 4, 8];
const STABLE_DISPLAY_AFTER: Duration = Duration::from_secs(30);
const DISPLAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(24);
const DISPLAY_FRAME_TIMEOUT: Duration = Duration::from_secs(15);

const CAPTURED_KEY_CONTEXT: &str = "GuestDisplayCaptured";

pub(crate) fn bind_keys(cx: &mut App) {
    // GPUI resolves focus navigation before dispatching raw key events.
    cx.bind_keys([
        KeyBinding::new("tab", NoAction, Some(CAPTURED_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(CAPTURED_KEY_CONTEXT)),
    ]);
}

struct GuestResize {
    dimensions: (u16, u16),
    changed_at: Instant,
    sent: bool,
}

struct ClipboardRead {
    revision: u64,
    started: Instant,
    receiver: mpsc::Receiver<anyhow::Result<String>>,
}

pub(crate) struct DisplayView {
    session: Option<Arc<Session>>,
    image: Option<TextureId>,
    gpu_image: bool,
    /// The GPU texture missed a frame, so the next copy must be whole, not just damage.
    gpu_stale: bool,
    input: crate::display::input::InputCapture,
    generation: u64,
    size: (u32, u32),
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scaling: Scaling,
    guest_resize: Option<GuestResize>,
    toolbar_pinned: bool,
    /// Host USB devices this window redirected, by (bus, address), so the menu shows which
    /// are connected.
    usb_attached: std::collections::HashSet<(u8, u8)>,
    toolbar_revealed: bool,
    /// The fullscreen toolbar sliding down from, and back into, the top edge.
    toolbar_presence: crate::theme::motion::Presence,
    toolbar_menu_open: bool,
    connection: Connection,
    device_scale: f32,
    focus: FocusHandle,
    buttons: u8,
    modifiers: Modifiers,
    status: Option<String>,
    /// Keyboard and pointer go to the guest only while captured.
    captured: bool,
    /// Capture on click rather than when the pointer enters the guest image.
    capture_on_click: bool,
    /// Set by Ctrl+Alt in hover mode, so releasing does not recapture at once.
    /// Cleared when the pointer leaves the guest image.
    hover_suppressed: bool,
    /// Tile settling for software frames: last generation seen, when it was seen, and
    /// when the first not-yet-shown change arrived.
    settle: (u64, Instant, Option<Instant>),
    /// Last guest clipboard announcement processed by this viewer.
    guest_clipboard_seen: u64,
    clipboard_reset_seen: u64,
    copy_requested: Option<Instant>,
    clipboard_read: Option<ClipboardRead>,
    last_clipboard_poll: Option<Instant>,
    /// Last text exchanged either way, so a copy is never echoed back.
    clipboard_text: Option<String>,
    clipboard_from_host: bool,
    /// Last host pointer position, where the guest cursor is drawn.
    pointer: Option<Point<Pixels>>,
    cursor: Option<(Arc<RenderImage>, Cursor)>,
    /// The guest cursor as the system cursor: which shape and scale the platform shows,
    /// and that shape's pixels. The compositor moves it with the pointer, so it never
    /// trails the way an in-window overlay does.
    system_cursor: Option<(u64, u32)>,
    system_cursor_pixels: Option<(u64, Arc<[u8]>)>,
    system_cursor_unsupported: bool,
    cursor_generation: u64,
    /// At most one SPICE upload callback is pending on the platform frame clock.
    frame_scheduled: bool,
    gpu_task: Option<Task<()>>,
    last_preview: Option<(u64, Instant)>,
    workspace: WeakEntity<Workspace>,
    machine: String,
    /// Copied from the workspace when it changes; render never reads the workspace.
    state: Option<State>,
    power_status: &'static str,
    busy: bool,
    connecting: bool,
    connection_attempt: u64,
    reconnect_attempts: usize,
    connected_at: Option<Instant>,
    frame_revision: u64,
    performance: Option<Rc<RefCell<FrameStats>>>,
    performance_readout: FrameRates,
    _observe: Option<Subscription>,
    _focus_out: Subscription,
    _activation: Subscription,
}

impl Drop for DisplayView {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            session.close();
        }
    }
}

impl DisplayView {
    /// Connects to the guest display through libvirt.
    pub(crate) fn connect(
        connection: Connection,
        id: String,
        machine: &Machine,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = machine.state;
        let power_status = machine.status();
        // A guest capture has its own explicit hide/show lifecycle. Typing must
        // not leave a second automatic hide active after the capture releases.
        window.set_cursor_hide_mode_override(Some(CursorHideMode::Never));
        let observe = workspace.upgrade().map(|w| {
            cx.observe_in(&w, window, |this: &mut Self, workspace, window, cx| {
                let w = workspace.read(cx);
                let same_connection = w.preferences.connection == this.connection;
                let machine = same_connection
                    .then(|| w.machines.iter().find(|m| m.id == this.machine))
                    .flatten();
                let state = machine.map(|m| m.state);
                let power_status = machine.map_or("Unavailable", Machine::status);
                let busy = same_connection && w.busy();
                let powered_off = same_connection
                    && w.loaded
                    && (state == Some(State::Stopped) || state.is_none());
                let changed = (this.state, this.power_status) != (state, power_status);
                if changed || this.busy != busy {
                    this.state = state;
                    this.power_status = power_status;
                    this.busy = busy;
                    if changed {
                        if powered_off && std::env::var_os("HYDROXL_VNC_SOCKET").is_none() {
                            this.connection_attempt = this.connection_attempt.wrapping_add(1);
                            this.connecting = false;
                            this.detach_session(window);
                            this.status = Some(power_status.into());
                        } else if this.machine_is_active() {
                            this.ensure_session(window, cx);
                        } else if !this.connecting && this.session.is_none() {
                            this.status = Some(power_status.into());
                        }
                    }
                    cx.notify();
                }
            })
        });
        cx.on_release_in(window, |this, window, _| {
            this.release(window);
            if let Some(id) = this.image.take() {
                window.drop_texture(id);
            }
        })
        .detach();
        let focus = cx.focus_handle();
        // Losing focus (another window, host Alt+Tab) releases the capture.
        let focus_out = cx.on_focus_out(&focus, window, |this, _, window, cx| {
            if this.captured {
                this.release(window);
                cx.notify();
            }
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() && this.captured {
                this.release(window);
                cx.notify();
            }
            if window.is_window_active() {
                // Wayland can deliver its selection offer after the focus event.
                this.last_clipboard_poll = None;
            }
        });
        let (scaling, toolbar_pinned, capture_on_click) = workspace
            .upgrade()
            .map(|workspace| {
                let preferences = &workspace.read(cx).preferences;
                (
                    preferences.scaling_for(&id),
                    preferences.toolbar_pinned,
                    preferences.capture_on_click,
                )
            })
            .unwrap_or((Scaling::default(), false, false));
        let mut view = Self {
            session: None,
            connection,
            toolbar_pinned,
            usb_attached: Default::default(),
            toolbar_revealed: false,
            toolbar_presence: crate::theme::motion::Presence::new(false),
            toolbar_menu_open: false,
            image: None,
            gpu_image: false,
            gpu_stale: false,
            input: Default::default(),
            generation: u64::MAX,
            size: (0, 0),
            bounds: Rc::new(Cell::new(Bounds::default())),
            scaling,
            guest_resize: None,
            device_scale: window.scale_factor(),
            focus,
            buttons: 0,
            modifiers: Modifiers::default(),
            status: Some(power_status.into()),
            captured: false,
            capture_on_click,
            hover_suppressed: false,
            guest_clipboard_seen: 0,
            clipboard_reset_seen: 0,
            settle: (u64::MAX, Instant::now(), None),
            clipboard_text: None,
            clipboard_from_host: false,
            copy_requested: None,
            clipboard_read: None,
            last_clipboard_poll: None,
            pointer: None,
            cursor: None,
            system_cursor: None,
            system_cursor_pixels: None,
            system_cursor_unsupported: false,
            cursor_generation: 0,
            frame_scheduled: false,
            gpu_task: None,
            last_preview: None,
            state: Some(state),
            power_status,
            busy: false,
            connecting: false,
            connection_attempt: 0,
            reconnect_attempts: 0,
            connected_at: None,
            frame_revision: 0,
            performance: None,
            performance_readout: FrameRates::default(),
            workspace,
            machine: id,
            _observe: observe,
            _focus_out: focus_out,
            _activation: activation,
        };
        if matches!(state, State::Running | State::Paused)
            || std::env::var_os("HYDROXL_VNC_SOCKET").is_some()
        {
            view.ensure_session(window, cx);
        }
        view
    }

    fn machine_is_active(&self) -> bool {
        matches!(self.state, Some(State::Running | State::Paused))
    }

    pub(crate) fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() && !self.connecting {
            self.reconnect_attempts = 0;
            self.status = Some("Connecting".into());
            self.connecting = true;
            self.connect_session(false, window, cx);
            cx.notify();
        }
    }

    fn connect_session(&mut self, retry: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.connection_attempt = self.connection_attempt.wrapping_add(1);
        let attempt = self.connection_attempt;
        let connection = self.connection;
        let id = self.machine.clone();
        let gpu = window.supports_dma_buf();
        cx.spawn_in(window, async move |this, cx| {
            let mut retry = retry;
            loop {
                if retry {
                    let delay = this.update(cx, |view, cx| {
                        if view.connection_attempt != attempt {
                            return None;
                        }
                        if !view.machine_is_active() {
                            view.connecting = false;
                            view.status = Some(view.power_status.into());
                            cx.notify();
                            return None;
                        }
                        let Some(&seconds) = RECONNECT_DELAYS.get(view.reconnect_attempts) else {
                            view.connecting = false;
                            cx.notify();
                            return None;
                        };
                        view.reconnect_attempts += 1;
                        Some(Duration::from_secs(seconds))
                    });
                    let Ok(Some(delay)) = delay else {
                        return;
                    };
                    cx.background_executor().timer(delay).await;
                }
                let prefer_spice = this.update(cx, |view, cx| {
                    if view.connection_attempt != attempt {
                        return None;
                    }
                    if retry && !view.machine_is_active() {
                        view.connecting = false;
                        view.status = Some(view.power_status.into());
                        cx.notify();
                        return None;
                    }
                    Some(view.machine(cx).is_none_or(|m| m.spice_viewer))
                });
                let Ok(Some(prefer_spice)) = prefer_spice else {
                    return;
                };
                let id = id.clone();
                let mut connecting = blocking::unblock(move || {
                    let target = crate::backend::with_command_deadline(
                        Instant::now() + Duration::from_secs(8),
                        || crate::backend::display_target(connection, &id, prefer_spice),
                    )?;
                    match target {
                        DisplayTarget::Vnc {
                            uri,
                            clipboard,
                        } => {
                            let endpoint = Endpoint::parse(&uri)
                                .ok_or_else(|| anyhow::anyhow!("Invalid guest display address."))?;
                            let session = Session::vnc(&endpoint, clipboard)?;
                            if std::env::var_os("HYDROXL_VNC_SOCKET").is_none()
                                && let Err(error) = crate::backend::with_command_deadline(
                                    Instant::now() + Duration::from_secs(4),
                                    || crate::backend::ensure_live_tablet(connection, &id),
                                )
                            {
                                eprintln!("Could not add a tablet to the running guest: {error}");
                            }
                            Ok(session)
                        }
                        DisplayTarget::Spice(options) => {
                            Session::spice(connection, id, options, gpu)
                        }
                        DisplayTarget::Dbus {
                            graphics,
                            clipboard,
                            usb,
                        } => Session::dbus(connection, &id, graphics, gpu, clipboard, usb),
                    }
                });
                let mut timeout = cx.background_executor().timer(DISPLAY_CONNECT_TIMEOUT);
                let result = poll_fn(|cx| {
                    if let Poll::Ready(result) = Pin::new(&mut connecting).poll(cx) {
                        return Poll::Ready(Some(result));
                    }
                    Pin::new(&mut timeout).poll(cx).map(|()| None)
                })
                .await;
                let Some(result) = result else {
                    let _ = this.update(cx, |view, cx| {
                        if view.connection_attempt == attempt {
                            view.connecting = false;
                            view.status = Some("Timed out connecting to the guest display.".into());
                            cx.notify();
                        }
                    });
                    // Native libvirt attachment can still return after the UI deadline.
                    if let Ok(session) = connecting.await {
                        session.close();
                    }
                    return;
                };
                let mut result = Some(result);
                let keep_waiting = this
                    .update_in(cx, |this, window, cx| {
                        if this.connection_attempt != attempt {
                            return false;
                        }
                        if !this.machine_is_active()
                            && std::env::var_os("HYDROXL_VNC_SOCKET").is_none()
                        {
                            this.connecting = false;
                            this.status = Some(this.power_status.into());
                            cx.notify();
                            return false;
                        }
                        let Some(result) = result.take() else {
                            this.connecting = false;
                            this.status = Some(
                                "The display connection ended. Reopen the display to retry.".into(),
                            );
                            cx.notify();
                            return false;
                        };
                        let wait = match result {
                            Ok(session) => {
                                this.connecting = false;
                                this.attach(session, window, cx);
                                false
                            }
                            Err(error) => {
                                if this.machine_is_active() {
                                    this.status = Some(crate::ui::primitives::plain_error(
                                        &format!("{error:#}"),
                                    ));
                                    cx.notify();
                                    return true;
                                }
                                this.connecting = false;
                                this.status =
                                    Some(crate::ui::primitives::plain_error(&format!("{error:#}")));
                                false
                            }
                        };
                        cx.notify();
                        wait
                    })
                    .unwrap_or(false);
                if let Some(Ok(session)) = result {
                    session.close();
                }
                if !keep_waiting {
                    return;
                }
                retry = true;
            }
        })
        .detach();
    }

    fn attach(&mut self, session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) {
        let frame_clock = matches!(session.as_ref(), Session::Spice(_));
        self.session = Some(session.clone());
        self.connected_at = Some(Instant::now());
        if let Some(stats) = &self.performance {
            stats.borrow_mut().reset();
            self.performance_readout = FrameRates::default();
        }
        self.frame_scheduled = false;
        self.guest_resize = None;
        self.last_preview = None;
        self.guest_clipboard_seen = 0;
        self.clipboard_reset_seen = 0;
        self.copy_requested = None;
        self.clipboard_read = None;
        self.last_clipboard_poll = None;
        self.clipboard_text = None;
        self.clipboard_from_host = false;
        self.generation = u64::MAX;
        self.cursor_generation = 0;
        if let Some((cursor, _)) = self.cursor.take() {
            cx.drop_image(cursor, Some(window));
        }
        // A reconnect keeps showing the last frame rather than a spinner.
        if self.image.is_none() {
            self.status = Some("Connecting".into());
            let waiting = session.clone();
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(DISPLAY_FRAME_TIMEOUT).await;
                let _ = this.update_in(cx, |view, window, cx| {
                    if view.image.is_none()
                        && view
                            .session
                            .as_ref()
                            .is_some_and(|session| Arc::ptr_eq(session, &waiting))
                    {
                        view.detach_session(window);
                        view.status =
                            Some("The guest display did not send a frame in time.".into());
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.focus.focus(window, cx);
        if frame_clock {
            self.schedule_spice_upload(window, cx);
        }
        // GPU draws are copied the moment they arrive; the frame loop only presents.
        {
            let session = session.clone();
            self.gpu_task = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    scanout::DrawOffered(session.frame()).await;
                    let current = this.update_in(cx, |this, window, cx| {
                        let current = this
                            .session
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &session));
                        if current {
                            this.copy_gpu_draw(&session, window, cx);
                        }
                        current
                    });
                    if !matches!(current, Ok(true)) {
                        break;
                    }
                }
            }));
        }
        // SPICE uses platform frame opportunities while visible. A slow timer
        // handles hidden-window housekeeping and restarts the frame loop on show.
        // Other transports retain their existing low-latency polling.
        cx.spawn_in(window, async move |this, cx| {
            let mut interval = Duration::from_millis(if frame_clock {
                50
            } else {
                4
            });
            loop {
                cx.background_executor().timer(interval).await;
                match this.update_in(cx, |this, window, cx| {
                    if !this.session.as_ref().is_some_and(|current| Arc::ptr_eq(current, &session))
                    {
                        return (false, false);
                    }
                    let visible = window.is_visible();
                    let alive = if frame_clock && visible {
                        this.schedule_spice_upload(window, cx);
                        true
                    } else {
                        this.upload(window, cx)
                    };
                    (alive, visible)
                }) {
                    Ok((true, visible)) => {
                        interval = Duration::from_millis(if visible && !frame_clock {
                            4
                        } else {
                            50
                        });
                    }
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn detach_session(&mut self, window: &mut Window) {
        self.release(window);
        if let Some(session) = self.session.take() {
            session.close();
        }
        self.gpu_task = None;
        if let Some(image) = self.image.take() {
            window.drop_texture(image);
        }
        self.gpu_image = false;
    }

    fn machine(&self, cx: &App) -> Option<Machine> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        if workspace.preferences.connection != self.connection {
            return None;
        }
        workspace.machines.iter().find(|m| m.id == self.machine).cloned()
    }
}
