//! Clipboard subsystem · pin: the whole-panel "stays put" state, its frozen height, and the
//! showing generation the deferred dismiss is keyed on.
//!
//! The pin is deliberately NOT an entry pin. The per-entry pin (model::toggle_pin_on) marks one
//! history record; this marks the panel itself: while pinned, losing key must not dismiss it.
//!
//! Lifetime is the process (design decision G): nothing is persisted and there is no config key, so
//! an app restart is the only thing that clears it besides the corner ×. "Hide" and "unpin" are
//! therefore separate operations, and every close path except the × leaves the pin set.

use super::*;

/// The pinned panel's height (the PANEL height, not the padded window height) captured when the pin
/// was pressed. `None` means "not pinned"; the height is frozen so a later row/scroll change cannot
/// resize a panel the user is holding open.
static PIN: Mutex<Option<f64>> = Mutex::new(None);

/// The showing generation: bumped on every show and every hide.
///
/// A deferred dismiss cannot be keyed on "is the panel visible" alone. The counterexample the design
/// review produced: an outside click queues a dismiss, another path hides the panel, the user shows
/// it again unpinned - visibility is true and the pin is false, so the stale callback would close a
/// panel the user just opened. The generation is what makes "this dismiss belongs to the showing it
/// was queued for" expressible; the window pointer cannot, because the window is reused.
static SHOWING: AtomicU64 = AtomicU64::new(0);

/// Whether the panel is pinned right now.
pub(crate) fn is_pinned() -> bool {
    PIN.lock().unwrap().is_some()
}

/// The frozen panel height, when pinned.
pub(crate) fn frozen_height() -> Option<f64> {
    *PIN.lock().unwrap()
}

/// Pin the panel at `height` (the panel rect's height, not the window's). Idempotent: pinning an
/// already-pinned panel keeps the FIRST height, so a second activation cannot silently re-freeze the
/// panel at a height it drifted to.
///
/// Returns whether this call changed the state.
pub(super) fn pin(height: f64) -> bool {
    let mut slot = PIN.lock().unwrap();
    if slot.is_some() {
        return false;
    }
    *slot = Some(height);
    true
}

/// Unpin. This only flips the state and the button; the panel stays visible (design decision C), and
/// restoring the window properties that pinning changed is deferred to the actual hide - restoring
/// collection behavior / hidesOnDeactivate here could move the panel off the current Space, which is
/// exactly the cross-Space case that must survive.
///
/// Returns whether this call changed the state.
pub(super) fn unpin() -> bool {
    PIN.lock().unwrap().take().is_some()
}

/// Clear the pin without touching any window. Used where the panel is genuinely going away for good
/// (the corner ×), never by the generic hide path.
pub(super) fn forget_pin() {
    *PIN.lock().unwrap() = None;
}

/// A queued outside-click dismiss, carrying the showing it belongs to.
///
/// The click arrives on the event-monitor callback, but the dismissal must run on a later turn
/// (orderOut synchronously fires the resign notification). This slot is what crosses that boundary
/// without parking an NSEvent or a raw window pointer in a delayed task.
static PENDING_OUTSIDE_CLICK: Mutex<Option<u64>> = Mutex::new(None);

pub(super) fn set_pending_outside_click(generation: u64) {
    *PENDING_OUTSIDE_CLICK.lock().unwrap() = Some(generation);
}

pub(super) fn take_pending_outside_click() -> Option<u64> {
    PENDING_OUTSIDE_CLICK.lock().unwrap().take()
}

pub(super) fn showing_generation() -> u64 {
    SHOWING.load(Ordering::SeqCst)
}

/// Start a new showing: any dismiss queued against an older generation is now stale.
pub(super) fn begin_showing() -> u64 {
    SHOWING.fetch_add(1, Ordering::SeqCst) + 1
}

/// Whether `generation` is still the showing on screen. A deferred dismiss acts only when this is
/// true, so a callback from a previous showing cannot close a panel opened since.
pub(super) fn showing_is_current(generation: u64) -> bool {
    showing_generation() == generation
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin is process-global, so the state tests run one after another rather than in parallel.
    fn with_clean_pin<R>(f: impl FnOnce() -> R) -> R {
        forget_pin();
        let result = f();
        forget_pin();
        result
    }

    #[test]
    fn pinning_freezes_the_first_height_and_unpinning_clears_it() {
        with_clean_pin(|| {
            assert!(!is_pinned(), "a fresh panel starts unpinned");
            assert_eq!(frozen_height(), None);

            assert!(pin(420.5), "the first pin changes the state");
            assert!(is_pinned());
            assert_eq!(frozen_height(), Some(420.5));

            // A second activation must not re-freeze at a drifted height.
            assert!(!pin(999.0), "pinning an already-pinned panel is a no-op");
            assert_eq!(
                frozen_height(),
                Some(420.5),
                "the height frozen by the first pin is the one that holds"
            );

            assert!(unpin(), "unpinning changes the state");
            assert!(!is_pinned());
            assert_eq!(frozen_height(), None);
            assert!(!unpin(), "unpinning twice is a no-op");
        });
    }

    /// The stale-dismiss counterexample from the design review, as a state assertion: a generation
    /// captured before a hide must not still be current after the next show.
    #[test]
    fn a_generation_from_a_previous_showing_is_stale() {
        let first = begin_showing();
        assert!(showing_is_current(first));

        let second = begin_showing();
        assert!(
            !showing_is_current(first),
            "a dismiss queued for the first showing must not act while the second is on screen"
        );
        assert!(showing_is_current(second));
    }
}
