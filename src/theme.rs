//! Semantic palette and geometry.
//!
//! Colors come from GPUI Component's stock shadcn/ui neutral theme. Hydroxl
//! only adds a raised sidebar plane in dark mode, contrast-safe status text, and a
//! control ladder (track, hover, selected) that stays distinct from canvas and from the
//! inset well in both modes, so components never reference raw colors.
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::*;
use std::borrow::Cow;

pub const SIDEBAR_WIDTH: f32 = 256.;
pub const NARROW_WIDTH: f32 = 720.;
pub const CONTENT_WIDTH: f32 = 880.;
/// Hardware with its category list beside the selected category.
pub const TWO_COLUMN_WIDTH: f32 = 760.;
pub const WIDE_CONTENT_WIDTH: f32 = 1040.;
pub const LABEL_WIDTH: f32 = 160.;
pub const ROW_HEIGHT: f32 = 48.;
pub const HEADER_HEIGHT: f32 = 56.;
pub const WELL_HEIGHT: f32 = 280.;
/// Prose measure: about 75 characters at body size.
pub const READABLE_WIDTH: f32 = 640.;
pub const TITLE_SIZE: f32 = 20.;
pub const BODY_SIZE: f32 = 14.;
pub const CAPTION_SIZE: f32 = 12.;
/// Field labels: smaller and quieter than values, so a dialog title always leads.
pub const LABEL_SIZE: f32 = 13.;
pub const DIALOG_TITLE_SIZE: f32 = 18.;
/// Settings category list beside the selected category.
pub const NAV_WIDTH: f32 = 192.;
/// One settings category, and the step between categories.
pub const NAV_ITEM: f32 = 32.;
pub const NAV_GAP: f32 = 4.;
/// Label-and-value rows: read-only facts, and rows holding a control.
pub const FACT_HEIGHT: f32 = 32.;
pub const SETTING_HEIGHT: f32 = 40.;
const FONT: &str = "Geist";

/// Motion roles. One duration and one curve per category keeps transitions consistent.
/// GPUI's animation element settles on its final frame when `App::reduce_motion` is set;
/// `reduce_motion_preference` reads that setting from the desktop at startup.
pub mod motion {
    use std::time::{Duration, Instant};
    /// A view replacing another in place: tab, machine or category change.
    pub const SWAP: Duration = Duration::from_millis(180);
    /// A bar or panel entering from an edge.
    pub const REVEAL: Duration = Duration::from_millis(200);
    /// Anything leaving: always quicker than it arrived.
    pub const EXIT: Duration = Duration::from_millis(130);
    /// Travel for entering content, in pixels. Small enough to read as settling, not sliding.
    pub const TRAVEL: f32 = 6.;

    /// A CSS-style cubic Bézier timing curve, solved for x by bisection (monotonic in x).
    fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
        let t = t.clamp(0., 1.);
        let at = |a: f32, b: f32, s: f32| {
            let u = 1. - s;
            3. * u * u * s * a + 3. * u * s * s * b + s * s * s
        };
        let (mut lo, mut hi) = (0f32, 1f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.;
            if at(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        at(y1, y2, (lo + hi) / 2.)
    }
    /// Entering surfaces and structural moves: quick start, long settle.
    pub fn emphasized(t: f32) -> f32 {
        bezier(0.16, 1., 0.3, 1., t)
    }
    /// Leaving surfaces: accelerate away.
    pub fn exit(t: f32) -> f32 {
        bezier(0.4, 0., 1., 1., t)
    }

    /// A surface that enters and leaves, and can reverse mid-flight from where it is.
    /// `value` is 1 when fully present and 0 when gone.
    #[derive(Clone, Copy, Debug)]
    pub struct Presence {
        target: bool,
        from: f32,
        since: Option<Instant>,
        generation: u64,
    }

    impl Presence {
        pub const fn new(shown: bool) -> Self {
            Self {
                target: shown,
                from: shown as u8 as f32,
                since: None,
                generation: 0,
            }
        }
        fn duration(&self) -> Duration {
            if self.target {
                REVEAL
            } else {
                EXIT
            }
        }
        /// Retargets; returns how long the change runs when it started one. With reduced
        /// motion the surface reaches its target at once.
        pub fn set(&mut self, shown: bool, reduce: bool, now: Instant) -> Option<Duration> {
            if shown == self.target {
                return None;
            }
            self.from = self.value(now);
            self.target = shown;
            self.generation += 1;
            self.since = (!reduce).then_some(now);
            self.since.map(|_| self.duration())
        }
        pub fn value(&self, now: Instant) -> f32 {
            let to = self.target as u8 as f32;
            let Some(since) = self.since else {
                return to;
            };
            let t = now.duration_since(since).as_secs_f32() / self.duration().as_secs_f32();
            if t >= 1. {
                return to;
            }
            let eased = if self.target {
                emphasized(t)
            } else {
                exit(t)
            };
            self.from + (to - self.from) * eased
        }
        pub fn mounted(&self, now: Instant) -> bool {
            self.target || self.value(now) > 0.
        }
        /// Whether the surface is still accepting input: never while it leaves.
        pub fn shown(&self) -> bool {
            self.target
        }
        /// Time left in the current change, for the frame driver.
        pub fn remaining(&self, now: Instant) -> Duration {
            self.since.map_or(Duration::ZERO, |since| {
                self.duration().saturating_sub(now.duration_since(since))
            })
        }
        /// Changes on every retarget, so a frame driver keyed by it restarts.
        pub fn generation(&self) -> u64 {
            self.generation
        }
    }

    /// A position that moves between integer slots and, when retargeted mid-move, carries
    /// on from where it is instead of jumping back to where it started.
    #[derive(Clone, Copy, Debug)]
    pub struct Slide {
        from: f32,
        to: usize,
        since: Option<Instant>,
        generation: u64,
    }

    impl Slide {
        pub const fn at(slot: usize) -> Self {
            Self {
                from: slot as f32,
                to: slot,
                since: None,
                generation: 0,
            }
        }
        pub fn target(&self) -> usize {
            self.to
        }
        pub fn position(&self, now: Instant) -> f32 {
            let Some(since) = self.since else {
                return self.to as f32;
            };
            let t = now.duration_since(since).as_secs_f32() / SWAP.as_secs_f32();
            if t >= 1. {
                return self.to as f32;
            }
            self.from + (self.to as f32 - self.from) * emphasized(t)
        }
        /// Moves to `slot`, starting from the current position.
        pub fn go(&mut self, slot: usize, reduce: bool, now: Instant) {
            if slot == self.to {
                return;
            }
            self.from = self.position(now);
            self.to = slot;
            self.generation += 1;
            self.since = (!reduce).then_some(now);
        }
        /// Settles at the target with no motion, for when the view is rebuilt rather than
        /// navigated (a new machine, a tab switch).
        pub fn settle(&mut self) {
            self.from = self.to as f32;
            self.since = None;
        }
        pub fn moving(&self, now: Instant) -> bool {
            self.since.is_some_and(|since| now.duration_since(since) < SWAP)
        }
        pub fn generation(&self) -> u64 {
            self.generation
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn curves_start_and_end_in_place() {
            for curve in [emphasized as fn(f32) -> f32, exit] {
                assert!(curve(0.).abs() < 1e-3);
                assert!((curve(1.) - 1.).abs() < 1e-3);
            }
            assert!(emphasized(0.25) > 0.5, "enter is front-loaded");
            assert!(exit(0.25) < 0.25, "exit accelerates");
        }

        #[test]
        fn presence_reverses_from_where_it_is() {
            let start = Instant::now();
            let mut bar = Presence::new(false);
            bar.set(true, false, start);
            let mid = start + REVEAL / 2;
            let shown_mid = bar.value(mid);
            assert!(shown_mid > 0. && shown_mid < 1.);
            bar.set(false, false, mid);
            assert!((bar.value(mid) - shown_mid).abs() < 1e-4, "no jump on reversal");
            assert!(!bar.mounted(mid + EXIT));
        }

        #[test]
        fn reduced_motion_arrives_at_once() {
            let now = Instant::now();
            let mut bar = Presence::new(false);
            assert_eq!(bar.set(true, true, now), None);
            assert_eq!(bar.value(now), 1.);
            let mut slide = Slide::at(0);
            slide.go(3, true, now);
            assert_eq!(slide.position(now), 3.);
        }

        #[test]
        fn slide_retargets_from_its_current_position() {
            let start = Instant::now();
            let mut slide = Slide::at(0);
            slide.go(4, false, start);
            let mid = start + SWAP / 2;
            let at = slide.position(mid);
            slide.go(1, false, mid);
            assert!((slide.position(mid) - at).abs() < 1e-4);
        }
    }
}

/// Whether the desktop asks for reduced motion. `HYDROXL_REDUCE_MOTION=1` or `0` overrides;
/// otherwise GNOME's `enable-animations` and KDE's animation speed decide.
pub fn reduce_motion_preference() -> bool {
    if let Some(value) = std::env::var_os("HYDROXL_REDUCE_MOTION") {
        return value != "0";
    }
    let gnome = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "false");
    if gnome {
        return true;
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
    });
    config.and_then(|dir| std::fs::read_to_string(dir.join("kdeglobals")).ok()).is_some_and(
        |text| {
            text.lines()
                .filter_map(|line| line.trim().strip_prefix("AnimationDurationFactor="))
                .any(|factor| factor.trim().parse::<f32>().is_ok_and(|f| f == 0.))
        },
    )
}

pub fn load_fonts(cx: &App) {
    let fonts = [
        include_bytes!("../assets/fonts/Geist_Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/Geist_Medium.ttf").as_slice(),
        include_bytes!("../assets/fonts/Geist_SemiBold.ttf").as_slice(),
        include_bytes!("../assets/fonts/Geist_Bold.ttf").as_slice(),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts.into_iter().map(Cow::Borrowed).collect()) {
        eprintln!("Could not load Geist: {error}");
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub sidebar_accent: Hsla,
    pub border: Hsla,
    pub well: Hsla,
    /// Hover on an unselected sidebar row.
    pub sidebar_hover: Hsla,
    /// Segmented track: distinct from canvas and from the inset well it may sit in.
    pub control: Hsla,
    pub control_hover: Hsla,
    /// The chosen segment, one step lighter than its track in both modes.
    pub control_selected: Hsla,
    /// Unselected segment labels: 4.5:1 or better on `control`.
    pub control_muted: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub positive: Hsla,
    pub attention: Hsla,
    pub danger: Hsla,
}
impl Palette {
    pub fn get(cx: &App) -> Self {
        let t = cx.theme();
        let dark = t.mode.is_dark();
        Self {
            canvas: t.background,
            sidebar: t.sidebar,
            sidebar_accent: t.sidebar_accent,
            border: t.border,
            well: if dark {
                t.sidebar
            } else {
                t.muted
            },
            sidebar_hover: t.sidebar_accent.opacity(0.5),
            control: rgb(if dark {
                0x262626
            } else {
                0xe5e5e5
            })
            .into(),
            control_hover: rgb(if dark {
                0x303030
            } else {
                0xf0f0f0
            })
            .into(),
            control_selected: rgb(if dark {
                0x404040
            } else {
                0xffffff
            })
            .into(),
            control_muted: if dark {
                t.muted_foreground
            } else {
                rgb(0x525252).into()
            },
            text: t.foreground,
            muted: t.muted_foreground,
            // Text-safe status roles: 4.5:1 or better on canvas in each mode.
            positive: rgb(if dark {
                0x4ade80
            } else {
                0x15803d
            })
            .into(),
            attention: rgb(if dark {
                0xfbbf24
            } else {
                0xb45309
            })
            .into(),
            danger: rgb(if dark {
                0xf87171
            } else {
                0xdc2626
            })
            .into(),
        }
    }
}

pub fn apply(dark: bool, cx: &mut App) {
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let has_font = cx.text_system().all_font_names().iter().any(|f| f == FONT);
    let t = Theme::global_mut(cx);
    if has_font {
        t.font_family = FONT.into();
    }
    t.font_size = px(BODY_SIZE);
    t.notification.placement = Anchor::BottomRight;
    if dark {
        // shadcn/ui v4 dark sidebar: one step above the canvas.
        t.colors.sidebar = rgb(0x171717).into();
        t.colors.sidebar_accent = rgb(0x262626).into();
        t.colors.sidebar_border = rgb(0x262626).into();
    } else {
        // Keyboard focus needs 3:1 against white; neutral-400 falls short.
        t.colors.ring = rgb(0x737373).into();
    }
    cx.refresh_windows();
}
