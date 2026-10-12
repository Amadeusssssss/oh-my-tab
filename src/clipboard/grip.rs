//! Clipboard subsystem · picker drag handle: the three dots on the panel's top edge, and the
//! one-showing position they move the panel to.
//!
//! The handle is deliberately its own view rather than "drag the panel anywhere": the picker is
//! almost entirely buttons (rows, filter pills, the clear actions, the corner buttons), so a
//! background drag would have almost no target and would compete with click-to-paste. A dedicated
//! band keeps every existing click exactly as it was.
//!
//! Scope of the moved position: **one showing**. The anchor lives in `SESSION_ORIGIN`, is cleared
//! by `on_picker_hidden`, and is never written to the configuration, so the next summon puts the
//! panel back where `clipboard.picker_position` asks for it.

use super::*;

/// One in-flight grab of the handle.
///
/// The drag measures the pointer in *screen* coordinates (`NSEvent.mouseLocation`) rather than
/// `locationInWindow`: the window is what moves, so a window-relative point would travel with it
/// and the delta would stay near zero.
#[derive(Clone, Copy, Debug)]
struct GripDrag {
    /// The screen point (AppKit bottom-left origin) the press started at.
    mouse_down: NSPoint,
    /// The picker's panel rect when the press started; the drag is this rect plus the delta.
    picker: NSRect,
    /// The **anchor** at press time: where the picker sits as if no detail panel were open. With the
    /// detail open the live rect is shifted left by the group layout, so the anchor is a different
    /// number -- and it is the anchor, not the live rect, that a finished drag remembers (see
    /// `current_anchor`).
    anchor: NSPoint,
    /// The detail panel's panel rect when the press started, while it is open: the pair is moved
    /// as one group, so `DETAIL_GAP` and the row alignment between them never change.
    detail: Option<NSRect>,
    /// Whether the pointer has travelled past `PICKER_DRAG_THRESHOLD` yet. Until it does, the grab
    /// is a no-op (the handle has no click action, and a sub-threshold wobble must not move the
    /// panel a pixel).
    moved: bool,
}

/// The pointer state the handle owns. `NSCursor` push/pop is a **global stack**, so every push has
/// to be matched exactly once -- including when the panel disappears mid-drag, which is why the
/// state is kept here rather than pushed blind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GripCursor {
    None,
    Open,
    Closed,
}

/// The picker's top-left for the current showing, as if no detail panel were open: the anchor a
/// drag moves, and the origin the detail group layout derives from. `None` means "wherever
/// `clipboard.picker_position` put it".
static SESSION_ORIGIN: Mutex<Option<NSPoint>> = Mutex::new(None);
static DRAG: Mutex<Option<GripDrag>> = Mutex::new(None);
static CURSOR: Mutex<GripCursor> = Mutex::new(GripCursor::None);
/// The handle view: the theme refresh repaints its dots, and hiding it must not leave the cursor pushed.
static GRIP_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

// ---------------------------------------------------------------------------------------------
// Position state
// ---------------------------------------------------------------------------------------------

/// The dragged anchor for this showing, if the user has moved the panel.
pub(super) fn session_origin() -> Option<NSPoint> {
    *SESSION_ORIGIN.lock().unwrap()
}

/// Remember the anchor a drag ended on.
pub(super) fn set_session_origin(origin: NSPoint) {
    *SESSION_ORIGIN.lock().unwrap() = Some(origin);
}

/// The origin to lay the detail group out from: the dragged anchor when there is one, otherwise
/// the live rect's own origin (which is the configured position).
pub(super) fn anchor_origin(live: NSPoint) -> NSPoint {
    session_origin().unwrap_or(live)
}

/// The anchor as it stands before a grab starts. Three cases, in order: the user already moved the
/// panel this showing; else the detail panel is open and the origin its close will restore IS the
/// anchor; else the live rect (the configured position) is.
fn current_anchor(picker: NSRect) -> NSPoint {
    if let Some(origin) = session_origin() {
        return origin;
    }
    if detail_visible() {
        if let Some(origin) = *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() {
            return origin;
        }
    }
    picker.origin
}

/// Forget everything the current showing moved: the anchor a drag ended on and any in-flight grab.
///
/// This is the "one showing" invariant in one place -- a dragged position never outlives the panel
/// that was dragged, and it is never written to the configuration. Split out from
/// `on_picker_hidden` so it can be asserted without touching the cursor stack.
pub(super) fn forget_session_position() {
    *DRAG.lock().unwrap() = None;
    *SESSION_ORIGIN.lock().unwrap() = None;
}

/// Everything the picker must let go of when it hides: the position state above, and the cursor
/// push -- `NSCursor` is a global stack, so a hand pushed when the panel disappears has to be
/// popped by hand (a later `mouseUp` never arrives, and must not write a position either).
pub(super) fn on_picker_hidden() {
    crate::debug_assert_main_thread();
    forget_session_position();
    set_cursor(GripCursor::None);
}

/// The correction a finished drag applies, so a panel the user left half off-screen stays usable.
/// Pure.
///
/// The picker keeps its whole rect inside `visible`. While the detail panel is open the pair is
/// corrected as **one rigid group**, so a picker that is only just inside cannot push the detail
/// off the right edge. When the group is wider than the screen both rules cannot hold; the leading
/// edge wins and the detail overflows, the same degradation `detail_group_frames` uses.
pub(super) fn drag_release_correction(
    picker: NSRect,
    detail: Option<NSRect>,
    visible: NSRect,
) -> NSPoint {
    let min_x = visible.origin.x;
    let max_x = visible.origin.x + visible.size.width;
    let min_y = visible.origin.y;
    let max_y = visible.origin.y + visible.size.height;

    let group_left = detail.map_or(picker.origin.x, |d| picker.origin.x.min(d.origin.x));
    let group_right = detail.map_or(picker.origin.x + picker.size.width, |d| {
        (picker.origin.x + picker.size.width).max(d.origin.x + d.size.width)
    });

    let mut dx = 0.0;
    if group_right > max_x {
        dx = max_x - group_right;
    }
    if group_left + dx < min_x {
        dx = min_x - group_left;
    }
    if detail.is_some() && group_right - group_left > visible.size.width {
        dx = min_x - group_left;
    }

    // The picker is the taller of the two and the detail is laid out inside its vertical span, so
    // correcting the picker's y corrects the pair.
    let mut dy = 0.0;
    if picker.size.height > visible.size.height {
        // A panel taller than the screen cannot be wholly inside it. The handle sits at the panel's TOP
        // edge, and a panel whose handle is off-screen cannot be dragged back -- so the top edge wins and
        // the bottom overflows. The other order (bottom first) put the handle above the display.
        dy = max_y - (picker.origin.y + picker.size.height);
    } else {
        if picker.origin.y + picker.size.height > max_y {
            dy = max_y - (picker.origin.y + picker.size.height);
        }
        if picker.origin.y + dy < min_y {
            dy = min_y - picker.origin.y;
        }
    }
    NSPoint::new(dx, dy)
}

/// Clamp the picker's origin so the panel it belongs to stays inside `visible`: the standalone picker's own
/// frame, with the same edge margin the drag clamp keeps.
///
/// The detail panel's close restores a standalone origin that was saved while a GROUP was on screen. That
/// group is wider than the picker, so after a drag to a narrower display the saved origin can sit past the
/// new screen's right edge even though the group was clamped into it -- restoring it would put the panel
/// (and the handle, and the corner buttons) off-screen. Pure.
pub(super) fn clamp_picker_origin(origin: NSPoint, size: NSSize, visible: NSRect) -> NSPoint {
    let min_x = visible.origin.x + PICKER_EDGE_MARGIN;
    let max_x = visible.origin.x + visible.size.width - PICKER_EDGE_MARGIN;
    let min_y = visible.origin.y + PICKER_EDGE_MARGIN;
    let max_y = visible.origin.y + visible.size.height - PICKER_EDGE_MARGIN;
    let x = if size.width >= visible.size.width {
        min_x
    } else {
        origin.x.max(min_x).min(max_x - size.width)
    };
    let y = if size.height >= visible.size.height {
        // Same rule as the release clamp: the handle is at the panel's top, so the top edge wins.
        max_y - size.height
    } else {
        origin.y.max(min_y).min(max_y - size.height)
    };
    NSPoint::new(x, y)
}

// ---------------------------------------------------------------------------------------------
// The handle view
// ---------------------------------------------------------------------------------------------

/// Build the handle on the header strip and register it for the hide-time cleanup.
pub(super) unsafe fn make_grip_view(header_strip: *mut AnyObject) {
    let view: *mut AnyObject = msg_send![grip_view_class(), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: picker_grip_frame()];
    let _: () = msg_send![view, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000));

    let dot_color = crate::ffi::hex_to_cg_color(clipboard_palette().secondary_text);
    for dot in picker_grip_dot_frames() {
        let mark: *mut AnyObject = msg_send![class!(NSView), alloc];
        let mark: *mut AnyObject = msg_send![mark, initWithFrame: dot];
        let _: () = msg_send![mark, setWantsLayer: true];
        let mark_layer: *mut AnyObject = msg_send![mark, layer];
        // Round, like the keycap chips: the dots are a handle's texture, not three tiny squares.
        let _: () = msg_send![mark_layer, setCornerRadius: (dot.size.height / 2.0)];
        crate::ffi::layer_set_background(mark_layer, dot_color);
        let _: () = msg_send![view, addSubview: mark];
        release_obj(mark);
    }

    // Hover feedback is what tells the user this band is grabbable, so the handle needs its own
    // tracking area. ActiveAlways, like every other tracking area on this panel: its host app
    // stays inactive behind the nonactivating panel, and ActiveInActiveApp delivers nothing.
    let opts: u64 = 0x01 | 0x80; // NSTrackingMouseEnteredAndExited | ActiveAlways
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let bounds: NSRect = msg_send![view, bounds];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: bounds,
        options: opts,
        owner: view,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![view, addTrackingArea: tracking];
    release_obj(tracking);

    // A handle is a drag target, not a button: the accessibility role says so, and the tooltip is
    // where the double-click reset is stated (there is no room for it on the dots themselves).
    // "AXHandle" is NSAccessibilityHandleRole, the role AppKit uses for draggable handles.
    let role = make_nsstring("AXHandle");
    let _: () = msg_send![view, setAccessibilityRole: role];
    CFRelease(role as *const c_void);
    let label = make_nsstring(&t("clipboard.grip_label"));
    let _: () = msg_send![view, setAccessibilityLabel: label];
    CFRelease(label as *const c_void);
    let tooltip = make_nsstring(&t("clipboard.grip_tooltip"));
    let _: () = msg_send![view, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);

    let _: () = msg_send![header_strip, addSubview: view];
    *GRIP_VIEW.lock().unwrap() = Some(ObjPtr::new(view));
    release_obj(view);
}

// ---------------------------------------------------------------------------------------------
// Drag interaction
// ---------------------------------------------------------------------------------------------

extern "C" fn grip_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    crate::callback_guard::void("clipboard_grip_mouse_down", || unsafe {
        let click_count: isize = msg_send![event as *mut AnyObject, clickCount];
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        begin_drag(point, click_count);
    });
}

extern "C" fn grip_mouse_dragged(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("clipboard_grip_mouse_dragged", || {
        let point: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        drag_to(point);
    });
}

extern "C" fn grip_mouse_up(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("clipboard_grip_mouse_up", finish_drag);
}

extern "C" fn grip_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("clipboard_grip_mouse_entered", || {
        set_cursor(GripCursor::Open);
    });
}

extern "C" fn grip_mouse_exited(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("clipboard_grip_mouse_exited", || {
        // A captured drag keeps the closed hand even if AppKit reports the pointer leaving while
        // the window moves under it; popping here would unbalance the cursor stack mid-drag.
        if DRAG.lock().unwrap().is_some() {
            return;
        }
        set_cursor(GripCursor::None);
    });
}

/// First mouse, so a grab works while the panel's host app is inactive -- the same reason the
/// scroll indicator overrides it.
extern "C" fn grip_accepts_first_mouse(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) -> bool {
    true
}

fn begin_drag(point: NSPoint, click_count: isize) {
    crate::debug_assert_main_thread();
    let Some(picker) = picker_panel_rect() else {
        return;
    };
    // Defensive: the handler belongs to the handle, but a stale coordinate must not move a panel
    // the user grabbed elsewhere.
    let Some(band) = grip_screen_rect(picker) else {
        return;
    };
    if !rect_contains_point(band, point) {
        return;
    }
    if click_count >= 2 {
        // Double-click: put the panel back where the configuration asks for it. `reset` recomputes from
        // the mode, forgets this showing's anchor (it owns that state) and re-lays the detail group out
        // if it is open, so both windows move together.
        *DRAG.lock().unwrap() = None;
        super::picker::reset_picker_to_configured_position();
        set_cursor(GripCursor::Open);
        return;
    }
    // A detail closed a moment ago keeps its queued `finishDetailClose:` when the user grabs the handle.
    // Cancelling it here left that window ordered front, transparent and collapsed for good: its logical
    // "visible" flag was already cleared, so nothing would retry. The queued call belongs to a detail the
    // app already considers closed, so letting it finish IS the completion; cancelling a pending close is
    // the OPEN path's job (a quick reopen must not hide the new panel), and it does that itself.
    let detail = if detail_visible() {
        detail_panel_rect()
    } else {
        None
    };
    *DRAG.lock().unwrap() = Some(GripDrag {
        mouse_down: point,
        picker,
        anchor: current_anchor(picker),
        detail,
        moved: false,
    });
    set_cursor(GripCursor::Closed);
}

fn drag_to(point: NSPoint) {
    crate::debug_assert_main_thread();
    let drag = match *DRAG.lock().unwrap() {
        Some(drag) => drag,
        None => return,
    };
    let dx = point.x - drag.mouse_down.x;
    let dy = point.y - drag.mouse_down.y;
    if !drag.moved && dx.hypot(dy) < PICKER_DRAG_THRESHOLD {
        return;
    }
    *DRAG.lock().unwrap() = Some(GripDrag {
        moved: true,
        ..drag
    });
    let moved = |rect: NSRect| {
        NSRect::new(
            NSPoint::new(rect.origin.x + dx, rect.origin.y + dy),
            rect.size,
        )
    };
    apply_frames(Some(moved(drag.picker)), drag.detail.map(moved));
}

fn finish_drag() {
    crate::debug_assert_main_thread();
    let Some(drag) = DRAG.lock().unwrap().take() else {
        return;
    };
    if !drag.moved {
        // A click or a sub-threshold wobble never moved the window, but the press still swapped the cursor
        // to the closed hand: restore it on the way out, or the panel keeps claiming it is being held until
        // the pointer leaves the band.
        finish_cursor_after_release(drag.picker);
        return;
    }
    // The release is where the clamp is applied: while the pointer is held the panel follows it
    // exactly (dragging past an edge is allowed), and letting go is what decides the final spot.
    let Some(picker) = picker_panel_rect() else {
        return;
    };
    let detail = if detail_visible() {
        detail_panel_rect()
    } else {
        None
    };
    let Some(visible) = picker_visible_frame() else {
        // Without a screen there is nothing to clamp into: keep what the drag produced rather than
        // snapping the panel somewhere arbitrary. Still reported, so the state document never
        // disagrees with the window.
        set_session_origin(NSPoint::new(
            drag.anchor.x + (picker.origin.x - drag.picker.origin.x),
            drag.anchor.y + (picker.origin.y - drag.picker.origin.y),
        ));
        crate::e2e_state::record("clipboard_picker_moved");
        return;
    };
    let correction = drag_release_correction(picker, detail, visible);
    // Where the drag actually took the picker, release clamp included. The anchor has to move with it:
    // an anchor that only took the correction stayed at the press-time position, so closing the detail
    // snapped the panel back to where the gesture started, and the next grab read a stale anchor.
    let displacement = NSPoint::new(
        picker.origin.x - drag.picker.origin.x + correction.x,
        picker.origin.y - drag.picker.origin.y + correction.y,
    );
    let corrected_picker = NSRect::new(
        NSPoint::new(
            picker.origin.x + correction.x,
            picker.origin.y + correction.y,
        ),
        picker.size,
    );
    let corrected_detail = detail.map(|d| {
        NSRect::new(
            NSPoint::new(d.origin.x + correction.x, d.origin.y + correction.y),
            d.size,
        )
    });
    apply_frames(Some(corrected_picker), corrected_detail);
    // The anchor is the picker's origin *without* the detail's group shift, and while the detail is
    // open the origin stored at open time is what closing it restores: both move to the corrected
    // place together, so closing the detail cannot snap the panel back to where the drag started.
    let corrected_anchor = NSPoint::new(
        drag.anchor.x + displacement.x,
        drag.anchor.y + displacement.y,
    );
    set_session_origin(corrected_anchor);
    if detail.is_some() {
        *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() = Some(corrected_anchor);
    }
    // The pointer is still where the user let go.
    finish_cursor_after_release(corrected_picker);
    // The moved frame is a fact only the app can state, and the `--e2e-state` document is written on
    // events: without this the A2 drag scenario read the frame from before the gesture and called a
    // working drag a failure. Recorded once, at the end -- the intermediate frames of a gesture are not
    // state anyone reads, and each write collects the whole document.
    crate::e2e_state::record("clipboard_picker_moved");
}

/// Re-apply the handle's localized label and tooltip. The picker window is reused across showings, so a
/// string captured when the handle was built would keep the language that was active then (and VoiceOver
/// would keep announcing it).
pub(super) fn refresh_grip_strings() {
    let Some(view) = *GRIP_VIEW.lock().unwrap() else {
        return;
    };
    unsafe {
        let label = make_nsstring(&t("clipboard.grip_label"));
        let _: () = msg_send![view.0, setAccessibilityLabel: label];
        CFRelease(label as *const c_void);
        let tooltip = make_nsstring(&t("clipboard.grip_tooltip"));
        let _: () = msg_send![view.0, setToolTip: tooltip];
        CFRelease(tooltip as *const c_void);
    }
}

/// Re-apply the material's ink to the handle's dots, and drop any hover state on the way.
///
/// The picker window is reused across showings, so a colour captured when the handle was built survives a
/// light/dark switch and the dots keep the previous mode's ink. The clipboard theme refresh calls this with
/// the gear's restyle: the two header controls it added are the ones it owns.
pub(super) fn refresh_grip_style() {
    let Some(view) = *GRIP_VIEW.lock().unwrap() else {
        return;
    };
    unsafe {
        let dot_color = crate::ffi::hex_to_cg_color(clipboard_palette().secondary_text);
        let marks: *mut AnyObject = msg_send![view.0, subviews];
        let count: usize = msg_send![marks, count];
        for index in 0..count {
            let mark: *mut AnyObject = msg_send![marks, objectAtIndex: index as isize];
            let layer: *mut AnyObject = msg_send![mark, layer];
            crate::ffi::layer_set_background(layer, dot_color);
        }
    }
    if DRAG.lock().unwrap().is_none() {
        set_cursor(GripCursor::None);
    }
}

/// Drive one complete grab of the handle, from `from` to `to`, exactly as the mouse handlers do: the
/// threshold, the window move, the release clamp and the anchor all run for real, on the real windows.
///
/// The GUI smoke runner uses this instead of a synthetic input instrument, because no synthetic drag
/// reaches a nonactivating floating panel from outside the process. What that leaves uncovered -- that
/// AppKit delivers the real event stream to this view -- is asserted by the A2 scenario with a real
/// pointer gesture, so the two tiers split the claim instead of one of them covering neither.
pub(super) fn e2e_drag(from: NSPoint, to: NSPoint) {
    begin_drag(from, 1);
    drag_to(to);
    finish_drag();
}

/// Whether the handle is still holding the closed hand, for the GUI smoke: a press that never crossed
/// the drag threshold must not leave it behind (`finish_drag`'s early return used to skip the restore).
pub(super) fn e2e_cursor_is_closed() -> bool {
    *CURSOR.lock().unwrap() == GripCursor::Closed
}

/// The pointer is still where the user let go: over the band the hand stays open, elsewhere the band is
/// no longer hovered. Shared by both release exits, because the press pushed the closed hand either way.
fn finish_cursor_after_release(picker: NSRect) {
    match grip_screen_rect(picker) {
        Some(band) if rect_contains_point(band, current_cursor_point()) => {
            set_cursor(GripCursor::Open)
        }
        _ => set_cursor(GripCursor::None),
    }
}

// ---------------------------------------------------------------------------------------------
// Window plumbing
// ---------------------------------------------------------------------------------------------

/// Move the picker (and the detail panel, when it is open) with one call. `set_panel_frame` takes
/// panel rects and re-adds the elevation padding, so every rect here is a panel rect, never the
/// padded window frame.
fn apply_frames(picker: Option<NSRect>, detail: Option<NSRect>) {
    unsafe {
        if let (Some(rect), Some(window)) = (picker, picker_window()) {
            crate::glass::set_panel_frame(window, rect, true);
        }
        if let (Some(rect), Some(window)) = (detail, detail_window()) {
            crate::glass::set_panel_frame(window, rect, true);
        }
    }
}

unsafe fn picker_window() -> Option<*mut AnyObject> {
    PICKER_WINDOW.lock().unwrap().map(|w| w.0)
}

unsafe fn detail_window() -> Option<*mut AnyObject> {
    DETAIL_WINDOW.lock().unwrap().map(|w| w.0)
}

/// The picker's panel rect, or `None` before the window exists.
fn picker_panel_rect() -> Option<NSRect> {
    unsafe { picker_window().map(|window| crate::glass::panel_frame_of(window)) }
}

fn detail_panel_rect() -> Option<NSRect> {
    unsafe { detail_window().map(|window| crate::glass::panel_frame_of(window)) }
}

/// The screen the panel is on right now: AppKit assigns the window to the screen it mostly
/// overlaps, so this follows a drag across displays without a second rule to keep in step.
fn picker_visible_frame() -> Option<NSRect> {
    unsafe { picker_window().map(|window| super::picker::picker_screen_frame(window)) }
}

fn current_cursor_point() -> NSPoint {
    unsafe { msg_send![class!(NSEvent), mouseLocation] }
}

/// The handle's hit band in screen coordinates, for the panel rect it currently sits on.
fn grip_screen_rect(picker: NSRect) -> Option<NSRect> {
    let local = picker_grip_frame();
    Some(NSRect::new(
        NSPoint::new(
            picker.origin.x + local.origin.x,
            picker.origin.y + picker.size.height - local.origin.y - local.size.height,
        ),
        local.size,
    ))
}

// ---------------------------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------------------------

fn set_cursor(cursor: GripCursor) {
    let mut current = CURSOR.lock().unwrap();
    if *current == cursor {
        return;
    }
    unsafe {
        if *current != GripCursor::None {
            let _: () = msg_send![class!(NSCursor), pop];
        }
        let object: *mut AnyObject = match cursor {
            GripCursor::Open => msg_send![class!(NSCursor), openHandCursor],
            GripCursor::Closed => msg_send![class!(NSCursor), closedHandCursor],
            GripCursor::None => std::ptr::null_mut(),
        };
        if !object.is_null() {
            let _: () = msg_send![object, push];
        }
    }
    *current = cursor;
}

// ---------------------------------------------------------------------------------------------
// Class
// ---------------------------------------------------------------------------------------------

unsafe fn grip_view_class() -> *mut AnyObject {
    static GRIP_VIEW_CLASS: OnceLock<StaticClass> = OnceLock::new();
    GRIP_VIEW_CLASS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardGripView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            for (selector, implementation) in [
                (sel!(mouseDown:), grip_mouse_down as *mut c_void),
                (sel!(mouseDragged:), grip_mouse_dragged as *mut c_void),
                (sel!(mouseUp:), grip_mouse_up as *mut c_void),
                (sel!(mouseEntered:), grip_mouse_entered as *mut c_void),
                (sel!(mouseExited:), grip_mouse_exited as *mut c_void),
            ] {
                class_addMethod(cls, selector, implementation, types.as_ptr());
            }
            let types_first_mouse = CString::new("B@:@").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstMouse:),
                grip_accepts_first_mouse as *mut c_void,
                types_first_mouse.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// The screen a normal display offers, minus the dock/menu bar.
    fn screen() -> NSRect {
        rect(0.0, 0.0, 1440.0, 860.0)
    }

    #[test]
    fn a_panel_inside_the_screen_is_left_alone() {
        let picker = rect(400.0, 300.0, 560.0, 500.0);
        let correction = drag_release_correction(picker, None, screen());
        assert_eq!(correction, NSPoint::new(0.0, 0.0));
    }

    /// The property that matters: whatever the drag produced, the picker ends up whole on screen.
    #[test]
    fn the_released_picker_always_ends_up_fully_inside() {
        let screen = screen();
        for (x, y) in [
            (-200.0, 300.0),
            (1200.0, 300.0),
            (400.0, 700.0),
            (400.0, -300.0),
            (-9999.0, -9999.0),
            (9999.0, 9999.0),
        ] {
            let picker = rect(x, y, 560.0, 500.0);
            let correction = drag_release_correction(picker, None, screen);
            let moved = NSRect::new(
                NSPoint::new(x + correction.x, y + correction.y),
                picker.size,
            );
            assert!(moved.origin.x >= screen.origin.x, "x for {x},{y}");
            assert!(
                moved.origin.x + moved.size.width <= screen.origin.x + screen.size.width,
                "right edge for {x},{y}"
            );
            assert!(moved.origin.y >= screen.origin.y, "y for {x},{y}");
            assert!(
                moved.origin.y + moved.size.height <= screen.origin.y + screen.size.height,
                "top edge for {x},{y}"
            );
        }
    }

    /// With the detail open the pair is one group: a picker that just fits must not leave the
    /// detail hanging off the right edge.
    #[test]
    fn an_open_detail_is_corrected_with_the_picker_as_one_group() {
        let screen = screen();
        let picker = rect(900.0, 300.0, 560.0, 500.0); // right edge 1460 > 1440
        let detail = rect(1476.0, 300.0, 480.0, 500.0);
        let correction = drag_release_correction(picker, Some(detail), screen);
        let moved_picker = NSRect::new(
            NSPoint::new(
                picker.origin.x + correction.x,
                picker.origin.y + correction.y,
            ),
            picker.size,
        );
        let moved_detail = NSRect::new(
            NSPoint::new(
                detail.origin.x + correction.x,
                detail.origin.y + correction.y,
            ),
            detail.size,
        );
        assert_eq!(moved_detail.origin.x + moved_detail.size.width, 1440.0);
        assert!(moved_picker.origin.x >= 0.0);
        // The pair stays rigid: the gap the group layout established survives the correction.
        assert_eq!(
            moved_detail.origin.x - (moved_picker.origin.x + moved_picker.size.width),
            detail.origin.x - (picker.origin.x + picker.size.width)
        );
    }

    /// A group wider than the display cannot satisfy both edges; the leading edge wins and the
    /// picker -- which holds the handle -- stays whole.
    #[test]
    fn a_group_wider_than_the_screen_pins_the_picker_to_the_leading_edge() {
        let screen = rect(0.0, 0.0, 900.0, 860.0);
        let picker = rect(-120.0, 300.0, 560.0, 500.0);
        let detail = rect(456.0, 300.0, 480.0, 500.0);
        let correction = drag_release_correction(picker, Some(detail), screen);
        assert_eq!(picker.origin.x + correction.x, 0.0);
    }

    /// A panel taller than the target screen cannot fit, and the choice that matters is which edge keeps
    /// its place: the handle is at the panel's TOP, and a handle off-screen cannot be grabbed again. The
    /// earlier order aligned the bottom and left the handle above the display.
    #[test]
    fn a_panel_taller_than_the_screen_keeps_its_handle_reachable() {
        let screen = rect(0.0, 0.0, 1440.0, 700.0);
        let picker = rect(400.0, 500.0, 560.0, 900.0);
        let correction = drag_release_correction(picker, None, screen);
        let moved = NSRect::new(
            NSPoint::new(
                picker.origin.x + correction.x,
                picker.origin.y + correction.y,
            ),
            picker.size,
        );
        // The top edge (the handle end, in AppKit coordinates) sits on the visible frame's top edge.
        assert_eq!(moved.origin.y + moved.size.height, 700.0);
        // And it is reachable: the band is far enough below the top edge to be inside the display.
        let band_top_from_panel_top = picker_grip_frame().size.height;
        assert!(moved.origin.y + moved.size.height - band_top_from_panel_top >= 0.0);
        // The bottom is what overflows, not the top.
        assert!(moved.origin.y < 0.0);
    }

    /// The review's own case: a group laid out on a wide display leaves the standalone picker's saved origin
    /// far to the right (620pt of group shift in their numbers). Dragged onto a narrow display, the group is
    /// clamped to the leading edge, and closing the detail must NOT restore the picker to an x that puts it
    /// off that display -- which is what the unclamped origin would do.
    #[test]
    fn a_restored_picker_is_clamped_into_the_screen_it_is_on() {
        let narrow = rect(0.0, 0.0, 1000.0, 860.0);
        let unclamped = NSPoint::new(628.0, 300.0);
        let size = NSSize::new(560.0, 500.0);
        let clamped = clamp_picker_origin(unclamped, size, narrow);
        assert!(
            clamped.x + size.width <= 1000.0 - PICKER_EDGE_MARGIN + 0.001,
            "the restored picker must stay inside the screen (x={}, right={})",
            clamped.x,
            clamped.x + size.width
        );
        assert!(clamped.x >= PICKER_EDGE_MARGIN);
        assert_eq!(
            clamped.y, unclamped.y,
            "a y that already fits is left alone"
        );
        // An origin that fits is untouched, so the ordinary path is not shifted by the clamp.
        let inside = NSPoint::new(200.0, 300.0);
        assert_eq!(clamp_picker_origin(inside, size, narrow), inside);
    }

    #[test]
    fn the_dragged_anchor_is_used_until_the_showing_hides() {
        // One test, not two: the anchor is process-global state, and two tests setting and clearing it
        // race each other under the harness's parallel threads (the failure that produced this shape).
        let origin = NSPoint::new(120.0, 340.0);
        set_session_origin(origin);
        assert_eq!(session_origin(), Some(origin));
        // The detail group layout derives from the anchor, not from wherever the picker happens to be.
        assert_eq!(anchor_origin(NSPoint::new(500.0, 500.0)), origin);

        // The position a drag produced must not outlive the showing: a hidden picker forgets it, so the
        // next summon goes back to the configured mode and nothing reaches the configuration file.
        *DRAG.lock().unwrap() = Some(GripDrag {
            mouse_down: NSPoint::new(0.0, 0.0),
            picker: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
            anchor: NSPoint::new(0.0, 0.0),
            detail: None,
            moved: true,
        });
        forget_session_position();
        assert_eq!(session_origin(), None);
        assert!(DRAG.lock().unwrap().is_none());
        // And the group layout therefore falls back to the live origin again.
        assert_eq!(
            anchor_origin(NSPoint::new(700.0, 100.0)),
            NSPoint::new(700.0, 100.0)
        );
    }

    /// The handle's hit band is what §6.1 asks of a control, and the dots stay inside it AND centered
    /// in it. The dot frames are measured from the BAND's top-left (they are its subviews), so they are
    /// compared against the band's size, not its position in the strip. "Inside" alone is not enough:
    /// the dots used to be placed from a fixed 48pt constant, so widening the band would have left them
    /// at the left end while still being inside it.
    #[test]
    fn the_grip_band_spans_the_top_bar_and_centers_its_dots() {
        let band = picker_grip_frame();
        assert!(band.size.width >= 28.0 && band.size.height >= 28.0);
        // The whole top bar, flush with both edges of the panel, inside the header's top padding.
        assert_eq!(band.origin.x, 0.0);
        assert_eq!(band.size.width, PICKER_W);
        assert_eq!(band.origin.y, 0.0);
        assert_eq!(band.size.height, TOP_PAD_Y);
        let dots = picker_grip_dot_frames();
        for (index, dot) in dots.iter().enumerate() {
            assert!(dot.origin.x >= 0.0 && dot.origin.y >= 0.0);
            assert!(
                dot.origin.x + dot.size.width <= band.size.width,
                "dot {index} must stay inside the band horizontally"
            );
            assert!(
                dot.origin.y + dot.size.height <= band.size.height,
                "dot {index} must stay inside the band vertically"
            );
        }
        // The middle dot sits on the band's center line, and the three run left to right without
        // touching: a group that is off-center in the band would fail this.
        let middle = &dots[1];
        assert_eq!(
            middle.origin.x + middle.size.width / 2.0,
            band.size.width / 2.0
        );
        assert_eq!(
            middle.origin.y + middle.size.height / 2.0,
            band.size.height / 2.0
        );
        assert!(dots[0].origin.x + dots[0].size.width < dots[1].origin.x);
        assert!(dots[1].origin.x + dots[1].size.width < dots[2].origin.x);
    }
}
