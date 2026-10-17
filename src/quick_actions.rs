//! Quick-action bodies: the behaviours behind the quick-action bindings (open System Settings,
//! open Finder, lock the screen).
//!
//! The interception itself no longer lives here: every global shortcut is matched by the binding
//! engine (`key_bindings`), and this module keeps only what the actions do, all of it on the main
//! thread.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::event_tap::{keyboard, tap_location};
use crate::event_tap::{
    CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventRef, CGEventSetFlags,
    CGEventSetIntegerValueField, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::ffi::{make_nsstring, CFRelease};
use crate::log_debug;
use std::ffi::c_void;

/// Open System Settings via the x-apple.systempreferences: URL scheme; LaunchServices launches or
/// raises it. "Open Settings" means the system's settings, not this app's window.
pub(crate) fn open_system_settings() {
    unsafe {
        let url_str = make_nsstring("x-apple.systempreferences:");
        // URLWithString: returns an autoreleased NSURL (+0) we do not own; never CFRelease it.
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
        CFRelease(url_str as *const c_void);
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        // openURL: returns BOOL; objc2 validates the return encoding, so receive it as bool.
        let opened: bool = msg_send![workspace, openURL: url];
        log_debug!("[quick] System Settings opened: {}", opened);
    }
}

/// Lock the screen by synthesizing macOS's Control+Command+Q.
///
/// The synthesized events carry `SYNTHETIC_MARKER`: they travel through the session event tap,
/// where a user binding on that very chord would otherwise pick them up and lock again, forever.
pub(crate) fn lock_screen() {
    // macOS 26 removed the legacy CGSession path, so use the public CoreGraphics event API.
    const KEYCODE_Q: u16 = 12;
    const K_FLAG_COMMAND_CONTROL: CGEventFlags = keyboard::FLAG_COMMAND | keyboard::FLAG_CONTROL;
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_Q, true);
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_Q, false);
        if down.is_null() || up.is_null() {
            // Whichever half was created still carries +1 ownership: the failure path must
            // release the non-null one or it leaks.
            if !down.is_null() {
                CFRelease(down as *const c_void);
            }
            if !up.is_null() {
                CFRelease(up as *const c_void);
            }
            crate::log_info!("[quick] lock screen failed: could not create keyboard event");
            return;
        }
        for event in [down, up] {
            CGEventSetFlags(event, K_FLAG_COMMAND_CONTROL);
            CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
        }
        CGEventPost(tap_location::SESSION_EVENT_TAP, down);
        CGEventPost(tap_location::SESSION_EVENT_TAP, up);
        CFRelease(down as *const c_void);
        CFRelease(up as *const c_void);
    }
    log_debug!("[quick] lock screen requested");
}

/// Open a NEW Finder window every time (Win+E semantics; openURL: dedupes and only raises an
/// existing window showing the folder). Activate Finder first (IgnoreOtherApps), then post one
/// Cmd+N straight to Finder's process via CGEventPostToPid -- pid-targeted delivery does not depend
/// on activation timing: Finder itself always dequeues it and creates the window. The new window is
/// created asynchronously: remember the focused-window id before Cmd+N, poll until the focused
/// window id changes (the new window has appeared), then maximize it right away (zoom, NOT
/// fullscreen). AX calls need the main thread; this function already runs there. The app holds the
/// Accessibility permission (its event taps require it), so synthesizing keystrokes and AX writes
/// is legitimate.
pub(crate) fn open_new_finder_window() {
    unsafe {
        let mut finder_app = find_finder_app();
        if finder_app.is_null() {
            // Finder not running: openURL: launches it via LaunchServices with the home folder;
            // wait for the launch, then maximize the window the same way.
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            let path = make_nsstring(&home);
            // fileURLWithPath: returns an autoreleased NSURL (+0) we do not own; never release.
            let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: path];
            CFRelease(path as *const c_void);
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let opened: bool = msg_send![workspace, openURL: url];
            log_debug!("[quick] Finder launched with home folder: {}", opened);
            // Cold start can take seconds: wait up to ~4s.
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(200));
                finder_app = find_finder_app();
                if !finder_app.is_null() {
                    break;
                }
            }
            if finder_app.is_null() {
                log_debug!("[quick] Finder did not launch in time");
                return;
            }
            let pid: i32 = msg_send![finder_app, processIdentifier];
            // The launch-opened folder window IS the new window; maximize once it shows in AX.
            for _ in 0..12 {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if crate::window_management::focused_cgwid_of_pid(pid).is_some() {
                    let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                    log_debug!("[quick] launched Finder window maximized: {}", ok);
                    return;
                }
            }
            return;
        }

        let pid: i32 = msg_send![finder_app, processIdentifier];
        // The focused window before Cmd+N: lets the poll tell the new window from the old one.
        let prev_cgwid = crate::window_management::focused_cgwid_of_pid(pid);
        // NSApplicationActivateIgnoringOtherApps = 1 << 1; activateWithOptions: returns BOOL and
        // must be received as bool (objc2 validates return encodings in debug builds).
        let activated: bool = msg_send![finder_app, activateWithOptions: 2isize];
        log_debug!("[quick] Finder activated: {} pid={}", activated, pid);
        post_cmd_n_to_pid(pid);
        for _ in 0..12 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let cur = crate::window_management::focused_cgwid_of_pid(pid);
            if let Some(cgwid) = cur {
                if Some(cgwid) != prev_cgwid {
                    let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                    log_debug!("[quick] new Finder window maximized: {}", ok);
                    return;
                }
            }
        }
        log_debug!("[quick] new Finder window did not appear in time");
    }
}

/// Hide every regular application, revealing the desktop. Our own app is excluded: hiding it
/// would strand the settings window and this app's own panels.
///
/// This is the all-apps form of Command+H, not "show desktop" (a Mission Control reveal) and not
/// "minimize everything". Show all windows brings the apps back, including any the user had hidden
/// before: the state is deliberately not tracked per app, so one switch mirrors the key.
pub(crate) fn hide_all_apps() {
    set_all_apps_visible(false);
}

/// Unhide every regular application.
pub(crate) fn show_all_apps() {
    set_all_apps_visible(true);
}

/// Hide or unhide every running regular (Dock-visible) application.
///
/// `NSRunningApplication` is the receiver that reaches other processes; `NSApplication` has no
/// per-app hide that does. Both `hide` and `unhide` take no argument and return BOOL, so a partial
/// failure is possible: it is logged rather than treated as fatal, because one unresponsive app must
/// not stop the rest, and the two selectors are sent through separate typed `msg_send!` calls
/// (a dynamic selector would leave objc2 without the return encoding).
pub(crate) fn set_all_apps_visible(visible: bool) {
    let own_pid = std::process::id() as i32;
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![apps, count];
        let mut changed = 0usize;
        let mut failed = 0usize;
        for i in 0..count {
            let app: *mut AnyObject = msg_send![apps, objectAtIndex: i as isize];
            if app.is_null() {
                continue;
            }
            // NSApplicationActivationPolicyRegular = 0; background and helper agents have no
            // windows to hide, and hiding one can break its host.
            let policy: isize = msg_send![app, activationPolicy];
            if policy != 0 {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid <= 0 || pid == own_pid {
                continue;
            }
            let ok: bool = if visible {
                msg_send![app, unhide]
            } else {
                msg_send![app, hide]
            };
            if ok {
                changed += 1;
            } else {
                failed += 1;
            }
        }
        log_debug!(
            "[quick] {} all apps: {} changed, {} failed",
            if visible { "show" } else { "hide" },
            changed,
            failed
        );
    }
}

/// Find Finder in the running applications (returns an NSRunningApplication, a +0 reference we do
/// not own).
unsafe fn find_finder_app() -> *mut AnyObject {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let finder_ns = make_nsstring("com.apple.finder");
    let apps: *mut AnyObject = msg_send![workspace, runningApplications];
    let count: usize = msg_send![apps, count];
    let mut found: *mut AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: i as isize];
        // bundleIdentifier is a copy-property getter returning a +0 reference we do NOT own;
        // never CFRelease it (early release double-frees when the pool drains).
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        if bundle.is_null() {
            continue;
        }
        let is_finder: bool = msg_send![bundle, isEqualToString: finder_ns];
        if is_finder {
            found = app;
            break;
        }
    }
    CFRelease(finder_ns as *const c_void);
    found
}

/// Post one Cmd+N (down + up) targeted at the given process. The event enters that process's own
/// queue and is handled by it, so focus changes in other apps cannot steal it.
unsafe fn post_cmd_n_to_pid(pid: i32) {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventPostToPid(pid: i32, event: CGEventRef);
    }
    // kVK_ANSI_N = 45 (matches shortcut.rs's "n" -> 0x2D).
    const KEY_N: u16 = 0x2D;
    const K_FLAG_COMMAND: CGEventFlags = keyboard::FLAG_COMMAND;
    let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, true);
    let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, false);
    // Our own synthesized events must never be picked up by the binding engine.
    CGEventSetIntegerValueField(down, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventSetIntegerValueField(up, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventSetFlags(down, K_FLAG_COMMAND);
    CGEventSetFlags(up, K_FLAG_COMMAND);
    CGEventPostToPid(pid, down);
    std::thread::sleep(std::time::Duration::from_millis(30));
    CGEventPostToPid(pid, up);
    // CGEventCreateKeyboardEvent returns +1; release after use.
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
}
