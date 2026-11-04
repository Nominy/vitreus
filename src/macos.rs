//! macOS: the Picasa-style overlay. While Vitreus is the active app the menu
//! bar and Dock auto-hide (macOS shows them again as soon as you switch apps),
//! and the maximized window covers the whole screen, not just the area left
//! between them.

// objc 0.2's msg_send! checks a `verify_message` feature this crate doesn't have.
#![allow(unexpected_cfgs)]

use core_graphics::geometry::CGRect;
use gpui::Window;
use objc::runtime::{BOOL, NO, Object, YES};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

// NSApplicationPresentationOptions
const AUTO_HIDE_DOCK: usize = 1 << 0;
const AUTO_HIDE_MENU_BAR: usize = 1 << 2;

/// Asks AppKit to slide the menu bar and Dock away while Vitreus is active.
/// Call on the main thread, once at start-up.
pub fn hide_menu_bar_and_dock() {
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, setPresentationOptions: AUTO_HIDE_DOCK | AUTO_HIDE_MENU_BAR];
    }
}

/// Grows the window over the whole screen, including where the menu bar and
/// Dock sit. With `force` false only a zoomed (maximized) window is grown, so
/// a window the user sized themselves stays as it is.
pub fn cover_screen(window: &Window, force: bool) {
    // gpui's own Window::window_handle() returns a gpui handle; we want AppKit's.
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let screen: *mut Object = msg_send![ns_window, screen];
        if screen.is_null() {
            return;
        }
        let zoomed: BOOL = msg_send![ns_window, isZoomed];
        if !force && zoomed == NO {
            return;
        }
        let full: CGRect = msg_send![screen, frame];
        let _: () = msg_send![ns_window, setFrame: full display: YES];
    }
}
