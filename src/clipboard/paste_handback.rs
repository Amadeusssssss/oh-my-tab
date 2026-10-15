//! Clipboard subsystem · paste hand-back: sending the synthesized Cmd+V when the panel is pinned.
//!
//! Normally paste hides the panel first and pastes on the same turn: with the panel ordered out, the
//! system's key window returns to the target application and the synthesized Cmd+V lands there
//! (see `detail::synthesize_paste`).
//!
//! A pinned panel must stay on screen (design decision R3), so the keystroke cannot be addressed by
//! "whoever is key after we leave". It is posted to the selected app's process queue instead:
//!
//! - **The target is activated through the app's documented paths.** A nonactivating panel can hold
//!   the keyboard while never being frontmost, so merely waiting would only work when something else
//!   happened to move focus. `activate_pid_with_api` with an explicit `FromApplication` request (the
//!   macOS 14+ cooperative handover) is what the switcher's own raise path uses.
//! - **The request has an identity.** Every arm gets a sequence number and the tick re-reads it, so a
//!   timer left over from a superseded paste can never send for the current one. Cancelling bumps the
//!   sequence, which invalidates every older tick without having to find it.
//! - **Everything is re-checked immediately before the send**: the deadline, request identity,
//!   pasteboard version, enabled state, and that no other external app has taken frontmost status.
//!   The selected process receives the keystroke directly, so an AX focus probe is unnecessary.
//! - **Failure is visible.** A hand-back that cannot be completed tells the user through the panel's
//!   toast rather than pasting into an unknown target.
//!
//! The wait is a bounded, cancellable poll on the main run loop - never a spin, and it activates the
//! target once rather than once per tick.

use super::*;

/// How long the hand-back waits for the activation request to leave any other app frontmost.
/// The same shape as the switcher's cross-desktop activation budget; a starting point, not a
/// measured performance claim.
const PASTE_ACTIVATION_BUDGET: Duration = Duration::from_millis(400);
/// The poll step within that budget.
const PASTE_HAND_BACK_STEP: f64 = 0.02;
/// `NSApplicationActivationPolicyProhibited`: background-only processes cannot receive user input.
const ACTIVATION_POLICY_PROHIBITED: i64 = 2;

/// One in-flight paste.
struct PendingPaste {
    /// The request identity. Bumping this on every arm and cancel invalidates older ticks.
    sequence: u64,
    /// The entry being pasted, compared by the same identity the history uses.
    entry: ClipEntry,
    /// The pasteboard change count the write-back produced; the send requires it to be unchanged.
    pasteboard_version: i64,
    /// The process that receives the targeted keystroke.
    target_pid: i32,
    /// Whether this was an Option+Enter burn-after-paste, which deletes on a successful send.
    burn: bool,
    /// Monotonic deadline.
    deadline: Instant,
}

static PENDING_PASTE: Mutex<Option<PendingPaste>> = Mutex::new(None);
/// The last external app observed before the picker takes key status. A pinned row click can make
/// the nonactivating panel's owning app frontmost before its action callback runs, so deriving the
/// paste target only when the row is clicked can select the picker itself.
static LAST_EXTERNAL_TARGET_PID: Mutex<Option<i32>> = Mutex::new(None);
/// Monotonic request identity, bumped by every arm and every cancel.
static PASTE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Whether a paste hand-back is in flight (asserted by the smoke runner).
pub(crate) fn handback_in_flight() -> bool {
    PENDING_PASTE.lock().unwrap().is_some()
}

/// Remember the external app currently in front. Called before the panel becomes key and whenever
/// it resigns key while pinned, so switching to another app updates the eventual paste destination.
/// If our app is already frontmost, retain the last external target instead of replacing it with
/// the picker process.
pub(super) fn capture_external_target() {
    let (_, pid) = crate::ffi::frontmost_app_info();
    if record_external_target(pid) {
        log_debug!("[clip] pinned paste target captured pid={pid}");
    }
}

/// Track an external app activation while the picker is pinned and visible. Unlike the window's
/// resign-key notification, this observes every switch after the panel has already become non-key.
pub(crate) fn note_external_app_activated(pid: i32, activation_policy: i64) {
    if !is_pinned() || !picker_is_visible() {
        return;
    }
    let mut target = LAST_EXTERNAL_TARGET_PID.lock().unwrap();
    let Some(updated) = external_target_after_activation(
        *target,
        pid,
        std::process::id() as i32,
        activation_policy,
    ) else {
        return;
    };
    *target = Some(updated);
    log_debug!("[clip] pinned paste target updated from app activation pid={pid}");
}

fn record_external_target(pid: i32) -> bool {
    let mut target = LAST_EXTERNAL_TARGET_PID.lock().unwrap();
    let Some(updated) =
        external_target_after_activation(*target, pid, std::process::id() as i32, 0)
    else {
        return false;
    };
    *target = Some(updated);
    true
}

/// Cancel any in-flight hand-back, releasing the burn suppression token. Returns whether there was
/// one.
///
/// Called when a newer paste supersedes this one, when the panel hides, and on teardown or module
/// disable. The entry is never deleted here: a burn only happens on a send that actually succeeded.
/// Bumping the sequence is what makes the cancellation stick - an already-queued tick finds a
/// different (or no) request and does nothing.
pub(super) fn cancel() -> bool {
    PASTE_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let had = PENDING_PASTE.lock().unwrap().take().is_some();
    if had {
        disarm_paste_delete_suppression();
    }
    had
}

/// Arm the hand-back: activate the target, then deliver Cmd+V to that process directly.
///
/// Returns false when there is no usable target, in which case nothing is armed and the caller must
/// tell the user rather than pretending a paste happened.
pub(super) fn begin(entry: &ClipEntry, pasteboard_version: i64, burn: bool) -> bool {
    let (_, current_pid) = crate::ffi::frontmost_app_info();
    let target_pid = preferred_target_pid(
        *LAST_EXTERNAL_TARGET_PID.lock().unwrap(),
        current_pid,
        std::process::id() as i32,
    );
    // Our own process can be frontmost (the settings window); a background or agent process cannot
    // hold an input focus. Neither is a valid paste target, and sending Cmd+V to one pastes nowhere.
    if target_pid <= 0 || target_pid == std::process::id() as i32 {
        log_debug!(
            "[clip] pinned paste: no usable target (pid={}), not sending",
            target_pid
        );
        return false;
    }
    // NOTE: the caller has already cancelled any previous request BEFORE arming this one's burn
    // suppression token. Cancelling here instead would disarm the token this very paste just armed,
    // and the poll could then re-record our own write-back as a fresh copy.
    let sequence = PASTE_SEQUENCE.fetch_add(1, Ordering::SeqCst) + 1;
    *PENDING_PASTE.lock().unwrap() = Some(PendingPaste {
        sequence,
        entry: entry.clone(),
        pasteboard_version,
        target_pid,
        burn,
        deadline: Instant::now() + PASTE_ACTIVATION_BUDGET,
    });
    // A row click can follow an interaction with the search control. Make the non-text list
    // container the panel responder before the hand-back starts, so an early Cmd+V cannot be
    // interpreted as a paste into the query field if AppKit delays the app activation.
    restore_picker_responder();
    // Ask the target to take activation back. Asked ONCE, not per tick: repeating it would fight the
    // user for focus for the whole budget. An explicit FromApplication request selects the macOS 14+
    // cooperative handover, which is the path that actually releases a nonactivating panel's keyboard.
    let request = crate::window_collector::ActivationApiRequest::FromApplication;
    let (activated, api) = crate::window_collector::activate_pid_with_api(target_pid, request);
    log_debug!(
        "[clip] pinned paste: activation requested pid={} api={:?} activated={}",
        target_pid,
        api,
        activated
    );
    schedule_tick(sequence);
    true
}

pub(super) fn restore_picker_responder() -> bool {
    unsafe {
        let window = *PICKER_WINDOW.lock().unwrap();
        let container = *PICKER_CONTAINER.lock().unwrap();
        if let (Some(window), Some(container)) = (window, container) {
            let accepted: bool = msg_send![window.0, makeFirstResponder: container.0];
            let responder: *mut AnyObject = msg_send![window.0, firstResponder];
            return accepted && responder == container.0;
        }
        false
    }
}

/// Prefer the currently frontmost external app, which may have changed while the panel stayed
/// pinned. When clicking the panel has made our own process frontmost, use the most recently
/// captured external app. Pure so app switching during a pinned showing is covered without AppKit.
fn preferred_target_pid(captured: Option<i32>, current: i32, own: i32) -> i32 {
    (current > 0 && current != own)
        .then_some(current)
        .or_else(|| captured.filter(|pid| *pid > 0 && *pid != own))
        .unwrap_or(current)
}

/// Update the remembered target from an application-activation notification, ignoring our own
/// process and invalid PIDs. Pure so consecutive switches remain testable after the panel is already
/// non-key.
fn external_target_after_activation(
    captured: Option<i32>,
    activated: i32,
    own: i32,
    activation_policy: i64,
) -> Option<i32> {
    (activated > 0 && activated != own && activation_policy != ACTIVATION_POLICY_PROHIBITED)
        .then_some(activated)
        .or(captured)
}

/// Schedule the next step, BOUND to the request it belongs to.
///
/// The sequence travels as the perform argument, so a queued callback from an old request identifies
/// itself and is dropped by the entry check. Reading the current slot instead would make an old
/// callback adopt the new request's identity and keep polling for it - two chains for one request.
fn schedule_tick(sequence: u64) {
    unsafe {
        let target = observer();
        let token: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithUnsignedLongLong: sequence];
        let _: () = msg_send![
            target,
            performSelector: sel!(clipboardPasteHandbackTick:),
            withObject: token,
            afterDelay: PASTE_HAND_BACK_STEP
        ];
    }
}

/// One bounded step of the wait: re-check everything, then either send or give up.
pub(super) extern "C" fn paste_handback_tick(_self: *mut c_void, _cmd: Sel, token: *mut c_void) {
    crate::callback_guard::void("paste_handback_tick", || {
        // No argument means this callback is not one of ours.
        let Some(sequence) = read_sequence_token(token) else {
            return;
        };
        match inspect_pending(sequence) {
            TickAction::Wait => schedule_tick(sequence),
            TickAction::Abort => {}
            TickAction::Send {
                entry,
                burn,
                target_pid,
            } => {
                send_now(&entry, burn, Some(target_pid));
            }
        }
    });
}

/// The request identity carried by a scheduled tick, or None when the argument is not one.
fn read_sequence_token(token: *mut c_void) -> Option<u64> {
    if token.is_null() {
        return None;
    }
    let value: u64 = unsafe { msg_send![token as *mut AnyObject, unsignedLongLongValue] };
    Some(value)
}

enum TickAction {
    /// Still inside the budget and another app remains frontmost.
    Wait,
    /// A gate failed, or the deadline passed: nothing is sent.
    Abort,
    /// The selected target process can safely receive the direct event.
    Send {
        entry: ClipEntry,
        burn: bool,
        target_pid: i32,
    },
}

/// Evaluate every send gate exactly once, immediately before the send.
///
/// This runs on the same tick that decides to send, so nothing can slip between the check and the
/// keystroke except the single send itself. The AX queries here can block for up to their own
/// timeout, which is why the request identity is re-read under the lock afterwards: a newer paste
/// that arrived while they ran must take precedence.
fn inspect_pending(sequence: u64) -> TickAction {
    // Read what the gates need WITHOUT holding the lock across the query.
    let (target_pid, version, burn, entry) = {
        let guard = PENDING_PASTE.lock().unwrap();
        match guard.as_ref() {
            // A callback from a superseded request identifies itself here and does nothing: it must
            // not adopt the current request and keep polling on its behalf.
            Some(pending) if pending.sequence == sequence => (
                pending.target_pid,
                pending.pasteboard_version,
                pending.burn,
                pending.entry.clone(),
            ),
            _ => return TickAction::Abort,
        }
    };

    // Check the deadline before other work, so a target that never becomes frontmost cannot keep the
    // poll alive past its budget. This is cheap and runs on the main thread.
    {
        let guard = PENDING_PASTE.lock().unwrap();
        match guard.as_ref() {
            Some(current) if current.sequence == sequence => {
                if Instant::now() >= current.deadline {
                    let mut guard = guard;
                    guard.take();
                    if burn {
                        disarm_paste_delete_suppression();
                    }
                    drop(guard);
                    // A timeout is a failure the user must see: the panel is on screen and the paste
                    // they asked for did not happen. A deliberate cancellation never reaches here.
                    show_toast(&t("clipboard.toast_paste_no_target"));
                    log_debug!("[clip] pinned paste: timed out waiting for target activation");
                    return TickAction::Abort;
                }
            }
            // A newer request owns the slot; this stale tick must not touch it.
            _ => return TickAction::Abort,
        }
    }

    let version_ok = pasteboard_version_is_current(version);
    let enabled = CONFIG.read().unwrap().clipboard.enabled;
    let (_, frontmost_pid) = crate::ffi::frontmost_app_info();
    let own_pid = std::process::id() as i32;

    // Re-check identity after querying the pasteboard and frontmost process: a newer request or a
    // cancellation must win over this tick.
    let mut guard = PENDING_PASTE.lock().unwrap();
    let Some(current) = guard.as_ref() else {
        return TickAction::Abort;
    };
    if current.sequence != sequence {
        return TickAction::Abort;
    }

    // A replaced pasteboard or a disabled module is terminal: the paste can never become deliverable
    // within this request, so it is dropped and its suppression token released.
    if !version_ok || !enabled {
        guard.take();
        if burn {
            disarm_paste_delete_suppression();
        }
        log_debug!("[clip] pinned paste: aborted (version_ok={version_ok} enabled={enabled})");
        return TickAction::Abort;
    }

    // A pinned panel can stay key while the selected app remains active behind it. AX's
    // system-wide focused-element query is not a reliable hand-back signal in that state, and
    // waiting for it caused a visible false timeout. Post Cmd+V to the selected process queue
    // directly; refuse only if a different external app became frontmost after the click.
    if !send_target_still_matches(target_pid, frontmost_pid, own_pid) {
        return TickAction::Wait;
    }
    // Claim the request before releasing the lock, so a second tick cannot also send.
    guard.take();
    TickAction::Send {
        entry,
        burn,
        target_pid,
    }
}

/// Send the keystroke now that every gate passed.
fn send_now(entry: &ClipEntry, burn: bool, target_pid: Option<i32>) -> bool {
    unsafe {
        let pasted = match target_pid {
            Some(pid) => super::detail::synthesize_paste_to_pid(pid),
            None => synthesize_paste(),
        };
        if pasted {
            if let Some(pid) = target_pid {
                log_debug!("[clip] pinned paste: Cmd+V events posted to pid={pid}");
            }
        }
        if pasted && burn {
            delete_burned_entry(entry);
            if clear_system_pasteboard_after_paste() {
                schedule_system_pasteboard_clear();
            }
        } else if burn {
            // The write-back already succeeded; only the keystroke failed. Disarm so the token cannot
            // swallow the next genuine copy, and keep the entry (never lose a record without a paste).
            disarm_paste_delete_suppression();
        }
        if !pasted {
            // The user is looking at the panel, so this is where a failure can be read.
            show_toast(&t("clipboard.toast_paste_no_target"));
        }
        pasted
    }
}

/// Whether the pasteboard still holds what this request wrote.
fn pasteboard_version_is_current(expected: i64) -> bool {
    unsafe {
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return false;
        }
        let current: i64 = msg_send![pasteboard, changeCount];
        current == expected
    }
}

/// A pinned row click can make our own process frontmost. Any other process means the user switched
/// away after the target was captured. Unit-tested to avoid sending into that app.
fn send_target_still_matches(target_pid: i32, current_frontmost_pid: i32, own_pid: i32) -> bool {
    target_pid > 0 && (current_frontmost_pid == target_pid || current_frontmost_pid == own_pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_click_keeps_the_pre_panel_external_target() {
        let own = std::process::id() as i32;
        assert_eq!(preferred_target_pid(Some(42), own, own), 42);
        assert_eq!(preferred_target_pid(None, 42, own), 42);
        assert_eq!(preferred_target_pid(None, own, own), own);
        assert_eq!(preferred_target_pid(Some(own), 42, own), 42);
        assert_eq!(
            preferred_target_pid(Some(42), 43, own),
            43,
            "switching from app A to app B while pinned must paste into B"
        );
        // After the panel has already resigned key, app activations still update the remembered
        // destination: A -> B -> C, then our app becomes frontmost when the row is clicked.
        let target_after_b = external_target_after_activation(Some(42), 43, own, 0);
        let target_after_c = external_target_after_activation(target_after_b, 44, own, 0);
        assert_eq!(target_after_c, Some(44));
        assert_eq!(
            preferred_target_pid(target_after_c, own, own),
            44,
            "a click that makes our app frontmost must use the most recently activated external app"
        );
        assert_eq!(
            external_target_after_activation(Some(42), 45, own, 1),
            Some(45),
            "an accessory app with an input window can become a paste target"
        );
        assert_eq!(
            external_target_after_activation(Some(42), 46, own, ACTIVATION_POLICY_PROHIBITED),
            Some(42),
            "a prohibited background app must not replace the target"
        );
    }

    /// The wrong-app-paste regression: the user switched to another app after choosing the target.
    #[test]
    fn a_target_that_lost_frontmost_to_another_app_is_refused() {
        assert!(
            !send_target_still_matches(1000, 2000, 3000),
            "the current frontmost process is not the request's target: sending would paste into the wrong app"
        );
    }

    /// The happy path: the target is still frontmost when the send is decided.
    #[test]
    fn the_target_still_frontmost_allows_the_send() {
        assert!(send_target_still_matches(1000, 1000, 3000));
        assert!(send_target_still_matches(1000, 3000, 3000));
    }

    /// An unresolvable frontmost process (pid <= 0) is never a match, mirroring begin()'s refusal.
    #[test]
    fn an_unresolvable_frontmost_process_never_matches() {
        assert!(!send_target_still_matches(1000, -1, 3000));
        assert!(!send_target_still_matches(1000, 0, 3000));
    }
}
