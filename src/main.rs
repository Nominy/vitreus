#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod catalog;
mod codecs;
mod decode;
mod info;
mod loader;
#[cfg(target_os = "macos")]
mod macos;
mod register;
mod settings;
mod update;
mod viewer;

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};

use crate::viewer::*;

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.iter().any(|a| a == "--register" || a == "--unregister") {
        std::process::exit(register::run(&args));
    }

    let mut initial = None;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--version" | "-V") => {
                println!("vitreus {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some("--help" | "-h") => {
                println!(
                    "Vitreus {}: a fast photo viewer\n\n\
                     usage: vitreus [photo-or-folder]\n\
                     \x20      vitreus --register [--machine]    Windows: make Vitreus a photo viewer\n\
                     \x20      vitreus --unregister [--machine]  Windows: undo --register",
                    env!("CARGO_PKG_VERSION")
                );
                return;
            }
            // macOS adds a process serial number when launched from Finder.
            Some(s) if s.starts_with("-psn_") => {}
            _ if initial.is_none() => initial = Some(PathBuf::from(arg)),
            _ => {}
        }
    }

    let app = Application::new().with_assets(assets::Assets);

    // macOS hands over files opened from Finder as URLs, also after launch.
    let (opened_tx, mut opened_rx) = futures::channel::mpsc::unbounded::<PathBuf>();
    app.on_open_urls(move |urls| {
        for path in urls.iter().filter_map(|url| path_from_file_url(url)) {
            opened_tx.unbounded_send(path).ok();
        }
    });

    app.run(move |cx: &mut App| {
        bind_keys(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let restore = Bounds::centered(None, size(px(1280.0), px(840.0)), cx);
        // macOS: start over the whole screen; the menu bar and Dock auto-hide
        // while Vitreus is active (src/macos.rs). Elsewhere: maximized.
        #[cfg(target_os = "macos")]
        let window_bounds = {
            macos::hide_menu_bar_and_dock();
            match cx.primary_display() {
                Some(display) => WindowBounds::Windowed(display.bounds()),
                None => WindowBounds::Maximized(restore),
            }
        };
        #[cfg(not(target_os = "macos"))]
        let window_bounds = WindowBounds::Maximized(restore);
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                title: Some("Vitreus".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.0), px(13.0))),
            }),
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(560.0), px(360.0))),
            app_id: Some("vitreus".into()),
            ..Default::default()
        };
        let window = cx
            .open_window(options, |window, cx| {
                cx.new(|cx| Viewer::new(initial, window, cx))
            })
            .expect("failed to open the viewer window");

        // AppKit may keep a new window clear of the menu bar; once Vitreus is
        // active (menu bar and Dock hidden), grow it over the full screen, and
        // again whenever it comes back while still maximized.
        #[cfg(target_os = "macos")]
        window
            .update(cx, |_, window, cx| {
                let mut first = true;
                cx.observe_window_activation(window, move |_, window, _| {
                    if window.is_window_active() {
                        macos::cover_screen(window, std::mem::take(&mut first));
                    }
                })
                .detach();
            })
            .ok();

        cx.spawn(async move |cx| {
            while let Some(path) = opened_rx.next().await {
                let opened = window.update(cx, |viewer, window, cx| {
                    viewer.open(path, cx);
                    window.activate_window();
                });
                if opened.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.activate(true);
    });
}

fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("right", Next, context),
        KeyBinding::new("space", Next, context),
        KeyBinding::new("left", Previous, context),
        KeyBinding::new("backspace", Previous, context),
        KeyBinding::new("home", First, context),
        KeyBinding::new("end", Last, context),
        KeyBinding::new("up", ZoomIn, context),
        KeyBinding::new("=", ZoomIn, context),
        KeyBinding::new("shift-=", ZoomIn, context),
        KeyBinding::new("secondary-=", ZoomIn, context),
        KeyBinding::new("down", ZoomOut, context),
        KeyBinding::new("-", ZoomOut, context),
        KeyBinding::new("secondary--", ZoomOut, context),
        KeyBinding::new("f", ZoomFit, context),
        KeyBinding::new("0", ZoomFit, context),
        KeyBinding::new("secondary-0", ZoomFit, context),
        KeyBinding::new("a", ZoomActual, context),
        KeyBinding::new("1", ZoomActual, context),
        KeyBinding::new("l", RotateLeft, context),
        KeyBinding::new("r", RotateRight, context),
        KeyBinding::new("secondary-[", RotateLeft, context),
        KeyBinding::new("secondary-]", RotateRight, context),
        KeyBinding::new("f11", ToggleFullscreen, context),
        KeyBinding::new("enter", ToggleFullscreen, context),
        KeyBinding::new("i", ToggleInfo, context),
        KeyBinding::new("secondary-,", ToggleSettings, context),
        KeyBinding::new("tab", ToggleChrome, context),
        KeyBinding::new("delete", DeleteFile, context),
        KeyBinding::new("secondary-backspace", DeleteFile, context),
        KeyBinding::new("w", RevealFile, context),
        KeyBinding::new("secondary-c", CopyImage, context),
        KeyBinding::new("secondary-o", OpenFile, context),
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("secondary-w", Close, context),
        KeyBinding::new("secondary-q", Close, context),
    ];
    if cfg!(target_os = "macos") {
        bindings.push(KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, context));
    }
    cx.bind_keys(bindings);
}

/// `file:///Users/me/My%20Photo.jpg` → `/Users/me/My Photo.jpg`.
fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let mut iter = rest.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hex = [iter.next()?, iter.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    // `/C:/photos/a.jpg` on Windows.
    let decoded = match decoded.as_bytes() {
        [b'/', _, b':', ..] if cfg!(windows) => decoded[1..].to_owned(),
        _ => decoded,
    };
    Some(PathBuf::from(decoded))
}
