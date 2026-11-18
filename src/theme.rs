//! Semantic palette and geometry.
//!
//! Colors come from GPUI Component's stock shadcn/ui neutral theme. Hydroxl
//! only adds a raised sidebar plane in dark mode and contrast-safe status text,
//! so components never reference raw colors.
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::*;
use std::borrow::Cow;

pub const SIDEBAR_WIDTH: f32 = 256.;
pub const NARROW_WIDTH: f32 = 720.;
pub const CONTENT_WIDTH: f32 = 880.;
/// Hardware with its category list beside the selected category.
pub const TWO_COLUMN_WIDTH: f32 = 760.;
pub const WIDE_CONTENT_WIDTH: f32 = 1040.;
pub const LABEL_WIDTH: f32 = 128.;
pub const ROW_HEIGHT: f32 = 48.;
pub const HEADER_HEIGHT: f32 = 56.;
pub const WELL_HEIGHT: f32 = 280.;
pub const TITLE_SIZE: f32 = 20.;
pub const BODY_SIZE: f32 = 14.;
pub const CAPTION_SIZE: f32 = 12.;
/// Field labels: smaller and quieter than values, so a dialog title always leads.
pub const LABEL_SIZE: f32 = 13.;
pub const DIALOG_TITLE_SIZE: f32 = 18.;
const FONT: &str = "Geist";

/// Motion roles. One duration per category keeps transitions consistent; GPUI's animation
/// element settles on its final frame when the system asks for reduced motion.
pub mod motion {
    use std::time::Duration;
    /// A view replacing another in place: tab or machine change.
    pub const SWAP: Duration = Duration::from_millis(180);
    /// A bar or panel entering from an edge.
    pub const REVEAL: Duration = Duration::from_millis(200);
    /// Travel for entering content, in pixels. Small enough to read as settling, not sliding.
    pub const TRAVEL: f32 = 6.;
    /// Ease-out cubic: fast start, gentle settle.
    pub fn ease_out(t: f32) -> f32 {
        1. - (1. - t).powi(3)
    }
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
