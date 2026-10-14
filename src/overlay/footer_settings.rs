//! The switcher overlay's footer settings control.
//!
//! The gear sits in the footer band's trailing corner and opens the settings window on the App
//! Switcher page. It is the same control family as the clipboard picker's corner gear: a borderless
//! glyph button with normal/hover/pressed only (no keyboard focus ring), because the overlay is a
//! nonactivating panel whose keyboard focus belongs to the card container, and the control has no
//! selected or open state to express. See the corner-glyph-button exception in
//! docs/design-style-en.md section 10.
//!
//! Why the click retires the session before showing settings:
//!
//! show_settings_page activates the app and orders the settings window front, which can
//! synchronously resign the overlay panel's key status and re-enter the dismiss path. Doing that
//! while this session is still live would let the resign handler hide the panel mid-click, or let a
//! trigger release commit the highlighted card on the way out. So the click ends the session
//! LOGICALLY first, while holding no borrow, and only then presents settings and performs the
//! visual hide. The trigger release that may follow finds state.visible == false, and is a no-op.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::*;
use crate::ffi::{
    class_addMethod, hex_to_cg_color, layer_set_background, make_nsstring, objc_allocateClassPair,
    objc_registerClassPair, release_obj, CFRelease,
};
use crate::ffi::{MainThreadSlot, StaticClass};

/// The footer gear button (kept for theme repaints and layout, like the status label).
pub(crate) static FOOTER_SETTINGS_BUTTON: MainThreadSlot<Option<ObjPtr>> =
    MainThreadSlot::new(None);

/// Whether a press on the gear is in progress. The click owns the gesture from mouseDown to mouseUp:
/// window input must stay enabled while it runs, and every session-ending path clears it.
static ARMED: AtomicBool = AtomicBool::new(false);

pub(crate) fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

/// Disarm without acting: a press released outside the button, or any session-ending path.
pub(crate) fn disarm() {
    ARMED.store(false, Ordering::SeqCst);
}

pub(crate) fn is_armed() -> bool {
    ARMED.load(Ordering::SeqCst)
}

/// The button class: a borderless glyph button with the neutral hover the corner-glyph exception
/// describes. Deliberately NOT the card close button's class -- that one hardcodes the destructive
/// red hover, which would misread as a dangerous action on a settings control.
unsafe fn footer_settings_button_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabFooterSettingsButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            if cls.is_null() {
                return StaticClass(class!(NSButton));
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                footer_settings_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                footer_settings_exited as *mut c_void,
                types.as_ptr(),
            );
            // The press arms the gesture; the action (dispatched on mouseUp) consumes it. Arming at
            // press is what lets a release outside the button cancel cleanly instead of leaving the
            // session half-ended.
            class_addMethod(
                cls,
                sel!(mouseDown:),
                footer_settings_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

extern "C" fn footer_settings_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    crate::callback_guard::void("footer_settings_mouse_down", || unsafe {
        arm();
        set_footer_settings_style(_self as *mut AnyObject, true);
        // NSButton's own mouseDown: runs the highlight/press tracking loop. Overriding it without
        // calling super replaced that loop entirely, so the button never tracked the press and
        // never dispatched its action -- it looked right and did nothing when clicked.
        type MouseDown = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void);
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let call: MouseDown = std::mem::transmute(objc_msgSendSuper as *const ());
        call(&mut sup, sel!(mouseDown:), event);
    });
}

extern "C" fn footer_settings_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("footer_settings_entered", || unsafe {
        set_footer_settings_style(_self as *mut AnyObject, true);
    });
}

extern "C" fn footer_settings_exited(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("footer_settings_exited", || unsafe {
        // Leaving the button during the press cancels the gesture: the action will still be sent on
        // mouseUp (AppKit tracks the press), so the arm flag is what tells it apart from a real click.
        disarm();
        set_footer_settings_style(_self as *mut AnyObject, false);
    });
}

/// Normal/hover/pressed for the gear: a neutral ink that strengthens under the pointer over one
/// faint wash. It never borrows the close button's destructive red -- that colour says "this
/// destroys something", and opening settings destroys nothing.
pub(crate) unsafe fn set_footer_settings_style(button: *mut AnyObject, hovered: bool) {
    if button.is_null() {
        return;
    }
    let palette = ui_palette();
    let (tint, role) = if hovered {
        (palette.primary_text, crate::glass::PanelInk::Primary)
    } else {
        (palette.secondary_text, crate::glass::PanelInk::Secondary)
    };
    let _: () = msg_send![button, setContentTintColor: crate::glass::panel_ink(tint, role)];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let background = if hovered {
            palette.hover_bg
        } else {
            0x00000000
        };
        layer_set_background(layer, hex_to_cg_color(background));
    }
}

/// The gear's frame inside the panel: the trailing corner of the footer band, the corner-safe inset
/// in from the trailing edge and vertically centred in the band.
pub(crate) fn footer_settings_frame(panel_size: NSSize) -> NSRect {
    let band = crate::theme::footer_band_h();
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    let inset = crate::theme::footer_control_inset(
        radius,
        panel_size.width,
        panel_size.height,
        crate::theme::FOOTER_CONTROL_SIZE,
        band,
    )
    .max(crate::theme::H_PADDING);
    let size = crate::theme::FOOTER_CONTROL_SIZE;
    NSRect::new(
        NSPoint::new(
            (panel_size.width - inset - size).max(0.0),
            ((band - size) / 2.0).max(0.0),
        ),
        NSSize::new(size, size),
    )
}

/// Build the gear button. Returns a +1 reference; the caller adds it to a superview and releases it.
pub(crate) unsafe fn make_footer_settings_button(frame: NSRect) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![footer_settings_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    // An SF Symbol sized by its own configuration: a symbol image is not sized by the button's font.
    let symbol_name = make_nsstring("gearshape");
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_name as *const c_void);
    if !image.is_null() {
        let config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: crate::theme::FONT_CAPTION,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![button, setImage: image];
    }
    let empty = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![button, setTarget: super::cancel::overlay_observer_target()];
    let _: () = msg_send![button, setAction: sel!(openSwitcherSettings:)];
    set_footer_settings_style(button, false);
    let opts: u64 = 0x01 | 0x80; // NSTrackingMouseEnteredAndExited | ActiveAlways
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        options: opts,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
    let label = make_nsstring(&crate::i18n::t("overlay.open_settings"));
    let _: () = msg_send![button, setAccessibilityLabel: label];
    CFRelease(label as *const c_void);
    let tooltip = make_nsstring(&crate::i18n::t("overlay.open_settings"));
    let _: () = msg_send![button, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);
    button
}

/// Refresh the gear's localized strings after a locale change: the window is cached across a locale
/// switch, so its tooltip and accessibility label would otherwise keep the previous language.
pub(crate) fn refresh_localized_strings() {
    let Some(button) = *FOOTER_SETTINGS_BUTTON.lock().unwrap() else {
        return;
    };
    unsafe {
        let label = make_nsstring(&crate::i18n::t("overlay.open_settings"));
        let _: () = msg_send![button.0, setAccessibilityLabel: label];
        CFRelease(label as *const c_void);
        let tooltip = make_nsstring(&crate::i18n::t("overlay.open_settings"));
        let _: () = msg_send![button.0, setToolTip: tooltip];
        CFRelease(tooltip as *const c_void);
    }
}

/// Reposition the gear after the panel resizes and repaint it from the current palette.
pub(crate) fn layout_and_repaint(panel_size: NSSize) {
    let Some(button) = *FOOTER_SETTINGS_BUTTON.lock().unwrap() else {
        return;
    };
    unsafe {
        let _: () = msg_send![button.0, setFrame: footer_settings_frame(panel_size)];
        set_footer_settings_style(button.0, false);
    }
}
