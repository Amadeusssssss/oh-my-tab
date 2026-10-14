//! The binding engine: one session-level event tap that owns every binding-driven global shortcut.
//!
//! It replaces the two taps that `window_management` and `quick_actions` used to run, each with its
//! own hardcoded key set. One tap means one matching decision per press: with user-defined chords
//! the two key sets would overlap, and both taps would act on the same press.
//!
//! The tap thread never reads the config. It resolves a press against an immutable snapshot
//! (`RwLock<Arc<BindingTable>>`, replaced whole on every config change), so the hot path takes one
//! uncontended read lock for the length of an `Arc` clone, then a hash lookup — no allocation, no
//! config lock, and no master/row switch consulted a second time.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::event_monitor::GlobalEvent;
use crate::event_tap::{
    self, keyboard, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent, CGEventGetFlags,
    CGEventGetIntegerValueField, CGEventMask, CGEventRef, CGEventTapProxy, CGEventType,
    K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::key_bindings::{Action, BindingTable};
use crate::mouse::shortcut::Shortcut;
use crate::{log_debug, log_info};

use super::trigger::{Trigger, MODIFIER_MASK};

use keyboard::{
    EVENT_FLAGS_CHANGED as K_CG_EVENT_FLAGS_CHANGED, EVENT_KEY_DOWN as K_CG_EVENT_KEY_DOWN,
    EVENT_KEY_UP as K_CG_EVENT_KEY_UP, FIELD_AUTOREPEAT as K_CG_KEYBOARD_EVENT_AUTOREPEAT,
    FIELD_KEYCODE as K_CG_KEYBOARD_EVENT_KEYCODE, FLAG_COMMAND, FLAG_CONTROL, FLAG_OPTION,
    FLAG_SHIFT,
};

/// A double-tap gesture must land inside this window; the same 350ms the quick actions used for
/// double-Control.
const GESTURE_INTERVAL: Duration = Duration::from_millis(350);

/// The published snapshot. `None` would mean "no table yet"; the engine always holds one, starting
/// empty, so the tap never has to branch on it.
static TABLE: LazyLock<RwLock<Arc<BindingTable>>> =
    LazyLock::new(|| RwLock::new(Arc::new(BindingTable::build(&[], false))));

/// Main keys whose press this engine swallowed, and which therefore stay swallowed to their
/// release.
///
/// Keyed by the physical key alone: the user may release Option before the main key, so the release
/// arrives with a different modifier set and would otherwise stop matching. A repeat or a release
/// of a key that is not in here is passed through rather than turned into an action — an unknown
/// repeat (a tap that restarted mid-hold) must not fire anything.
static SWALLOWED_KEYS: LazyLock<Mutex<Vec<u16>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Per-modifier "is it held" state, so `flagsChanged` can be read as press edges.
static MODIFIER_DOWN: LazyLock<Mutex<HashMap<u32, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Last press time of each modifier that has a gesture bound to it.
static GESTURE_LAST: LazyLock<Mutex<HashMap<u32, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether this app is the frontmost one, published from the main thread.
///
/// The tap runs on its own thread and must not call `NSWorkspace` (main-thread AppKit), so the fact
/// is published here and read as a plain atomic. It exists because a window-scoped action must not be
/// swallowed while the user is typing in our own window: the event has to reach the focused text
/// field. Deciding that on the main thread is too late -- by then the press is already swallowed and
/// the field never sees it.
static SELF_FRONTMOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Publish whether this app is frontmost. Called from the main thread's activation notifications.
pub(crate) fn publish_self_frontmost(frontmost: bool) {
    SELF_FRONTMOST.store(frontmost, Ordering::Relaxed);
}

/// Whether this app is frontmost, as last published. Off-main safe.
fn self_is_frontmost() -> bool {
    SELF_FRONTMOST.load(Ordering::Relaxed)
}

/// The published flag, for the A1 smoke runner: the AppKit activation notifications set it and the
/// tap reads it. The unit tests cover the *rule* the flag feeds; this is what lets a runner assert
/// the wiring they cannot reach.
pub(crate) fn debug_self_frontmost() -> bool {
    self_is_frontmost()
}

/// Drop every pending gesture candidate from `last`.
///
/// A gesture is "this modifier was tapped twice on its own". Any real key press in between breaks
/// that reading: Command tapped, then `C` (`Command+C`), then Command again inside the window is a
/// copy followed by a fresh modifier press, not a double-tap of Command. Without this the first tap
/// stayed on record and the next press fired the gesture action -- a window move, a lock screen or a
/// pointer jump the user never asked for.
///
/// Pure over the caller's map so the rule is testable, and so the key path applies it atomically
/// with everything else it decides. Synthesized events never reach it: the tap filters those before
/// the decision, because our own key injection must not change gesture state.
fn clear_pending_gestures(last: &mut HashMap<u32, Instant>) {
    last.clear();
}

static TAP_CONTROL: event_tap::TapThreadControl = event_tap::TapThreadControl::new();
static ENGINE_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

/// Whether the published table still resolves `trigger` to `action`: a press's authorization,
/// re-checked on the main thread against the config as it is now.
pub(crate) fn trigger_is_live(trigger: Trigger, action: Action) -> bool {
    match TABLE.read() {
        Ok(table) => table_maps(&table, trigger, action),
        Err(_) => false,
    }
}

/// Pure core of the re-check (see `trigger_is_live`).
fn table_maps(table: &BindingTable, trigger: Trigger, action: Action) -> bool {
    match trigger {
        Trigger::Chord(chord) => table.lookup_chord(chord.keycode, chord.flags) == Some(action),
        Trigger::DoubleModifier(flag) => table.lookup_gesture(flag) == Some(action),
    }
}

/// The currently published binding count (live bindings only), for diagnostics and e2e state.
pub(crate) fn live_binding_count() -> usize {
    TABLE.read().map(|table| table.len()).unwrap_or(0)
}

/// Rebuild the snapshot from a config. Called for every config change that may touch the bindings;
/// cheap enough (tens of entries) to run unconditionally.
pub(crate) fn publish(cfg: &Config) {
    let items = cfg.key_bindings.items.as_deref().unwrap_or(&[]);
    let table = BindingTable::build(items, cfg.key_bindings.enabled);
    log_debug!(
        "[keybind] published {} live binding(s) from {} entr(ies)",
        table.len(),
        items.len()
    );
    if let Ok(mut guard) = TABLE.write() {
        *guard = Arc::new(table);
    }
}

/// Whether the master switch is on, i.e. whether the tap has anything to do.
pub(crate) fn enabled_in(cfg: &Config) -> bool {
    cfg.key_bindings.enabled
}

/// Start the tap thread when the master switch is on. Idempotent, and cheap when it is already
/// running.
pub(crate) fn start() {
    if !crate::input_monitor::taps_allowed() {
        return;
    }
    // The permission supervisor works off a config snapshot it cloned earlier, so a start request
    // can arrive after the user switched bindings off. Re-check the live config: booting the tap
    // then would swallow chords for a disabled feature -- UNLESS the panel's own action keys are
    // on, because their claim is established by this tap and would otherwise never run.
    if !tap_is_needed() {
        return;
    }
    let mut guard = match ENGINE_THREAD.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    if guard.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }
    if let Some(finished) = guard.take() {
        let _ = finished.join();
    }
    TAP_CONTROL.prepare_start();
    reset_input_state();
    *guard = Some(spawn_tap_thread());
    log_info!("Key bindings enabled.");
}

/// Whether ANY feature this tap serves still needs it.
///
/// The tap serves two independent features now: the user's key bindings and the overlay's own
/// action keys (Cmd+W/M/H/Q while the panel is open). The panel's chords are claimed by THIS tap,
/// so the tap has to run when either feature is on -- checking only the binding switch left the
/// four panel switches inert (and a claimed Cmd+Q able to reach the app's own Quit item).
///
/// One predicate, used by every lifecycle caller (startup, config change, permission recovery), so
/// the answers cannot drift apart.
pub(crate) fn tap_is_needed() -> bool {
    crate::config::CONFIG
        .read()
        .map(|cfg| enabled_in(&cfg) || crate::panel_actions::configured_in(&cfg))
        .unwrap_or(false)
}

/// Stop the tap thread. Idempotent.
pub(crate) fn stop() {
    TAP_CONTROL.stop();
    let handle = ENGINE_THREAD.lock().ok().and_then(|mut guard| guard.take());
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    reset_input_state();
    log_info!("Key bindings disabled.");
}

/// Forget every in-flight press and gesture. Called when the tap stops, and by the settings
/// recorder around a recording: a press swallowed before the recorder opened must not stay
/// swallowed forever, and two Control presses on either side of a recording must not read as one
/// double-tap.
pub(crate) fn reset_input_state() {
    if let Ok(mut keys) = SWALLOWED_KEYS.lock() {
        keys.clear();
    }
    // The panel's HELD record goes: this runs when the tap STOPS or restarts, so any key-up still
    // outstanding is one this process can no longer receive, and keeping the record would make the
    // next real press look like a repeat and be swallowed.
    //
    // The queued actions are NOT dropped here -- they are cancelled by session generation. A press
    // already released to AppKit must still be consumed when the main thread handles it; deleting
    // the record would make the handler report "not ours" and let a panel-claimed Cmd+Q reach this
    // app's own Quit item.
    crate::panel_actions::clear_held();
    crate::panel_actions::end_session();
    if let Ok(mut modifiers) = MODIFIER_DOWN.lock() {
        modifiers.clear();
    }
    if let Ok(mut gestures) = GESTURE_LAST.lock() {
        gestures.clear();
    }
}

fn spawn_tap_thread() -> thread::JoinHandle<()> {
    // keyDown + keyUp + flagsChanged (the double-tap edge). The switcher and the clipboard chord
    // keep their own taps; the reserved-chord validation is what keeps the three from overlapping.
    let mask: CGEventMask = (1u64 << K_CG_EVENT_KEY_DOWN)
        | (1u64 << K_CG_EVENT_KEY_UP)
        | (1u64 << K_CG_EVENT_FLAGS_CHANGED);
    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        let created = event_tap::create_tap_with_retry(
            tap_location::SESSION_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(binding_tap_callback),
            std::ptr::null_mut(),
            "keybind",
            Some(TAP_CONTROL.cancel_flag()),
        );
        let Some(created) = created else {
            return;
        };
        let rl = CFRunLoopGetCurrent();
        TAP_CONTROL.register(created.tap, rl);
        let watchdog = event_tap::start_tap_watchdog(created.tap, TAP_CONTROL.cancel_flag());
        if !TAP_CONTROL.cancel_flag().load(Ordering::SeqCst) && crate::input_monitor::taps_allowed()
        {
            log_debug!("Key bindings event tap started.");
            event_tap::CFRunLoopRun();
        }
        event_tap::stop_tap_watchdog(watchdog);
        event_tap::CGEventTapEnable(created.tap, false);
        TAP_CONTROL.clear(created.tap);
        event_tap::teardown_event_tap(rl, created);
    })
}

/// Panic boundary: a panic cannot unwind through an `extern "C"` frame, and the fallback passes
/// the event through rather than swallowing it.
unsafe extern "C" fn binding_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    crate::callback_guard::event("binding_tap_callback", event, || unsafe {
        binding_tap_inner(event_type, event)
    })
}

unsafe fn binding_tap_inner(event_type: CGEventType, event: CGEventRef) -> CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "keybind", TAP_CONTROL.is_stopping())
    {
        return event;
    }
    if !crate::input_monitor::taps_allowed() {
        return event;
    }
    // The settings recorder owns the keyboard while a chord is being recorded (including the wait
    // for its release): acting on it would run the very binding the user is trying to capture, and
    // arming a gesture from it would fire on the next tap.
    if crate::mouse::event_tap::RECORDING.load(Ordering::Relaxed) {
        return event;
    }
    match event_type {
        // A gesture is read from modifier state, and the events themselves always pass through:
        // the modifier is also a normal modifier for whatever app is frontmost. Our own synthesized
        // input is ignored here; for key events the decision function below owns that check.
        K_CG_EVENT_FLAGS_CHANGED => {
            if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) != SYNTHETIC_MARKER {
                handle_modifier_edges(event);
            }
            event
        }
        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP => handle_key_event(event_type, event),
        _ => event,
    }
}

/// What one key event is. The tap builds this from the `CGEvent`, and the decisions below are pure
/// so every pass/swallow/dispatch rule has an assertion without a live tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyEvent {
    keycode: u16,
    /// The event's raw flag set; the table masks it down to the four chord modifiers.
    flags: u32,
    /// True for a held-key repeat rather than a fresh press.
    autorepeat: bool,
    kind: KeyKind,
    /// Our own synthesized input (mouse key mappings, the lock-screen chord, the Finder chord).
    synthetic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    Down,
    Up,
}

/// What the tap does with one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyDecision {
    /// Hand the event to the system.
    PassThrough,
    /// Take the event (and, for a press, remember the key until its release).
    Swallow(Option<Action>),
}

/// Decide one key event and update the swallow record.
///
/// The record is keyed by the physical key alone: the user may release Option before the main key,
/// so the release arrives with a different modifier set and would stop matching a chord. A repeat
/// or a release of a key that was never taken is passed through rather than turned into an action —
/// an unknown repeat (a tap that restarted mid-hold) must not fire anything, and a release we never
/// swallowed belongs to the app.
fn decide_key(
    event: KeyEvent,
    table: &BindingTable,
    swallowed: &mut Vec<u16>,
    self_frontmost: bool,
    gestures: &mut HashMap<u32, Instant>,
) -> KeyDecision {
    // Checked before the record is touched: a synthesized keyUp must not clear the record of a real
    // key still being held (and a synthesized press must never dispatch). Our own injected keys also
    // must not disturb gesture state.
    if event.synthetic {
        return KeyDecision::PassThrough;
    }
    // A real key press ends any pending double-tap reading, whoever the key belongs to: Command tap,
    // `Command+C`, Command again is a copy and then a fresh press, not a double-tap. Done here rather
    // than beside the call so the rule travels with the decision it belongs to -- the defect was this
    // call being absent, and only a function that owns both effects can be tested for it.
    clear_pending_gestures(gestures);
    match event.kind {
        KeyKind::Up => {
            // Release of a key the panel claimed: it belonged to the panel for its whole hold, so
            // the release passes through too (and leaves the ordinary record untouched).
            if crate::panel_actions::decide_release(event.keycode) {
                return KeyDecision::PassThrough;
            }
            if let Some(index) = swallowed.iter().position(|held| *held == event.keycode) {
                swallowed.swap_remove(index);
                KeyDecision::Swallow(None)
            } else {
                KeyDecision::PassThrough
            }
        }
        KeyKind::Down if event.autorepeat => {
            // A repeat of a key the panel claimed stays claimed: one press is one action. This is
            // checked before the ordinary swallow record so a binding opened later in the same hold
            // cannot steal repeats back.
            // A repeat either answers for an already-claimed hold (swallow) or is not ours at all;
            // it must never establish a new claim.
            match crate::panel_actions::decide_press(
                event.keycode,
                event.flags,
                swallowed.contains(&event.keycode),
                true,
            ) {
                crate::panel_actions::ClaimDecision::SwallowRepeat => {
                    return KeyDecision::Swallow(None);
                }
                crate::panel_actions::ClaimDecision::PassThrough => {
                    return KeyDecision::PassThrough;
                }
                crate::panel_actions::ClaimDecision::NotClaimed => {}
            }
            if swallowed.contains(&event.keycode) {
                KeyDecision::Swallow(None)
            } else {
                KeyDecision::PassThrough
            }
        }
        KeyKind::Down => {
            // The panel owns the chord while it is open and the matching switch is on: its press
            // belongs to AppKit, not to a binding. `binding_already_swallowed` keeps ownership
            // unambiguous -- a key a binding took first is never reassigned to the panel.
            match crate::panel_actions::decide_press(
                event.keycode,
                event.flags,
                swallowed.contains(&event.keycode),
                false,
            ) {
                crate::panel_actions::ClaimDecision::PassThrough => {
                    return KeyDecision::PassThrough;
                }
                crate::panel_actions::ClaimDecision::SwallowRepeat => {
                    return KeyDecision::Swallow(None);
                }
                crate::panel_actions::ClaimDecision::NotClaimed => {}
            }
            match table.lookup_chord(event.keycode, event.flags) {
                Some(action) => {
                    if swallowed.contains(&event.keycode) {
                        // A second press without a release: treat it as a repeat, but one key never
                        // fires two actions.
                        KeyDecision::Swallow(None)
                    } else if action.is_window_scoped() && self_frontmost {
                        // A window action cannot act on this app's own window, so while our settings
                        // window is frontmost the chord belongs to the focused control: Option+arrow
                        // moves the caret in a text field. Deciding this here rather than on the main
                        // thread is what keeps those editing keys working -- the main thread's skip
                        // happens after the press was already swallowed, and no later handler can give
                        // back an event the app never received.
                        //
                        // Nothing is recorded in `swallowed`, so the matching release passes through
                        // too and the field sees a complete press/release pair.
                        KeyDecision::PassThrough
                    } else {
                        swallowed.push(event.keycode);
                        KeyDecision::Swallow(Some(action))
                    }
                }
                None => KeyDecision::PassThrough,
            }
        }
    }
}

/// keyDown/keyUp handling: decide, then act. The decision is taken once per press and its repeats
/// and release follow it.
unsafe fn handle_key_event(event_type: CGEventType, event: CGEventRef) -> CGEventRef {
    let kind = if event_type == K_CG_EVENT_KEY_UP {
        KeyKind::Up
    } else {
        KeyKind::Down
    };
    let key_event = KeyEvent {
        keycode: CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16,
        flags: CGEventGetFlags(event) as u32,
        autorepeat: CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT) != 0,
        kind,
        synthetic: CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA)
            == SYNTHETIC_MARKER,
    };
    // The table is snapshotted and the guard dropped before anything else runs: nothing below may
    // hold the read side while the main thread takes the write side on a config change.
    let table = match TABLE.read() {
        Ok(table) => Arc::clone(&table),
        Err(_) => return event,
    };
    let frontmost = self_is_frontmost();
    // Both tap state maps are taken together, after the table read has been released: the tap must
    // never hold its read side and either map at once, and the two maps are ordered consistently here
    // (gestures before swallowed) wherever both are touched.
    let decision = match (SWALLOWED_KEYS.lock(), GESTURE_LAST.lock()) {
        (Ok(mut swallowed), Ok(mut gestures)) => {
            decide_key(key_event, &table, &mut swallowed, frontmost, &mut gestures)
        }
        _ => return event,
    };
    match decision {
        KeyDecision::PassThrough => event,
        KeyDecision::Swallow(action) => {
            if let Some(action) = action {
                log_debug!("[keybind] pressed {}", action.config_id());
                crate::enqueue_global_event(GlobalEvent::BindingAction {
                    action: action.index(),
                    trigger: Trigger::Chord(Shortcut {
                        keycode: key_event.keycode,
                        flags: key_event.flags & MODIFIER_MASK,
                    }),
                });
            }
            std::ptr::null_mut()
        }
    }
}

/// Double-tap gestures: a modifier pressed twice inside `GESTURE_INTERVAL` with no other modifier
/// held. Pure over the caller's state maps, so the timing and the disqualifying cases are testable.
///
/// Every `flagsChanged` event passes through untouched: the modifier keeps working as a modifier
/// for whatever app is frontmost.
fn decide_gesture(
    now: Instant,
    flags: u32,
    table: &BindingTable,
    modifier_down: &mut HashMap<u32, bool>,
    last: &mut HashMap<u32, Instant>,
) -> Option<(Action, u32)> {
    if table.gesture_count() == 0 {
        return None;
    }
    // u32, not CGEventFlags: the gesture table is keyed by the chord grammar's modifier bits.
    for flag in [
        FLAG_COMMAND as u32,
        FLAG_OPTION as u32,
        FLAG_CONTROL as u32,
        FLAG_SHIFT as u32,
    ] {
        let down = flags & flag != 0;
        if modifier_down.insert(flag, down).unwrap_or(false) || !down {
            continue;
        }
        // Another modifier being held disqualifies the tap: Command held while Control is tapped
        // twice must not read as a double-Control. Every pending candidate goes, not just this
        // flag's: no gesture can be in flight while a combination is being held, and leaving another
        // modifier's older entry behind would let a stale tap fire later.
        if flags & (MODIFIER_MASK & !flag) != 0 {
            last.clear();
            continue;
        }
        let Some(action) = table.lookup_gesture(flag) else {
            // A lone press of a modifier with no gesture bound likewise ends any reading in progress:
            // the user's taps are not a sequence we are part-way through.
            last.clear();
            continue;
        };
        let is_double = match last.get(&flag).copied() {
            Some(previous) if now.duration_since(previous) <= GESTURE_INTERVAL => {
                last.remove(&flag);
                true
            }
            _ => {
                // At most one candidate is ever pending: a tap of another modifier is over as soon as
                // this one starts, or a stale entry could pair with a much later press.
                last.clear();
                last.insert(flag, now);
                false
            }
        };
        if is_double {
            return Some((action, flag));
        }
    }
    None
}

/// Read the modifier state out of one `flagsChanged` event and run the gesture decision.
unsafe fn handle_modifier_edges(event: CGEventRef) {
    let flags = CGEventGetFlags(event) as u32;
    // Snapshot once and drop the guard: the decision below may enqueue an action.
    let table = match TABLE.read() {
        Ok(table) => Arc::clone(&table),
        Err(_) => return,
    };
    let fired = {
        let Ok(mut modifiers) = MODIFIER_DOWN.lock() else {
            return;
        };
        let Ok(mut last) = GESTURE_LAST.lock() else {
            return;
        };
        decide_gesture(Instant::now(), flags, &table, &mut modifiers, &mut last)
    };
    if let Some((action, gesture_flag)) = fired {
        log_debug!("[keybind] gesture {}", action.config_id());
        crate::enqueue_global_event(GlobalEvent::BindingAction {
            action: action.index(),
            trigger: Trigger::DoubleModifier(gesture_flag),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_bindings::KeyBinding;

    fn item(action: &str, trigger: &str) -> KeyBinding {
        KeyBinding {
            action: action.to_string(),
            trigger: trigger.to_string(),
            enabled: true,
        }
    }

    /// A table with the seeded default bindings, the master switch on.
    fn default_table() -> BindingTable {
        let cfg = Config::default();
        BindingTable::build(cfg.key_bindings.items.as_deref().unwrap_or(&[]), true)
    }

    fn down(keycode: u16, flags: u32) -> KeyEvent {
        KeyEvent {
            keycode,
            flags,
            autorepeat: false,
            kind: KeyKind::Down,
            synthetic: false,
        }
    }

    fn up(keycode: u16, flags: u32) -> KeyEvent {
        KeyEvent {
            keycode,
            flags,
            autorepeat: false,
            kind: KeyKind::Up,
            synthetic: false,
        }
    }

    #[test]
    fn a_matching_press_dispatches_once_and_its_repeats_and_release_are_swallowed() {
        let table = default_table();
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        let option = crate::mouse::shortcut::FLAG_ALT;
        assert_eq!(
            decide_key(
                down(0x0E, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::Swallow(Some(Action::OpenFinder))
        );
        assert_eq!(swallowed, vec![0x0E]);
        // Held key: still ours, but no second action.
        let repeat = KeyEvent {
            autorepeat: true,
            ..down(0x0E, option)
        };
        assert_eq!(
            decide_key(repeat, &table, &mut swallowed, false, &mut gestures),
            KeyDecision::Swallow(None)
        );
        assert_eq!(
            decide_key(
                up(0x0E, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::Swallow(None)
        );
        assert!(swallowed.is_empty(), "the release clears the record");
    }

    /// The reason the record is keyed by the physical key alone: releasing Option first must not
    /// leave the main key swallowed-but-unknown, nor let the release escape to the app.
    #[test]
    fn the_release_is_matched_after_the_modifiers_are_let_go() {
        let table = default_table();
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        let option = crate::mouse::shortcut::FLAG_ALT;
        decide_key(
            down(0x0E, option),
            &table,
            &mut swallowed,
            false,
            &mut gestures,
        );
        // Option released first: the keyUp arrives with no chord modifiers at all.
        assert_eq!(
            decide_key(up(0x0E, 0), &table, &mut swallowed, false, &mut gestures),
            KeyDecision::Swallow(None)
        );
        assert!(swallowed.is_empty());
    }

    #[test]
    fn unrelated_keys_and_unknown_repeats_pass_through() {
        let table = default_table();
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        let option = crate::mouse::shortcut::FLAG_ALT;
        // Not bound.
        assert_eq!(
            decide_key(
                down(0x00, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::PassThrough
        );
        // A repeat with no record (a tap that restarted mid-hold) must not fire anything.
        let stale_repeat = KeyEvent {
            autorepeat: true,
            ..down(0x0E, option)
        };
        assert_eq!(
            decide_key(stale_repeat, &table, &mut swallowed, false, &mut gestures),
            KeyDecision::PassThrough
        );
        // A release we never swallowed belongs to the app.
        assert_eq!(
            decide_key(
                up(0x0E, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::PassThrough
        );
    }

    #[test]
    fn a_second_press_without_a_release_never_fires_twice() {
        let table = default_table();
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        let option = crate::mouse::shortcut::FLAG_ALT;
        assert!(matches!(
            decide_key(
                down(0x0E, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::Swallow(Some(_))
        ));
        assert_eq!(
            decide_key(
                down(0x0E, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::Swallow(None)
        );
    }

    /// The lock-screen action synthesizes Control+Command+Q and marks it. Without the marker the
    /// engine would dispatch whatever that chord is bound to -- including the lock action itself,
    /// which would lock again on every pass.
    #[test]
    fn synthesized_events_neither_dispatch_nor_clear_a_real_press() {
        let table = default_table();
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        let option = crate::mouse::shortcut::FLAG_ALT;
        decide_key(
            down(0x0E, option),
            &table,
            &mut swallowed,
            false,
            &mut gestures,
        );
        let synthetic_up = KeyEvent {
            synthetic: true,
            ..up(0x0E, option)
        };
        assert_eq!(
            decide_key(synthetic_up, &table, &mut swallowed, false, &mut gestures),
            KeyDecision::PassThrough
        );
        assert_eq!(swallowed, vec![0x0E], "the real press is still recorded");

        // And a synthesized press of a bound chord never dispatches.
        let mut empty = Vec::new();
        let synthetic_down = KeyEvent {
            synthetic: true,
            ..down(0x0E, option)
        };
        assert_eq!(
            decide_key(synthetic_down, &table, &mut empty, false, &mut gestures),
            KeyDecision::PassThrough
        );
        assert!(empty.is_empty());
    }

    /// The queue carries the trigger, and the consumer re-checks it: a press queued just before an
    /// edit must not run once its binding is gone, switched off, or given to another action.
    #[test]
    fn a_queued_press_is_re_authorized_against_the_current_table() {
        let chord = Shortcut {
            keycode: 0x0E,
            flags: crate::mouse::shortcut::FLAG_ALT,
        };
        let trigger = Trigger::Chord(chord);

        let live = BindingTable::build(&[item("quick.open_finder", "option+e")], true);
        assert!(table_maps(&live, trigger, Action::OpenFinder));

        // The binding was deleted (or its row switched off, or bindings switched off): empty
        // table.
        let gone = BindingTable::build(&[], false);
        assert!(!table_maps(&gone, trigger, Action::OpenFinder));

        // The chord now runs something else: the queued action must not run.
        let rebound = BindingTable::build(&[item("quick.lock_screen", "option+e")], true);
        assert!(!table_maps(&rebound, trigger, Action::OpenFinder));
        assert!(table_maps(&rebound, trigger, Action::LockScreen));

        // The same action moved to another chord: the action is still live, but not through this
        // trigger, so the queued press is not authorized.
        let rekeyed = BindingTable::build(&[item("quick.open_finder", "option+g")], true);
        assert!(!table_maps(&rekeyed, trigger, Action::OpenFinder));
    }

    /// The gesture form of the same check.
    #[test]
    fn a_queued_gesture_is_re_authorized_too() {
        let control = crate::mouse::shortcut::FLAG_CTRL;
        let trigger = Trigger::DoubleModifier(control);
        let live = BindingTable::build(&[item("quick.locate_pointer", "double-control")], true);
        assert!(table_maps(&live, trigger, Action::LocatePointer));
        let gone = BindingTable::build(&[], false);
        assert!(!table_maps(&gone, trigger, Action::LocatePointer));
    }

    /// A window action's chord passes through while our own window is frontmost.
    ///
    /// The defect: the engine swallowed every matching press and only then skipped the window action
    /// on the main thread, so with the settings window up, Option+arrow neither moved the window nor
    /// reached the focused text field -- caret movement and Shift+Option selection stopped working.
    /// The old tap checked the frontmost app before swallowing; this restores that order.
    ///
    /// The release must pass through too: the field needs a complete press/release pair, and recording
    /// the press in `swallowed` is what would swallow its release.
    #[test]
    fn a_window_action_passes_through_while_own_window_is_frontmost() {
        let table = BindingTable::build(&[item("window.snap_left", "option+left")], true);
        let option = crate::mouse::shortcut::FLAG_ALT;
        let mut gestures = HashMap::new();

        // Our own window is frontmost: the chord belongs to the focused control, so the press passes.
        let mut swallowed = Vec::new();
        assert_eq!(
            decide_key(
                down(0x7B, option),
                &table,
                &mut swallowed,
                true,
                &mut gestures
            ),
            KeyDecision::PassThrough,
            "a window chord must reach the focused control"
        );
        assert!(
            swallowed.is_empty(),
            "nothing is swallowed, so the release passes through too"
        );
        assert_eq!(
            decide_key(
                up(0x7B, option),
                &table,
                &mut swallowed,
                true,
                &mut gestures
            ),
            KeyDecision::PassThrough,
            "the release must not be swallowed either"
        );

        // Another app is frontmost: the same chord is ours and the action fires.
        let mut swallowed = Vec::new();
        assert_eq!(
            decide_key(
                down(0x7B, option),
                &table,
                &mut swallowed,
                false,
                &mut gestures
            ),
            KeyDecision::Swallow(Some(Action::SnapLeft))
        );
        assert_eq!(swallowed, vec![0x7B]);
    }

    /// The frontmost exception covers WINDOW actions only: a quick action is not about the frontmost
    /// window, so it still fires while our own window is up (Option+L must still lock the screen).
    #[test]
    fn only_window_actions_are_exempted_by_our_own_window_being_frontmost() {
        let table = BindingTable::build(&[item("quick.lock_screen", "option+l")], true);
        let option = crate::mouse::shortcut::FLAG_ALT;
        let mut swallowed = Vec::new();
        let mut gestures = HashMap::new();
        assert_eq!(
            decide_key(
                down(0x25, option),
                &table,
                &mut swallowed,
                true,
                &mut gestures
            ),
            KeyDecision::Swallow(Some(Action::LockScreen)),
            "a quick action is not a window action and must still fire"
        );
    }

    /// A real key press between the two taps of a modifier ends the reading.
    ///
    /// The defect: the first tap stayed on record through other keys, so `Command` (tap), `Command+C`
    /// (copy), `Command` again inside the window fired the gesture action -- a window move, a lock
    /// screen or a pointer jump the user never asked for. The gesture is "this modifier was tapped
    /// twice on its own"; a key press in between is what makes that false.
    #[test]
    fn a_real_key_press_clears_a_pending_gesture_candidate() {
        let table = BindingTable::build(
            &[
                item("quick.locate_pointer", "double-control"),
                item("window.snap_left", "option+left"),
            ],
            true,
        );
        let control = FLAG_CONTROL as u32;
        let start = Instant::now();
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        // First tap of Control: a candidate, nothing fires.
        assert_eq!(
            decide_gesture(start, control, &table, &mut modifiers, &mut last),
            None
        );
        assert!(last.contains_key(&control), "the first tap is on record");
        // Release, then a real key press happens (the `C` of a copy). It goes through the production
        // key decision, so a missing invalidation is what this catches -- asserting the rule by hand
        // would pass even with the call removed from the tap path.
        decide_gesture(
            start + Duration::from_millis(10),
            0,
            &table,
            &mut modifiers,
            &mut last,
        );
        let mut swallowed = Vec::new();
        let key_table = BindingTable::build(&[item("window.snap_left", "option+left")], true);
        let plain_key = down(0x08, 0); // `C`, no modifiers
        assert_eq!(
            decide_key(plain_key, &key_table, &mut swallowed, false, &mut last),
            KeyDecision::PassThrough,
            "an unbound key is not swallowed"
        );
        assert!(
            last.is_empty(),
            "the production key path must clear the pending gesture"
        );
        // The next Control press is a FIRST tap again, not a double-tap.
        assert_eq!(
            decide_gesture(
                start + Duration::from_millis(20),
                control,
                &table,
                &mut modifiers,
                &mut last
            ),
            None,
            "a key press between the taps must stop them reading as a double-tap"
        );
    }

    /// Only one gesture candidate can be pending: tapping another modifier ends the previous one.
    ///
    /// The defect: a candidate for one modifier survived while another was tapped, so an unrelated
    /// later press could pair with the stale entry. This pins the invariant on the state map, which
    /// is what the caller holds.
    #[test]
    fn only_one_gesture_candidate_is_ever_pending() {
        let table = BindingTable::build(
            &[
                item("quick.locate_pointer", "double-control"),
                item("quick.lock_screen", "double-command"),
            ],
            true,
        );
        let control = FLAG_CONTROL as u32;
        let command = FLAG_COMMAND as u32;
        let start = Instant::now();
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        decide_gesture(start, control, &table, &mut modifiers, &mut last);
        assert_eq!(last.len(), 1);
        // Release Control, then tap Command: its own candidate replaces the Control one.
        decide_gesture(
            start + Duration::from_millis(5),
            0,
            &table,
            &mut modifiers,
            &mut last,
        );
        decide_gesture(
            start + Duration::from_millis(10),
            command,
            &table,
            &mut modifiers,
            &mut last,
        );
        assert_eq!(
            last.len(),
            1,
            "a stale candidate must not survive another modifier's tap"
        );
        assert!(
            last.contains_key(&command),
            "the pending candidate is the most recent tap"
        );
        // A combination being held ends the reading entirely: no gesture can be mid-flight.
        decide_gesture(
            start + Duration::from_millis(15),
            command | (FLAG_SHIFT as u32),
            &table,
            &mut modifiers,
            &mut last,
        );
        assert!(
            last.is_empty(),
            "holding a combination must leave no pending gesture"
        );
    }

    #[test]
    fn a_gesture_fires_only_on_the_second_press_inside_the_window() {
        let table = BindingTable::build(&[item("quick.locate_pointer", "double-control")], true);
        let control = FLAG_CONTROL as u32;
        let start = Instant::now();
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        assert_eq!(
            decide_gesture(start, control, &table, &mut modifiers, &mut last),
            None
        );
        // A real double-tap has a release in between; a release is never a press edge.
        assert_eq!(
            decide_gesture(
                start + Duration::from_millis(50),
                0,
                &table,
                &mut modifiers,
                &mut last
            ),
            None
        );
        assert_eq!(
            decide_gesture(
                start + Duration::from_millis(100),
                control,
                &table,
                &mut modifiers,
                &mut last
            ),
            Some((Action::LocatePointer, control))
        );
        // Too slow: the history was cleared by the firing, and a third press starts over.
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        decide_gesture(start, control, &table, &mut modifiers, &mut last);
        decide_gesture(
            start + Duration::from_millis(10),
            0,
            &table,
            &mut modifiers,
            &mut last,
        );
        assert_eq!(
            decide_gesture(
                start + GESTURE_INTERVAL + Duration::from_millis(1),
                control,
                &table,
                &mut modifiers,
                &mut last
            ),
            None
        );
    }

    #[test]
    fn a_gesture_needs_the_modifier_alone_and_a_bound_action() {
        let table = BindingTable::build(&[item("quick.locate_pointer", "double-control")], true);
        let control = FLAG_CONTROL as u32;
        let command = FLAG_COMMAND as u32;
        let start = Instant::now();
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        // Command held: two Control taps are not a double-Control.
        for (offset, flags) in [
            (0u64, control | command),
            (25, command),
            (50, control | command),
        ] {
            assert_eq!(
                decide_gesture(
                    start + Duration::from_millis(offset),
                    flags,
                    &table,
                    &mut modifiers,
                    &mut last
                ),
                None
            );
        }
        // Nothing bound to Option: no gesture, and no state kept.
        let mut modifiers = HashMap::new();
        let mut last = HashMap::new();
        let empty = BindingTable::build(&[], false);
        assert_eq!(
            decide_gesture(start, FLAG_OPTION as u32, &empty, &mut modifiers, &mut last),
            None
        );
        assert!(last.is_empty());
    }
}
