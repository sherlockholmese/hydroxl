extern crate gpui_kit as gpui;
mod app;
mod appliance;
mod backend;
mod display;
mod forms;
mod operations;
mod theme;
mod ui;
mod workflows;
use app::{Preferences, Workspace};
use gpui_kit::*;
use std::path::PathBuf;
gpui_kit::actions!(
    hydroxl,
    [NewMachineAction, RefreshAction, PreferencesAction, AboutAction, QuitAction]
);

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Hydroxl\n\nUsage: hydroxl [--demo] [--import] [FILE.ova | FILE.ovf | FILE.xml]");
        return;
    }
    let demo = args.iter().any(|a| a == "--demo");
    let initial = args.iter().find(|a| !a.to_string_lossy().starts_with('-')).map(PathBuf::from);
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        theme::load_fonts(cx);
        gpui_kit::init(cx);
        theme::apply(Preferences::load().dark, cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-n", NewMachineAction, Some("Hydroxl")),
            KeyBinding::new("f5", RefreshAction, Some("Hydroxl")),
            KeyBinding::new("ctrl-,", PreferencesAction, Some("Hydroxl")),
            KeyBinding::new("ctrl-q", QuitAction, Some("Hydroxl")),
        ]);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1120.), px(780.)),
                cx,
            ))),
            window_min_size: Some(size(px(360.), px(540.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Hydroxl".into()),
                ..Default::default()
            }),
            app_id: Some("dev.hydroxl.Hydroxl".into()),
            ..Default::default()
        };
        match gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(demo, initial.clone(), window, cx))
        }) {
            Ok((_, view)) => {
                cx.activate(true);
                let _ = view;
            }
            Err(error) => {
                eprintln!("Could not open Hydroxl: {error}");
                cx.quit();
            }
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
