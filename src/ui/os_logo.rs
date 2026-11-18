//! Guest operating system marks. The machine's libosinfo id (or, failing that, its name)
//! selects a logo; unknown systems fall back to a neutral monitor glyph.
use crate::backend::Machine;
use crate::theme::Palette;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{prelude::*, *};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

struct Mark {
    svg: &'static str,
    /// Brand color. `None` keeps the colors embedded in the SVG.
    color: Option<u32>,
}

macro_rules! mark {
    ($file:literal, $color:expr) => {
        Mark {
            svg: include_str!(concat!("../../assets/os/", $file, ".svg")),
            color: $color,
        }
    };
}

/// Ordered: the first matching needle wins, so specific names precede generic ones.
fn mark_for(key: &str) -> Option<Mark> {
    let has = |needles: &[&str]| needles.iter().any(|n| key.contains(n));
    Some(
        if has(&[
            "win/11",
            "windows 11",
            "win11",
            "win/2k22",
            "win/2k25",
            "server 2022",
            "server 2025",
        ]) {
            mark!("windows-11", Some(0x0078d4))
        } else if has(&[
            "win/10",
            "windows 10",
            "win10",
            "win/8",
            "windows 8",
            "win/2k12",
            "win/2k16",
            "win/2k19",
            "server 2016",
            "server 2019",
        ]) {
            mark!("windows-10", Some(0x00a4ef))
        } else if has(&["win/", "windows", "winxp", "win7", "vista"]) {
            if has(&[
                "win/7",
                "windows 7",
                "win7",
                "vista",
                "xp",
                "win/2k8",
                "win/2k3",
                "win/2k",
                "win/me",
                "win/9",
            ]) {
                mark!("windows-classic", None)
            } else {
                mark!("windows-11", Some(0x0078d4))
            }
        } else if has(&["ubuntu"]) {
            mark!("ubuntu", Some(0xe95420))
        } else if has(&["fedora"]) {
            mark!("fedora", Some(0x51a2da))
        } else if has(&["debian"]) {
            mark!("debian", Some(0xa81d33))
        } else if has(&["archlinux", "arch linux"]) || key.split_whitespace().any(|w| w == "arch") {
            mark!("archlinux", Some(0x1793d1))
        } else if has(&["linuxmint", "linux mint", "mint"]) {
            mark!("linuxmint", Some(0x86be43))
        } else if has(&["suse"]) {
            mark!("opensuse", Some(0x73ba25))
        } else if has(&["manjaro"]) {
            mark!("manjaro", Some(0x35bf5c))
        } else if has(&["centos"]) {
            mark!("centos", Some(0x262577))
        } else if has(&["rhel", "redhat", "red hat"]) {
            mark!("redhat", Some(0xee0000))
        } else if has(&["rocky"]) {
            mark!("rockylinux", Some(0x10b981))
        } else if has(&["alma"]) {
            mark!("almalinux", Some(0x000000))
        } else if has(&["alpine"]) {
            mark!("alpinelinux", Some(0x0d597f))
        } else if has(&["gentoo"]) {
            mark!("gentoo", Some(0x54487a))
        } else if has(&["nixos"]) {
            mark!("nixos", Some(0x5277c3))
        } else if has(&["kali"]) {
            mark!("kalilinux", Some(0x557c94))
        } else if has(&["pop!_os", "popos", "pop os", "pop-os"]) {
            mark!("popos", Some(0x48b9c7))
        } else if has(&["elementary"]) {
            mark!("elementary", Some(0x64baff))
        } else if has(&["zorin"]) {
            mark!("zorin", Some(0x15a6f0))
        } else if has(&["freebsd"]) {
            mark!("freebsd", Some(0xab2b28))
        } else if has(&["openbsd"]) {
            mark!("openbsd", Some(0xf2ca30))
        } else if has(&["netbsd"]) {
            mark!("netbsd", Some(0xff6600))
        } else if has(&["android"]) {
            mark!("android", Some(0x34a853))
        } else if has(&["macos", "mac os", "osx"]) {
            mark!("macos", Some(0x000000))
        } else if has(&["linux"]) {
            mark!("linux", Some(0xfcc624))
        } else {
            return None;
        },
    )
}

fn luminance(rgb: u32) -> f32 {
    let channel = |shift: u32| {
        let c = ((rgb >> shift) & 0xff) as f32 / 255.;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}
fn hex(color: Hsla) -> u32 {
    let c = color.to_rgb();
    ((c.r * 255.).round() as u32) << 16
        | ((c.g * 255.).round() as u32) << 8
        | (c.b * 255.).round() as u32
}

thread_local! {
    static CACHE: RefCell<HashMap<(&'static str, u32), Arc<Image>>> = RefCell::new(HashMap::new());
}

/// The machine's OS mark at `size` pixels. Brand colors that would fall under 2.5:1 against
/// the canvas use the text color instead, so every mark stays visible in both themes.
pub(crate) fn os_logo(machine: &Machine, size: f32, p: Palette, cx: &App) -> AnyElement {
    let key = if machine.icon.is_empty() {
        format!("{} {}", machine.os, machine.name).to_lowercase()
    } else {
        machine.icon.clone()
    };
    let Some(mark) = mark_for(&key) else {
        return Icon::new(Glyph::Monitor)
            .with_size(px(size))
            .text_color(p.muted)
            .into_any_element();
    };
    let fill = mark.color.map(|brand| {
        let (a, b) = (luminance(brand), luminance(hex(cx.theme().background)));
        let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
        if ratio < 2.5 {
            hex(p.text)
        } else {
            brand
        }
    });
    let image = CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry((mark.svg, fill.unwrap_or(u32::MAX)))
            .or_insert_with(|| {
                let svg = match fill {
                    Some(color) => {
                        mark.svg.replacen("<svg ", &format!("<svg fill=\"#{color:06x}\" "), 1)
                    }
                    None => mark.svg.to_owned(),
                };
                Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
            })
            .clone()
    });
    img(image).size(px(size)).flex_shrink_0().into_any_element()
}
