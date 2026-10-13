//! Main-thread execution of a binding action.
//!
//! The event tap carries only the registry index; everything here runs on the main thread, where
//! AppKit, the Accessibility API and the config live. This is also the single place where the two
//! cross-cutting rules are applied, so no action can bypass them:
//!
//! * the recorder wins — while the settings recorder is capturing a chord, no binding runs;
//! * an in-flight switcher session is cancelled first, because releasing the switcher modifier
//!   would otherwise commit the card selected before this action's own effect;
//! * a window action never acts on this app's own windows (they are fixed-layout panels whose
//!   constraints would fight the write). It no-ops rather than quietly acting on a background
//!   window.

use std::sync::atomic::Ordering;

use crate::i18n::tf;
use crate::log_debug;

use super::Action;

/// Run one action that crossed the thread bridge, but only while the press is still authorized.
///
/// A press is authorized when it is swallowed; the config can change before the main thread drains
/// the queue (the settings page edits it live) and a queued lock, window move or feature switch must
/// not run after its binding was deleted, its row switched off, its group switched off, or the chord
/// was given to another action. The trigger that fired travels with the action for exactly this
/// check -- no old switch is consulted, only the table the engine is publishing now.
pub(crate) fn execute_authorized(index: u16, trigger: crate::key_bindings::Trigger) {
    let Some(action) = Action::from_index(index) else {
        log_debug!("[keybind] dropped unknown action index {}", index);
        return;
    };
    if !super::engine::trigger_is_live(trigger, action) {
        log_debug!(
            "[keybind] dropped {}: its trigger is no longer bound to it",
            action.config_id()
        );
        return;
    }
    execute(action);
}

pub(crate) fn execute(action: Action) {
    // The recorder is capturing this chord: running its action as well would fight the recording.
    // Checked here, not only in the tap, because an action enqueued just before the recorder opened
    // must not run either.
    if crate::mouse::event_tap::RECORDING.load(Ordering::Relaxed) {
        log_debug!("[keybind] {} ignored while recording", action.config_id());
        return;
    }
    // Whatever the user asked for takes over from an unfinished switcher session -- including a
    // first show that has not been displayed yet, whose late result would otherwise reopen it.
    crate::overlay::reset_switcher();
    // A window action does not act on this app's own windows (they are fixed-layout panels whose
    // constraints would fight the write). The *decision* is taken in the tap, before the press is
    // swallowed, so that a chord like Option+arrow still reaches a focused text field -- arriving
    // here at all means the action was meant to run. Kept as a backstop for an action enqueued
    // before this app became frontmost.
    if action.is_window_scoped() && crate::ffi::frontmost_app_info().1 == std::process::id() as i32
    {
        log_debug!(
            "[keybind] {} skipped: oh-my-tab's own window is frontmost",
            action.config_id()
        );
        return;
    }
    if let Some(dir) = action.snap_direction() {
        crate::window_management::apply_direction(dir);
        return;
    }
    if let Some(dir) = action.display_direction() {
        crate::window_management::apply_display_move(dir);
        return;
    }
    match action {
        Action::Maximize => {
            crate::window_management::apply_direct(crate::window_management::DirectTarget::Maximize)
        }
        Action::Minimize => {
            crate::window_management::apply_direct(crate::window_management::DirectTarget::Minimize)
        }
        Action::Center => {
            crate::window_management::apply_direct(crate::window_management::DirectTarget::Center)
        }
        Action::HalfTop => {
            crate::window_management::apply_direct(crate::window_management::DirectTarget::HalfTop)
        }
        Action::HalfBottom => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::HalfBottom,
        ),
        Action::HalfLeft => {
            crate::window_management::apply_direct(crate::window_management::DirectTarget::HalfLeft)
        }
        Action::HalfRight => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::HalfRight,
        ),
        Action::QuarterTopLeft => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::QuarterTopLeft,
        ),
        Action::QuarterTopRight => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::QuarterTopRight,
        ),
        Action::QuarterBottomLeft => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::QuarterBottomLeft,
        ),
        Action::QuarterBottomRight => crate::window_management::apply_direct(
            crate::window_management::DirectTarget::QuarterBottomRight,
        ),
        Action::OpenSettings => crate::quick_actions::open_system_settings(),
        Action::OpenFinder => crate::quick_actions::open_new_finder_window(),
        Action::LockScreen => crate::quick_actions::lock_screen(),
        Action::LocatePointer => crate::pointer_locator::show(),
        // The four Dock notifications the mouse mappings already fire; "show desktop" is the same
        // behaviour as the quick action of that name, so both share one action identity.
        Action::MissionControl => {
            crate::mouse::system_action::fire("com.apple.expose.awake");
        }
        Action::AppExpose => {
            crate::mouse::system_action::fire("com.apple.expose.front.awake");
        }
        Action::Launchpad => {
            crate::mouse::system_action::fire("com.apple.launchpad.toggle");
        }
        Action::ShowDesktop => {
            crate::mouse::system_action::fire("com.apple.showdesktop.awake");
        }
        Action::PreviousApp => {
            crate::window_collector::activate_previous_app();
        }
        // Feature switches go through the menu's own path: config write, persist, the full apply
        // pass and the UI refreshes, so the runtime, the open settings window and the menu agree.
        Action::ToggleMouse => {
            crate::menu::toggle_service(crate::menu::ServiceToggle::Mouse);
        }
        Action::ToggleKeystrokeDisplay => {
            crate::menu::toggle_service(crate::menu::ServiceToggle::KeystrokeDisplay);
        }
        Action::ToggleClipboardHistory => {
            crate::menu::toggle_service(crate::menu::ServiceToggle::Clipboard);
        }
        // The picker's normal entry point, with its own disabled/unavailable handling: this action
        // opens the panel, it never switches recording on.
        Action::OpenClipboardPanel => crate::clipboard::on_clipboard_toggle(
            std::ptr::null_mut(),
            objc2::sel!(onClipboardToggled:),
            std::ptr::null_mut(),
        ),
        // Handled above through the direction accessors.
        Action::SnapUp
        | Action::SnapDown
        | Action::SnapLeft
        | Action::SnapRight
        | Action::DisplayUp
        | Action::DisplayDown
        | Action::DisplayLeft
        | Action::DisplayRight => {}
    }
}

/// Human-readable action name, for logs and error messages that must not show a config id.
pub(crate) fn action_label(action: Action) -> String {
    crate::i18n::t(action.label_key())
}

/// Message for a rejected chord, with the placeholder the locales expect.
pub(crate) fn reserved_trigger_message(value: &str) -> String {
    tf("errors.key_binding_trigger_reserved", &[("value", value)])
}
