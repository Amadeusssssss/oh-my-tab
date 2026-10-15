//! The clipboard subsystem's smoke mode: every `--smoke-*` launch runs in it, so a smoke
//! process never reads the developer's keychain or touches their real history. History file,
//! image cache and purge markers are redirected to a process-scoped temporary directory.

use super::*;

/// Smoke mode (every `--smoke-*` launch): the history file and the image cache are redirected
/// to a process-scoped temporary directory and the master key is the fixed test key, never the
/// login keychain (`cfg!(test)` is off in the real binary, so this is the only isolation
/// available there).
pub(super) static SMOKE_MODE: AtomicBool = AtomicBool::new(false);

/// Enable smoke mode (called at the top of `run` for every `--smoke-*` launch, before anything
/// can read the config, the storage or the keychain).
pub(crate) fn set_smoke_mode() {
    SMOKE_MODE.store(true, Ordering::SeqCst);
}

/// --smoke-clipboard entry (called on the main thread): inject two entries, then show/hide
/// the picker twice to exercise rebuild_rows' row-cleanup path -- the site of a double-release
/// UAF that once segfaulted on the second summon. Returns true on success; a crash is a failure.
pub(crate) fn smoke_runner() -> bool {
    // An 8x8 solid PNG shared by the image entry and the lazy detail preview. Deliberately
    // NOT the 1x1 transparent PNG: that one fails TIFFRepresentation re-encoding
    // (CGImageDestinationFinalize), so the detail-preview generate-and-cache branch would
    // never be exercised.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x4B,
        0x6D, 0x29, 0xDC, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x38,
        0xA1, 0xA1, 0x81, 0x15, 0x31, 0x0C, 0x2D, 0x09, 0x00, 0x82, 0x5D, 0x46, 0x01, 0x6A, 0x8D,
        0x16, 0x6B, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let tiny_hash = fnv1a64(TINY_PNG);
    {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        // Inject 12 entries: more than the visible rows (10), covering the scroll-document
        // (NSScrollView) path.
        for i in 0..12 {
            record_text(
                &mut hist,
                &format!("smoke entry {i:02}"),
                "Ghostty",
                "com.mitchellh.ghostty",
                50,
            );
        }
        record_text(
            &mut hist,
            "apple pie recipe",
            "Safari",
            "com.apple.Safari",
            50,
        );
        record_text(&mut hist, "banana bread", "Chrome", "com.google.Chrome", 50);
        // A source-less entry: the header shows "unknown source", no icon.
        record_text(&mut hist, "legacy entry without a source", "", "", 50);
        // Long code next to the newest image means Down after opening the image reaches a
        // genuinely overflowing soft-wrapped detail, covering custom wrapping, full TextKit
        // layout, the native scroller, top reset, and rubber-band state at both endpoints.
        record_text(
            &mut hist,
            &"fn detail_scrolling_regression() { let result = service.fetch(first_argument, second_argument) && enabled; }\n".repeat(200),
            "TextEdit",
            "com.apple.TextEdit",
            50,
        );
        // An image entry: written into the cache dir and referenced, covering the thumbnail
        // render/cleanup paths.
        let _ = cache_write_image(tiny_hash, TINY_PNG);
        let tiny = ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: tiny_hash,
            data_path: clip_image_path(tiny_hash),
            preview_png: Arc::new(TINY_PNG.to_vec()),
            source_path: None,
        };
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        // Re-copying the same image: the content-hash dedup keeps the history from growing
        // (exercises image dedup).
        let before = hist.len();
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        assert_eq!(hist.len(), before, "same image must dedup");
    }
    show_picker();
    hide_picker();
    // Second show: rebuild_rows removes the old rows first (the former UAF path).
    show_picker();
    // The panel's outline is a decoration over the whole panel, so it has two ways to be wrong that no
    // token comparison would catch: absent (or present) against its own switch, and swallowing the
    // panel's input. Both are asserted here on the real view tree, including the `--panel-outline=off`
    // launch, where "no outline" must be what the switch produced rather than a silent install failure.
    unsafe {
        let backdrop = (*PICKER_BACKDROP.lock().unwrap())
            .expect("the picker's backdrop exists once the picker has been shown");
        let expected = crate::glass::panel_outline_enabled();
        match crate::glass::outline_state(&backdrop) {
            Some((width, colour_matches)) => {
                assert!(
                    expected,
                    "an outline was installed while the switch asked for none"
                );
                assert!(
                    (width - crate::theme::PANEL_OUTLINE_WIDTH).abs() < 1e-9,
                    "the outline's stroke width {width} is not the PANEL_OUTLINE_WIDTH token"
                );
                assert!(
                    colour_matches,
                    "the outline is not drawn in the card_border token"
                );
            }
            None => assert!(
                !expected,
                "the switch asked for an outline, but none was installed"
            ),
        }
        assert!(
            !crate::glass::outline_intercepts(&backdrop, crate::glass::outline_centre(&backdrop)),
            "the outline decoration must never be the hit-test result: it would swallow every click on the panel"
        );
        // The outline's colour must be the palette token, not whatever the layer happened to hold: the
        // theme's `card_border` differs per mode, so a stale value is a visibly wrong stroke. Only checked
        // when the outline is expected -- `--panel-outline=off` is a legitimate launch, and requiring a view
        // there would fail the very smoke that verifies the switch.
        if crate::glass::panel_outline_enabled() {
            let token = crate::theme::ui_palette().card_border;
            let expected = crate::ffi::hex_to_cg_color(token);
            let outline = crate::glass::outline_view_for_smoke(&backdrop)
                .expect("the outline view exists while the switch is on");
            let layer: *mut AnyObject = msg_send![outline, layer];
            assert!(
                !layer.is_null()
                    && crate::ffi::CGColorEqualToColor(
                        crate::ffi::layer_border_color(layer),
                        expected as *const c_void
                    ),
                "the outline must be drawn in the current theme's card_border"
            );
        }

        // The elevation shadow's silhouette has to enclose the *panel*: a path over the padded window would
        // put the shadow's darkest part in the padding, and a path never re-set after a resize would leave
        // the shadow the size of the previous panel. Both were wrong at least once while this was built, and
        // neither is visible in a token comparison, so both are asserted on the real geometry.
        //
        // Presence is asserted against the *effective* elevation rather than skipped when absent: a panel
        // that should have a shadow and has no path is exactly the silent-pass this check exists to stop.
        let shadow = crate::glass::carrier_shadow_path_rect(&backdrop);
        let expects_shadow = crate::glass::effective_panel_elevation_id() != "none";
        assert_eq!(
            shadow.is_some(),
            expects_shadow,
            "the picker's elevation is {} but its shadow path {} installed",
            crate::glass::effective_panel_elevation_id(),
            if shadow.is_some() { "is" } else { "is not" }
        );
        if let Some(shadow) = shadow {
            let window = (*PICKER_WINDOW.lock().unwrap())
                .expect("the picker window exists once the picker has been shown")
                .0;
            let panel = crate::glass::panel_frame_of(window);
            let insets = crate::glass::panel_insets_of(window);
            let padded = crate::theme::window_frame_for_panel(panel, insets);
            assert!(
                (shadow.w - panel.size.width).abs() < 1.0
                    && (shadow.h - panel.size.height).abs() < 1.0,
                "the shadow path must enclose the panel ({}x{}), not the padded window ({}x{})",
                panel.size.width,
                panel.size.height,
                padded.size.width,
                padded.size.height
            );
            // The panel sits inside the window by the padding, so the path must be offset by it: a path at
            // the window's origin would be the "expanded twice" mistake.
            assert!(
                (shadow.x - insets.left).abs() < 1.0 && (shadow.y - insets.bottom).abs() < 1.0,
                "the shadow path must sit at the panel's offset inside the window ({}, {}), not at {}",
                insets.left,
                insets.bottom,
                shadow.x
            );
        }
    }
    assert!(
        unsafe { detail::smoke_row_backdrop_paint_paths() },
        "selected+hovered row backdrops must match in the build and runtime repaint paths"
    );
    // An active detail icon is a filled chip, so its glyph has to be readable *against its own
    // fill*. Measured on the pixels rather than on the tokens, because the failure was a wrong
    // role: the fill was `primary_text` while the glyph stayed `accent_text`, which is 13.91:1 in
    // light mode but 1.09:1 in dark mode, where the icon became a blank white disc.
    unsafe {
        assert!(
            active_detail_icon_glyph_is_legible_on_its_fill(),
            "an active detail icon must draw its glyph on its own fill"
        );
    }

    /// The clear actions must show the scope the current filter, query and "separate clear all"
    /// setting describe, in the locale in effect, and each button's width must hug its CURRENT
    /// title. Runs on the main thread: the state lives behind `MainThreadSlot`s. Every path that
    /// can change the scope, the setting or the locale has to leave this true.
    unsafe fn assert_clear_actions_match_scope() {
        let applied =
            || super::text_style::clear_action_applied().expect("the clear actions applied");
        let state = applied();
        let (clear, clear_all, widths, show_all) = (
            state.clear.clone(),
            state.clear_all.clone(),
            state.widths,
            state.show_all,
        );
        let expected = super::clip_clear_labels(
            *super::CLIP_FILTER.lock().unwrap(),
            super::with_clipboard_ui(|ui| !ui.search_query.is_empty()),
        );
        assert_eq!(
            (clear, clear_all),
            expected,
            "the clear actions must name the scope they clear"
        );
        let titles: Vec<String> = {
            let buttons = (*CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap())
                .expect("persistent clear action buttons must be built");
            buttons
                .iter()
                .map(|button| {
                    let title: *mut AnyObject = msg_send![button.0, title];
                    crate::ffi::nsstring_to_rust(title)
                })
                .collect()
        };
        assert_eq!(
            titles,
            vec![expected.0.clone(), expected.1.clone()],
            "the buttons must show the scope they clear"
        );
        // The width hugs the CURRENT title: measured text plus the shared click slack — not the
        // widest variant (the old reservation left a wide pill around the short "clear").
        let measured: [f64; 2] = [
            super::localized_string_width(&titles[0], crate::theme::FONT_CAPTION),
            super::localized_string_width(&titles[1], crate::theme::FONT_CAPTION),
        ]
        .map(|width| width + super::CLEAR_ACTION_CLICK_SLACK);
        assert!(
            widths[0] == measured[0] && widths[1] == measured[1],
            "widths {widths:?} must hug the current titles {measured:?}"
        );
        // The second button's visibility is the setting's decision; the hidden button parks with a
        // zero-size frame while the plain clear keeps its full text-hugging width.
        let buttons = (*CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap()).unwrap();
        let hidden: bool = msg_send![buttons[1].0, isHidden];
        assert_eq!(
            !hidden, show_all,
            "the clear-all button's visibility must follow the setting"
        );
        let frames: [NSRect; 2] = buttons.map(|button| msg_send![button.0, frame]);
        assert!(
            (frames[0].size.width - widths[0]).abs() < 0.5,
            "the plain clear must keep its text-hugging width in both visibility states"
        );
        if show_all {
            assert!(
                frames[1].size.width >= widths[1],
                "the visible clear-all button must fit its title"
            );
        }
        // The hover tracking area keeps the rect it was created with, so every resize must have
        // rebuilt it: after the initial layout AND after any label change it must cover the
        // button's current bounds (the width used to stay at the 1pt build placeholder). A
        // tracking rect lives in the button's own coordinate system, so compare it to bounds.
        for button in buttons.iter() {
            let bounds: NSRect = msg_send![button.0, bounds];
            let areas: *mut AnyObject = msg_send![button.0, trackingAreas];
            let count: usize = msg_send![areas, count];
            let covered = (0..count).any(|i| {
                let ta: *mut AnyObject = msg_send![areas, objectAtIndex: i];
                let rect: NSRect = msg_send![ta, rect];
                (rect.size.width - bounds.size.width).abs() < 0.5
                    && (rect.size.height - bounds.size.height).abs() < 0.5
            });
            assert!(
                covered,
                "a tracking area must cover the clear button's current bounds {bounds:?} (found {count})"
            );
        }
    }

    /// The handle view inside the header strip, found by its class name (the theme check needs it).
    unsafe fn grip_view_in_header() -> *mut AnyObject {
        let header = (*super::PICKER_HEADER_STRIP.lock().unwrap())
            .expect("the picker header strip must be built")
            .0;
        let subviews: *mut AnyObject = msg_send![header, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
            let class: *mut AnyObject = msg_send![view, class];
            let name = crate::ffi::nsstring_to_rust(msg_send![class, description]);
            if name == "OhMyTabClipboardGripView" {
                return view;
            }
        }
        std::ptr::null_mut()
    }

    /// The header controls' translated strings must follow a locale switch: the picker window is reused, so
    /// a label captured at build time keeps the language that was active when it was built (and VoiceOver
    /// keeps announcing it). Reads the live properties back, which is also what an assistive client sees.
    unsafe fn assert_header_strings_are_localized() {
        let grip = grip_view_in_header();
        assert!(!grip.is_null(), "the handle view must be in the header");
        let label: *mut AnyObject = msg_send![grip, accessibilityLabel];
        assert!(
            !label.is_null(),
            "the handle must carry an accessibility label"
        );
        assert_eq!(
            crate::ffi::nsstring_to_rust(label),
            super::t("clipboard.grip_label"),
            "the handle's accessibility label must follow the locale"
        );
        let tooltip: *mut AnyObject = msg_send![grip, toolTip];
        assert!(!tooltip.is_null(), "the handle must carry a tooltip");
        assert_eq!(
            crate::ffi::nsstring_to_rust(tooltip),
            super::t("clipboard.grip_tooltip"),
            "the handle's tooltip must follow the locale"
        );
        if let Some(button) = *super::PICKER_SETTINGS_BUTTON.lock().unwrap() {
            let label: *mut AnyObject = msg_send![button.0, accessibilityLabel];
            assert!(
                !label.is_null(),
                "the settings button must carry an accessibility label"
            );
            assert_eq!(
                crate::ffi::nsstring_to_rust(label),
                super::t("clipboard.open_settings"),
                "the settings button's accessibility label must follow the locale"
            );
            let tooltip: *mut AnyObject = msg_send![button.0, toolTip];
            assert!(
                !tooltip.is_null(),
                "the settings button must carry a tooltip"
            );
            assert_eq!(
                crate::ffi::nsstring_to_rust(tooltip),
                super::t("clipboard.open_settings"),
                "the settings button's tooltip must follow the locale"
            );
        }
    }

    /// The pin's STATE must be observable, not just its existence: the button is the panel's only
    /// stateful corner control, so the smoke proves that pinning and unpinning really change what it
    /// shows (glyph) and what it announces (accessibility label), and that a theme/locale refresh
    /// repaints it from the live value rather than leaving a stale state.
    unsafe fn assert_pin_button_reflects_state() {
        let Some(button) = *super::PICKER_PIN_BUTTON.lock().unwrap() else {
            panic!("the header must carry a pin button");
        };
        super::forget_pin();
        notifications::refresh_pin_button();
        let outline_label = crate::ffi::nsstring_to_rust(msg_send![button.0, accessibilityLabel]);
        assert_eq!(
            outline_label,
            super::t("clipboard.pin_off"),
            "an unpinned button must announce the unpinned state"
        );
        super::pin(400.0);
        notifications::refresh_pin_button();
        let filled_label = crate::ffi::nsstring_to_rust(msg_send![button.0, accessibilityLabel]);
        assert_eq!(
            filled_label,
            super::t("clipboard.pin_on"),
            "a pinned button must announce the pinned state, not only an action name"
        );
        assert_ne!(
            outline_label, filled_label,
            "the two states must be distinguishable to assistive clients"
        );
        // A locale/theme refresh must repaint the live state, not reset it.
        crate::ui_coordinator::apply_theme_and_locale_refresh();
        assert!(super::is_pinned(), "a refresh must not clear the pin");
        let after_refresh = crate::ffi::nsstring_to_rust(msg_send![button.0, accessibilityLabel]);
        assert_eq!(
            after_refresh,
            super::t("clipboard.pin_on"),
            "a refresh must repaint the pinned state"
        );
        // Unpinning must be a pure state flip: the panel is meant to STAY on screen until the next
        // outside click, so the properties that pinning changed must not be restored here (doing so
        // can move a cross-Space panel or make it vanish). The regression this pins down is the
        // "apply the properties in the unpin branch" defect.
        let window = (*super::PICKER_WINDOW.lock().unwrap())
            .expect("the picker window must exist")
            .0;
        // Apply the pinned properties, then drive the REAL button action to unpin: calling unpin()
        // directly would bypass whatever the action itself does, which is exactly where the
        // "restore in the unpin branch" defect lived. The still-visible panel must keep the pinned
        // properties, because restoring them at unpin can move a cross-Space panel or make it vanish.
        notifications::apply_pin_window_properties();
        let pinned_hides: bool = msg_send![window, hidesOnDeactivate];
        assert!(!pinned_hides, "a pinned panel must not hide on deactivate");
        // Fire the action IMP directly: the action signature is -(void)togglePanelPin:(id)sender,
        // and performSelector: would pass a Sel where an object belongs (that threw
        // NSInvalidArgumentException). methodForSelector gives the IMP; calling it with the button as
        // the sender is exactly what a click does.
        // methodForSelector returns an unsafe function pointer ('^v'), not an object ('^?'); cast
        // through the raw runtime instead of objc2's checked msg_send!.
        extern "C" {
            fn class_getInstanceMethod(cls: *mut c_void, sel: Sel) -> *mut c_void;
            fn method_getImplementation(method: *mut c_void) -> *mut c_void;
            fn object_getClass(obj: *mut c_void) -> *mut c_void;
        }
        // The action lives on the TARGET class (the clipboard observer), not on the button class, so
        // look the IMP up there and call it with the button as the sender - exactly what a click
        // through the target/action machinery does.
        let target: *mut AnyObject = msg_send![button.0, target];
        assert!(!target.is_null(), "the pin button must have a target");
        let method = class_getInstanceMethod(
            object_getClass(target as *mut c_void),
            sel!(togglePanelPin:),
        );
        assert!(
            !method.is_null(),
            "the target class must implement togglePanelPin:"
        );
        let imp = method_getImplementation(method);
        assert!(!imp.is_null(), "the method must have an implementation");
        let call: unsafe extern "C" fn(*mut c_void, Sel, *mut c_void) = std::mem::transmute(imp);
        call(
            target as *mut c_void,
            sel!(togglePanelPin:),
            button.0 as *mut c_void,
        );
        assert!(!super::is_pinned(), "the button action must unpin");
        let after_unpin: bool = msg_send![window, hidesOnDeactivate];
        assert!(
            !after_unpin,
            "unpinning must not restore the window properties while the panel is still visible"
        );

        // The unpin -> FIRST detail open path: a detail created now must inherit what the picker is
        // actually carrying (the pinned properties, retained by design), not recompute from the pin -
        // recomputing was the second half of the same defect.
        super::rebuild_rows();
        show_detail_for_sel();
        assert!(
            super::detail_visible(),
            "the detail must open for this check"
        );
        if let Some(detail) = (*super::DETAIL_WINDOW.lock().unwrap()).map(|w| w.0) {
            let detail_hides: bool = msg_send![detail, hidesOnDeactivate];
            assert_eq!(
                detail_hides, after_unpin,
                "a detail opened after unpin must inherit the picker's retained properties, not the defaults"
            );
        }
        // Close the detail again; the smoke continues from a quiet panel.
        hide_detail();

        // Only the actual hide restores the properties.
        notifications::apply_pin_window_properties_for_hidden_panel();
        let hides: bool = msg_send![window, hidesOnDeactivate];
        assert!(hides, "hiding the panel restores hidesOnDeactivate");
        super::forget_pin();
        notifications::refresh_pin_button();
    }

    /// A pinned paste must not be sent blindly: the hand-back arms, and cancellations (a newer paste,
    /// the panel hiding) must clear it so a stale request can never paste later. Asserted on state,
    /// because the actual keystroke delivery is A2 territory.
    unsafe fn assert_paste_handback_state_is_bounded() {
        assert!(
            !super::handback_in_flight(),
            "no paste hand-back may be armed outside a pinned paste"
        );
        // Cancelling an unarmed hand-back is a no-op and must not panic or leak.
        super::paste_handback::cancel();
        assert!(!super::handback_in_flight());
        // The target PID is fixed when the request starts and the Cmd+V event is posted directly
        // to that process. The pure target gate allows our own PID while the pinned panel is active,
        // but rejects a different frontmost app; the unit tests cover both decisions. What is
        // assertable here is that cancellation leaves no stale request armed.
        assert!(
            !super::handback_in_flight(),
            "cancel must clear the request"
        );

        // A row click must not leave an edited search field as first responder while a
        // process-targeted Cmd+V is being delivered.
        let window = (*super::PICKER_WINDOW.lock().unwrap())
            .expect("the picker window must exist")
            .0;
        let search = (*super::SEARCH_FIELD.lock().unwrap())
            .expect("the search field must exist")
            .0;
        let _: bool = msg_send![window, makeFirstResponder: search];
        assert!(
            super::paste_handback::restore_picker_responder(),
            "the pinned paste hand-back must restore the list responder after row selection"
        );
    }

    /// Option+V must recover when AppKit has hidden the panel but the intent flag stayed true.
    /// This constructs that exact stale state and invokes the same callback the event bridge uses.
    unsafe fn assert_shortcut_reopens_appkit_hidden_panel() {
        let originally_enabled = CONFIG.read().unwrap().clipboard.enabled;
        CONFIG.write().unwrap().clipboard.enabled = true;
        super::forget_pin();
        show_picker();
        assert_pinned_picker_glass_tracks_key_state();
        let window = (*super::PICKER_WINDOW.lock().unwrap())
            .expect("the picker window must exist")
            .0;
        let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        // Simulate an AppKit hide that did not reach our resign-key handler: the window is gone,
        // while this flag still contains the last showing's intent.
        super::PICKER_VISIBLE.store(true, Ordering::SeqCst);
        assert!(
            !super::picker_is_visible(),
            "the setup must leave the actual panel hidden"
        );
        super::on_clipboard_toggle(
            std::ptr::null_mut(),
            sel!(onClipboardToggled:),
            std::ptr::null_mut(),
        );
        assert!(
            super::picker_is_visible(),
            "Option+V must reopen a hidden panel even when the intent flag is stale"
        );
        let hides_on_deactivate: bool = msg_send![window, hidesOnDeactivate];
        assert!(
            !hides_on_deactivate,
            "a visible unpinned panel must stay under the explicit dismiss handlers instead of AppKit hiding it silently"
        );
        hide_picker();
        CONFIG.write().unwrap().clipboard.enabled = originally_enabled;
    }

    /// Liquid Glass's passive appearance is compensated only while a pinned picker has resigned
    /// key; returning key restores the configured tint unchanged.
    unsafe fn assert_pinned_picker_glass_tracks_key_state() {
        let original_material = CONFIG.read().unwrap().appearance.panel_material.clone();
        CONFIG.write().unwrap().appearance.panel_material = "liquid-glass".into();
        crate::clipboard::apply_backdrop_material();
        let glass_supported = objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_some();
        let effective_material = crate::glass::PanelMaterial::effective();
        let skipped_reason = if !glass_supported {
            Some("NSGlassEffectView is unavailable on this macOS version")
        } else if crate::theme::reduce_transparency_enabled() {
            Some("Reduce Transparency correctly selected the opaque fallback")
        } else if crate::dev_flags::value("panel-material").is_some()
            && effective_material != crate::glass::PanelMaterial::LiquidGlass
        {
            Some("the launch material override selected a non-glass surface")
        } else {
            None
        };
        if let Some(reason) = skipped_reason {
            crate::log_info!("[smoke-clipboard] passive Liquid Glass check skipped: {reason}");
            CONFIG.write().unwrap().appearance.panel_material = original_material;
            crate::clipboard::apply_backdrop_material();
            return;
        }
        assert_eq!(
            effective_material,
            crate::glass::PanelMaterial::LiquidGlass,
            "Glass is available and enabled, so the smoke must exercise its installed compensation layer"
        );
        let layer = (*super::PICKER_BACKDROP.lock().unwrap())
            .and_then(|backdrop| backdrop.compensation_layer)
            .expect("Liquid Glass must install its passive compensation layer");
        let window = (*super::PICKER_WINDOW.lock().unwrap())
            .expect("the picker window must exist")
            .0;
        assert!(super::pin(500.0), "the smoke must exercise a pinned panel");
        notifications::apply_pin_window_properties();

        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let resigned: *mut AnyObject = make_nsstring("NSWindowDidResignKeyNotification");
        let _: () = msg_send![center, postNotificationName: resigned, object: window];
        CFRelease(resigned as *const c_void);
        let visible_after_resign: bool = msg_send![window, isVisible];
        assert!(
            visible_after_resign,
            "a pinned picker must remain visible after resigning key"
        );
        let inactive_color = crate::ffi::layer_background_color(layer.0);
        assert!(!inactive_color.is_null());
        assert_eq!(
            crate::ffi::CGColorGetAlpha(inactive_color as *const c_void),
            crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA as f64 / 255.0,
            "a pinned picker that resigns key must apply the established passive-glass compensation"
        );

        let became_key: *mut AnyObject = make_nsstring("NSWindowDidBecomeKeyNotification");
        let _: () = msg_send![center, postNotificationName: became_key, object: window];
        CFRelease(became_key as *const c_void);
        let visible_after_become_key: bool = msg_send![window, isVisible];
        assert!(visible_after_become_key);
        let active_color = crate::ffi::layer_background_color(layer.0);
        assert!(!active_color.is_null());
        assert_eq!(
            crate::ffi::CGColorGetAlpha(active_color as *const c_void),
            0.0,
            "regaining key must remove passive-glass compensation"
        );
        super::forget_pin();
        notifications::apply_pin_window_properties();
        CONFIG.write().unwrap().appearance.panel_material = original_material;
        crate::clipboard::apply_backdrop_material();
    }

    /// The header's two new controls must exist in the LIVE view tree, not merely as geometry in a
    /// pure function: the handle the A2 drag scenario grabs, and a settings button wired to the
    /// action that opens the settings page. Both are found by class/action, so a rename that
    /// silently left the header without them fails here.
    unsafe fn assert_header_controls_are_live() {
        let header = (*super::PICKER_HEADER_STRIP.lock().unwrap())
            .expect("the picker header strip must be built")
            .0;
        let (search, pin, gear, close) = super::picker_header_frames();
        let grip = super::picker_grip_frame();

        let subviews: *mut AnyObject = msg_send![header, subviews];
        let count: usize = msg_send![subviews, count];
        let mut found_grip = false;
        let mut found_settings: *mut AnyObject = std::ptr::null_mut();
        let mut found_close = false;
        for index in 0..count {
            let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
            let class: *mut AnyObject = msg_send![view, class];
            let name: *mut AnyObject = msg_send![class, description];
            let name = crate::ffi::nsstring_to_rust(name);
            if name == "OhMyTabClipboardGripView" {
                found_grip = true;
                let frame: NSRect = msg_send![view, frame];
                assert_eq!(
                    frame, grip,
                    "the handle must sit on the band the geometry names"
                );
                // A handle with no tracking area never shows the hand cursor, and one whose frame
                // drifts from its tracking rect stops responding where it is drawn.
                let areas: *mut AnyObject = msg_send![view, trackingAreas];
                let area_count: usize = msg_send![areas, count];
                let covered = (0..area_count).any(|i| {
                    let area: *mut AnyObject = msg_send![areas, objectAtIndex: i as isize];
                    let rect: NSRect = msg_send![area, rect];
                    (rect.size.width - grip.size.width).abs() < 0.5
                        && (rect.size.height - grip.size.height).abs() < 0.5
                });
                assert!(covered, "the handle needs a tracking area over its band");
            }
            let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
            if !is_button {
                continue;
            }
            let action: Sel = msg_send![view, action];
            if action == sel!(openClipboardSettings:) {
                found_settings = view;
            } else if action == sel!(pickerClose:) {
                found_close = true;
            }
        }
        assert!(found_grip, "the header must carry the drag handle");
        assert!(found_close, "the header must still carry the close button");
        assert!(
            !found_settings.is_null(),
            "the header's settings button must exist and target openClipboardSettings:"
        );
        // The pin is the corner row's third glyph. It is found by ACTION, so a rename that left the
        // header without a working pin fails here, and its frame is checked against the geometry
        // that no-overlap proof is written against.
        let found_pin = (0..count).any(|index| {
            let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
            // Only buttons answer action; the grip is a plain NSView, and asking it would be a
            // "method not found" crash exactly like the loop above avoids.
            let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
            if !is_button {
                return false;
            }
            let action: Sel = msg_send![view, action];
            if action != sel!(togglePanelPin:) {
                return false;
            }
            let frame: NSRect = msg_send![view, frame];
            assert_eq!(
                frame, pin,
                "the pin button must sit in the frame the geometry reserves for it"
            );
            let label: *mut AnyObject = msg_send![view, accessibilityLabel];
            assert!(
                !label.is_null(),
                "the pin must state its state to assistive clients, not just show a glyph"
            );
            true
        });
        assert!(
            found_pin,
            "the header must carry a pin button targeting togglePanelPin:"
        );
        let gear_frame: NSRect = msg_send![found_settings, frame];
        assert_eq!(
            gear_frame, gear,
            "the settings button must sit in the frame the geometry reserves for it"
        );
        // The three controls share the search row and may not overlap: the search field gives up
        // both buttons' width, and the button frames are the ones the geometry names.
        assert!(search.origin.x + search.size.width <= gear.origin.x);
        assert!(gear.origin.x + gear.size.width <= close.origin.x);
        let settings_hidden: bool = msg_send![found_settings, isHidden];
        assert!(!settings_hidden, "the settings button must be visible");
    }

    // Footer shortcut legends must survive the fonts they are drawn with, in every shipped
    // locale: a label whose frame was sized for a smaller font than the one it renders with
    // wraps out of its one-line-high field and silently loses its tail.
    unsafe {
        assert!(
            footer_legends_layout_is_sane(),
            "clipboard footer legends must render fully"
        );
        let original_locale = CONFIG.read().unwrap().i18n.locale.clone();
        // A query is active so the scope wording (the widest variant) is what must survive.
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        update_clear_action_labels();
        for locale in ["en", "zh-Hans", "zh-Hant"] {
            crate::i18n::apply_config_locale(locale);
            refresh_localized_ui();
            assert!(
                footer_legends_layout_is_sane(),
                "clipboard footer legends must render fully in locale={locale}"
            );
            assert_header_strings_are_localized();
            // A locale refresh re-measures the reservation and re-applies the current scope; it
            // used to reset both to the two short labels.
            assert_clear_actions_match_scope();
        }
        super::with_clipboard_ui(|ui| ui.search_query.clear());
        crate::i18n::apply_config_locale(&original_locale);
        refresh_localized_ui();
    }
    // Delete/undo GUI smoke: delete the top image through the real list-focus path, then use
    // Cmd+Z to restore it; its cache must survive the undo window without duplication.
    unsafe {
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            set_picker_selection(0);
            rebuild_rows();
            let original = {
                let hist = CLIP_HISTORY.lock().unwrap();
                hist.iter().find(|entry| entry.image.is_some()).cloned()
            }
            .expect("clipboard smoke must contain an image entry");
            let hash = original.image.as_ref().unwrap().hash;
            let ev_delete = make_key_event(51);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_delete as *mut c_void);
            assert!(
                !CLIP_HISTORY
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| same_clip_entry_identity(entry, &original)),
                "backspace must remove the selected clipboard entry"
            );
            let ev_undo = make_key_event_with_modifiers(6, 0x0010_0000);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_undo as *mut c_void);
            let hist = CLIP_HISTORY.lock().unwrap();
            assert_eq!(
                hist.iter()
                    .filter(|entry| same_clip_entry_identity(entry, &original))
                    .count(),
                1,
                "Cmd+Z must restore exactly one entry"
            );
            assert!(
                cache_read_image(hash).is_some(),
                "undo must retain image bytes"
            );
        }
    }
    // Clear-entry GUI smoke: the plain clear is always visible; the opt-in "clear all" appears
    // beside it only with the setting on, and both remain hit-testable in either state. Clear
    // scopes are covered by pure logic tests so this smoke retains all fixtures for the
    // subsequent detail path.
    unsafe {
        let buttons = (*CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap())
            .expect("persistent clear action buttons must be built");
        // Hit-test from the window's content view, with the point converted to window
        // coordinates. (The panel's elevation-outline work nested the content parent inside a
        // padded container, so hit-testing `parent` directly with parent-local coordinates
        // silently tested a point shifted by that container's origin.)
        let content: *mut AnyObject = {
            let win: *mut AnyObject = msg_send![buttons[0].0, window];
            msg_send![win, contentView]
        };
        let is_hit_testable = |button: *mut AnyObject| -> bool {
            let bounds: NSRect = msg_send![button, bounds];
            let in_window: NSRect = msg_send![button, convertRect: bounds, toView: std::ptr::null_mut::<objc2::runtime::AnyObject>()];
            let center = NSPoint::new(
                in_window.origin.x + in_window.size.width / 2.0,
                in_window.origin.y + in_window.size.height / 2.0,
            );
            let hit: *mut AnyObject = msg_send![content, hitTest: center];
            hit == button
        };
        let original_separate = CONFIG.read().unwrap().clipboard.separate_clear_all;
        // Setting off (the default): only the plain clear is visible and hit-testable; the
        // clear-all button is hidden with a zero-size frame so it can never catch a click.
        CONFIG.write().unwrap().clipboard.separate_clear_all = false;
        update_clear_action_labels();
        let frame0: NSRect = msg_send![buttons[0].0, frame];
        let hidden1: bool = msg_send![buttons[1].0, isHidden];
        assert!(
            hidden1,
            "clear-all button must hide while the setting is off"
        );
        assert!(
            frame0.size.width > 0.0,
            "the plain clear must keep a real frame while the setting is off"
        );
        assert!(
            is_hit_testable(buttons[0].0),
            "the plain clear must remain directly hit-testable (frame0={frame0:?})"
        );
        // Setting on: both buttons visible side by side (plain clear left, clear-all right) with
        // the shared gap, and both hit-testable.
        CONFIG.write().unwrap().clipboard.separate_clear_all = true;
        update_clear_action_labels();
        let frames: [NSRect; 2] = buttons.map(|button| msg_send![button.0, frame]);
        assert!(frames[0].origin.x < frames[1].origin.x);
        assert_eq!(frames[0].origin.y, frames[1].origin.y);
        assert_eq!(
            frames[1].origin.x - (frames[0].origin.x + frames[0].size.width),
            super::CLEAR_ACTION_GAP
        );
        for button in buttons {
            assert!(
                is_hit_testable(button.0),
                "clear action button must remain directly hit-testable"
            );
        }
        CONFIG.write().unwrap().clipboard.separate_clear_all = original_separate;
        update_clear_action_labels();
        let pills: Vec<*mut AnyObject> = FILTER_PILLS
            .lock()
            .unwrap()
            .iter()
            .map(|pill| pill.0)
            .collect();
        assert!(
            pills.iter().all(|pill| {
                let hidden: bool = msg_send![*pill, isHidden];
                !hidden
            }),
            "filter tabs must remain visible beside clear actions"
        );
    }
    // Clear-action scope smoke: a typed query renames the plain clear to the "current" wording
    // (and the opt-in clear-all to "all results"); clearing the query through the shared path
    // (the × key, the field's Esc, a fresh summon) restores the category wording. Clearing the
    // query by hand in the list-focus Esc branch used to leave a stale scope on the buttons.
    unsafe {
        assert_header_controls_are_live();
        assert_pin_button_reflects_state();
        assert_paste_handback_state_is_bounded();
        assert_shortcut_reopens_appkit_hidden_panel();
        // The handle's dots must follow a theme switch: the picker window is reused across showings, so an
        // ink captured when the handle was built kept the previous mode's colour. This drives the real
        // refresh (the coordinator the settings switch runs) and reads the layer the dots are painted with.
        {
            let grip = grip_view_in_header();
            assert!(!grip.is_null(), "the handle view must be in the header");
            let first_dot_color = |grip: *mut AnyObject| -> Vec<f64> {
                let marks: *mut AnyObject = msg_send![grip, subviews];
                let count: usize = msg_send![marks, count];
                assert!(count >= 3, "the handle must hold its three dots");
                let mark: *mut AnyObject = msg_send![marks, objectAtIndex: 0isize];
                let layer: *mut AnyObject = msg_send![mark, layer];
                let color = crate::ffi::layer_background_color(layer);
                assert!(!color.is_null(), "a dot must be painted");
                let number = crate::ffi::CGColorGetNumberOfComponents(color as *const c_void);
                let components = crate::ffi::CGColorGetComponents(color as *const c_void);
                (0..number).map(|i| *components.add(i)).collect()
            };
            let original_theme = CONFIG.read().unwrap().appearance.theme.clone();
            CONFIG.write().unwrap().appearance.theme = "light".to_string();
            crate::ui_coordinator::apply_theme_and_locale_refresh();
            let light = first_dot_color(grip);
            CONFIG.write().unwrap().appearance.theme = "dark".to_string();
            crate::ui_coordinator::apply_theme_and_locale_refresh();
            let dark = first_dot_color(grip);
            assert!(
                light != dark,
                "the handle's dots must be repainted on a theme switch (light {light:?} vs dark {dark:?})"
            );
            CONFIG.write().unwrap().appearance.theme = original_theme;
            crate::ui_coordinator::apply_theme_and_locale_refresh();
            // A theme/locale/material refresh repaints the picker in place; it must NOT read as (or
            // cause) a pin change. A pinned panel that silently lost its pin on a light/dark switch
            // would change behaviour because of a visual setting (design review, round 2).
            super::pin(500.0);
            crate::ui_coordinator::apply_theme_and_locale_refresh();
            assert!(
                super::is_pinned() && super::frozen_height() == Some(500.0),
                "a theme/locale refresh must preserve the pin and its frozen height"
            );
            super::forget_pin();
        }
        // The moved position lasts exactly one showing and never reaches the configuration file (the
        // A2 drag scenario asserts the across-a-relaunch half of the same promise).
        super::grip::set_session_origin(NSPoint::new(42.0, 42.0));
        super::grip::on_picker_hidden();
        assert!(
            super::grip::session_origin().is_none(),
            "hiding the picker must forget the dragged position"
        );
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        update_clear_action_labels();
        assert_eq!(
            super::text_style::clear_action_applied().unwrap().clear,
            super::t("clipboard.clear_button_query"),
            "a typed query must name the current view"
        );
        clear_search();
        assert_clear_actions_match_scope();
    }
    // Search smoke: set a query -> rebuild (filtered display) -> arrow navigation within the
    // filtered list -> clear restores everything.
    unsafe {
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // Search-field down-arrow: the delegate command interception (moveDown:) moves focus
        // into the list and selects the first filtered entry. The handler is called directly
        // -- in the real chain the field editor translates ↓ to moveDown: and invokes it.
        if let Some(_f) = *SEARCH_FIELD.lock().unwrap() {
            search_field_do_command(
                std::ptr::null_mut(),
                sel!(control:textView:doCommandBySelector:),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                sel!(moveDown:),
            );
        }
        // Up at the list top: focus jumps back to the search field (query kept); the
        // search field's begin-editing delegate callback (search_field_began_editing) clears
        // the selection -> the highlight disappears.
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // Assert: after focus enters the search field the selection is cleared (no row
        // highlight).
        assert_eq!(
            super::picker_selection(),
            NO_SELECTION,
            "selection must clear when focus moves into the search field"
        );
        // Esc level one: clear the query and restore the full list (list-focus path).
        let ev_esc = make_key_event(53);
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_esc as *mut c_void);
        }
        clear_search();
    }
    // Keyboard-navigation smoke: build a real NSEvent and drive container_key_down, covering
    // arrow -> select -> scroll-into-view (once panicked on a wrong return-type encoding for
    // scrollRectToVisible:).
    unsafe {
        // Take the pointer first, then enter the block: the if-let scrutinee's temporary
        // MutexGuard lives until the block ends, and container_key_down -> rebuild_rows
        // re-locks PICKER_CONTAINER inside the block -- a self-deadlock on the same thread
        // (the smoke run used to hang; sample confirmed the stack stuck in rebuild_rows' lock).
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            let ev2 = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev2 as *mut c_void);
            // Scroll several times: each scroll fires the clip-view bounds-change notification
            // -> the indicator-update path (setOpacity: once panicked on a float/double
            // encoding mismatch; the smoke run must cover it).
            for step in 1..=5u8 {
                let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, step as f64 * 30.0)];
            }
        }
    }
    // The drag handle end to end: move the panel through the real gesture path, then check every claim
    // the feature makes -- the window follows, the detail round trip keeps the dragged position, the
    // double-click reset returns to the configured one, and a hidden-then-reshown picker is back at the
    // configured position (the "one showing" promise, through the REAL hide path rather than the
    // cleanup helper the assertions above call directly). Without this, a `hide_picker()` that forgot
    // the cleanup would pass every other check in this file.
    unsafe {
        let panel_rect = || -> (f64, f64, f64, f64) {
            super::picker_frame_top_left().expect("the picker must have a frame while it is shown")
        };
        let baseline = panel_rect();
        // Grab the handle's own band, so the gesture goes through the same containment check a real
        // press does: the band is reported in the panel's points, top-left based.
        let grip = super::picker_grip_rect_top_left().expect("the handle must be built");
        let screen_height = crate::overlay::hover::primary_screen_frame()
            .expect("a primary screen")
            .size
            .height;
        // Screen coordinates (AppKit, bottom-left origin) for the press point and the target: the panel's
        // top-left is top-left based, so y flips through the primary screen's height.
        let to_appkit = |x: f64, y: f64| NSPoint::new(x, screen_height - y);
        let from = to_appkit(
            baseline.0 + grip.0 + grip.2 / 2.0,
            baseline.1 + grip.1 + grip.3 / 2.0,
        );
        let dx = 90.0;
        let dy = 60.0;
        let to = NSPoint::new(from.x + dx, from.y - dy);

        // The whole top bar is the handle, not the three dots: a press near the band's LEFT end must drag
        // the panel too (before this change only the centered 48pt band was a target, so this point was
        // inert). Reset afterwards so the rest of this block measures from the configured position.
        // 20pt from the PANEL's left edge, deliberately NOT derived from the band: the band this used to
        // be started at (PICKER_W - 48)/2 = 256pt, so a point taken relative to the band would have been
        // inside it in both geometries and would have proved nothing.
        let near_left = to_appkit(baseline.0 + 20.0, baseline.1 + grip.1 + grip.3 / 2.0);
        super::grip::e2e_drag(
            near_left,
            NSPoint::new(near_left.x + 25.0, near_left.y - 15.0),
        );
        let dragged_from_the_edge = panel_rect();
        assert!(
            (dragged_from_the_edge.0 - (baseline.0 + 25.0)).abs() < 1.5
                && (dragged_from_the_edge.1 - (baseline.1 + 15.0)).abs() < 1.5,
            "a press inside the top bar but far from the dots must drag the panel (moved to {},{}, expected {},{})",
            dragged_from_the_edge.0,
            dragged_from_the_edge.1,
            baseline.0 + 25.0,
            baseline.1 + 15.0
        );
        super::picker::reset_picker_to_configured_position();
        let restored = panel_rect();
        assert!(
            (restored.0 - baseline.0).abs() < 1.5 && (restored.1 - baseline.1).abs() < 1.5,
            "the reset after the edge drag must return the panel to the configured position"
        );

        // A click on the handle, and a wobble under the threshold: neither may move the panel, and neither
        // may leave the closed hand pushed (the press does that regardless of travel).
        let before_click = panel_rect();
        super::grip::e2e_drag(from, from);
        assert!(
            !super::grip::e2e_cursor_is_closed(),
            "a click on the handle must not leave the closed hand cursor held"
        );
        assert_eq!(
            panel_rect(),
            before_click,
            "a click on the handle must not move the panel"
        );
        super::grip::e2e_drag(from, NSPoint::new(from.x + 1.0, from.y - 1.0));
        assert!(
            !super::grip::e2e_cursor_is_closed(),
            "a wobble under the drag threshold must not leave the closed hand cursor held"
        );
        assert_eq!(
            panel_rect(),
            before_click,
            "a wobble under the drag threshold must not move the panel"
        );

        super::grip::e2e_drag(from, to);
        let dragged = panel_rect();
        assert!(
            (dragged.0 - (baseline.0 + dx)).abs() < 1.5
                && (dragged.1 - (baseline.1 + dy)).abs() < 1.5,
            "the panel must follow the handle: dragged to ({},{}), expected ({},{})",
            dragged.0,
            dragged.1,
            baseline.0 + dx,
            baseline.1 + dy
        );
        // The anchor is what a detail panel's close restores, so it has to be where the drag ENDED. It is a
        // point in AppKit screen coordinates (bottom-left origin, the panel's bottom edge), while the
        // reported rect is top-left based, so the comparison converts the dragged rect's top edge back.
        let anchor = super::grip::session_origin().expect("a finished drag must leave an anchor");
        let dragged_appkit_origin_y = screen_height - dragged.1 - dragged.3;
        assert!(
            (anchor.x - dragged.0).abs() < 1.5
                && (anchor.y - dragged_appkit_origin_y).abs() < 1.5,
            "the anchor must be the dragged position ({} ,{}), not the press-time one (panel top-left {},{})",
            anchor.x,
            anchor.y,
            dragged.0,
            dragged.1
        );

        // Detail round trip: opening it must not move the panel horizontally (the defect was that the
        // group layout re-derived x and snapped a dragged panel back), and the position a close restores
        // must be the dragged anchor rather than where the gesture started.
        //
        // Close whatever earlier steps left open first, then open TWICE: the first open goes through the
        // open animation, whose model frame has not landed without a run-loop turn, while the second takes
        // the "already visible" path and applies the frames immediately. That second path is also the one a
        // selection change while the detail is open takes -- exactly the case that re-derived the x.
        super::hide_detail();
        assert!(
            !super::detail_visible(),
            "the detail must be closed before the round trip under test"
        );
        super::set_picker_selection(0);
        rebuild_rows();
        super::picker::show_detail_for_sel();
        assert!(
            super::detail_visible(),
            "the detail panel must open for the selected entry"
        );
        // The origin a close restores: this is the wiring that returns the panel to where the drag ended.
        let restore_origin = (*super::DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap())
            .expect("opening the detail must capture the origin its close restores");
        assert!(
            (restore_origin.x - anchor.x).abs() < 1.5 && (restore_origin.y - anchor.y).abs() < 1.5,
            "the origin a detail close restores must be the dragged anchor ({},{}), found ({},{})",
            anchor.x,
            anchor.y,
            restore_origin.x,
            restore_origin.y
        );
        super::picker::show_detail_for_sel();
        let opened = panel_rect();
        // The rule the layout must follow with an anchor: keep the anchor, and move the picker left ONLY by
        // what the group needs to fit inside the screen's edge margins. Stated as arithmetic here rather
        // than by calling the layout again -- a test that re-runs its subject proves nothing.
        let detail_w = super::DETAIL_WINDOW
            .lock()
            .unwrap()
            .map(|window| crate::glass::panel_frame_of(window.0).size.width)
            .expect("the detail window must exist while its panel is open");
        let group_w = super::PICKER_W + super::DETAIL_GAP + detail_w;
        let screen_w = crate::overlay::hover::primary_screen_frame()
            .expect("a primary screen")
            .size
            .width;
        let fitted =
            (screen_w - super::PICKER_EDGE_MARGIN - group_w).max(super::PICKER_EDGE_MARGIN);
        let expected_x = if dragged.0 + group_w <= screen_w - super::PICKER_EDGE_MARGIN {
            dragged.0
        } else {
            fitted
        };
        assert!(
            (opened.0 - expected_x).abs() < 1.5,
            "a layout with the detail open must keep the dragged x {} unless the group needs room (expected {}, found {})",
            dragged.0,
            expected_x,
            opened.0
        );
        super::hide_detail();
        assert!(
            !super::detail_visible(),
            "the detail must close again for the reset below"
        );

        // Double-click reset: back to the configured position, which is where this showing started.
        super::picker::reset_picker_to_configured_position();
        let reset = panel_rect();
        assert!(
            (reset.0 - baseline.0).abs() < 1.5 && (reset.1 - baseline.1).abs() < 1.5,
            "the reset must return the panel to the configured position ({},{}), found ({},{})",
            baseline.0,
            baseline.1,
            reset.0,
            reset.1
        );

        // The lifecycle: drag, hide through the real path, summon again -- the configured position decides
        // again, and nothing of the drag survives.
        super::grip::e2e_drag(from, to);
        assert!(super::grip::session_origin().is_some());
        super::hide_picker();
        assert!(
            super::grip::session_origin().is_none(),
            "hiding the picker must forget the dragged position through the real hide path"
        );
        super::show_picker();
        let reshown = panel_rect();
        assert!(
            (reshown.0 - baseline.0).abs() < 1.5 && (reshown.1 - baseline.1).abs() < 1.5,
            "a re-summoned picker must be back at the configured position ({},{}), found ({},{})",
            baseline.0,
            baseline.1,
            reshown.0,
            reshown.1
        );

        // A detail closed a moment ago, then a grab of the handle before its queued orderOut runs: the queued
        // call belongs to a detail the app already considers closed, so it must still complete. Cancelling it
        // (what the first version of the drag did) left the window ordered front, transparent and collapsed
        // with nothing left to retry. This runs last because the run-loop turn below can also deliver the
        // picker's own resign-key handling.
        super::picker::show_detail_for_sel();
        super::hide_detail();
        let detail_window = super::DETAIL_WINDOW.lock().unwrap().map(|w| w.0);
        super::grip::e2e_drag(from, to);
        let run_loop: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
        let limit: *mut AnyObject = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 0.6f64];
        let _: () = msg_send![run_loop, runUntilDate: limit];
        if let Some(window) = detail_window {
            let visible: bool = msg_send![window, isVisible];
            assert!(
                !visible,
                "the detail window must be ordered out after its close, even when the handle is grabbed before the queued call runs"
            );
        }
    }
    // Detail/pin smoke: → opens the detail (text), ↓ follows it (stays open), ← pins while
    // keeping the detail open, and → closes it; an image entry's → opens its detail (lazy
    // .detail generation); with the detail open, Esc's level one closes the detail only.
    unsafe {
        super::set_picker_selection(0);
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            // Right: expand the detail for display 0 (the newest recorded entry, an image ->
            // the image branch).
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
                "right arrow must open the detail panel"
            );
            // Down: the detail follows the selection (stays open); the selection moves to a
            // text entry -> the full-text branch.
            let ev = make_key_event(125);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
                "the detail must stay open while navigating"
            );
            // An overflowing text detail must use stable full layout plus the native scroller
            // and open at AppKit's actual top. Simulate bounds crossing both endpoints; the
            // notification callback only refreshes the capsules and must never rewrite the
            // clip view (rubber banding is native elasticity's job).
            let wrapped_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("text detail must install a scroll view")
                .0;
            let wrapped_doc: *mut AnyObject = msg_send![wrapped_scroll, documentView];
            let wrapped_string: *mut AnyObject = msg_send![wrapped_doc, string];
            let wrapped_horizontal: bool = msg_send![wrapped_scroll, hasHorizontalScroller];
            assert!(
                !wrapped_horizontal,
                "soft-wrap detail must remove the horizontal scroller"
            );
            assert!(
                nsstring_to_rust(wrapped_string).contains('\u{2028}'),
                "long code detail must contain custom soft-wrap separators"
            );
            assert!(
                DETAIL_SOURCE_MAP.lock().unwrap().is_some(),
                "custom soft wraps must retain a source map for copying"
            );

            let detail_content = DETAIL_CONTENT.lock().unwrap().unwrap().0;
            let wrap_button = detail_wrap_button(detail_content);
            assert!(
                !wrap_button.is_null(),
                "code detail must install a wrap button"
            );
            // The save-as button is only presence-checked: clicking it would present the
            // NSSavePanel modal loop, which tests must not trigger.
            let save_as_button = detail_save_as_button(detail_content);
            assert!(
                !save_as_button.is_null(),
                "detail toolbar must install a save-as button"
            );
            let _: () = msg_send![wrap_button, performClick: std::ptr::null::<AnyObject>()];
            let no_wrap_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("no-wrap detail must rebuild its scroll view")
                .0;
            let no_wrap_doc: *mut AnyObject = msg_send![no_wrap_scroll, documentView];
            let no_wrap_string: *mut AnyObject = msg_send![no_wrap_doc, string];
            let no_wrap_horizontal: bool = msg_send![no_wrap_scroll, hasHorizontalScroller];
            let no_wrap_frame: NSRect = msg_send![no_wrap_doc, frame];
            let no_wrap_clip: *mut AnyObject = msg_send![no_wrap_scroll, contentView];
            let no_wrap_bounds: NSRect = msg_send![no_wrap_clip, bounds];
            assert!(
                !no_wrap_horizontal,
                "no-wrap mode must use only the custom horizontal indicator"
            );
            assert!(
                DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap().is_some(),
                "no-wrap mode must install a custom horizontal indicator"
            );
            assert!(
                !nsstring_to_rust(no_wrap_string).contains('\u{2028}'),
                "no-wrap mode must display the untouched source"
            );
            // No-wrap mode carries the source map too (midpoints depend on it for lossless copy), and the
            // displayed text must contain midpoint markers and no U+2028 other than real spaces.
            assert!(DETAIL_SOURCE_MAP.lock().unwrap().is_some());
            assert!(
                no_wrap_frame.size.width > no_wrap_bounds.size.width,
                "long source lines must overflow the horizontal viewport"
            );

            // Restore the default soft-wrap state before checking full layout and vertical bounds.
            let no_wrap_button = detail_wrap_button(detail_content);
            assert!(
                !no_wrap_button.is_null(),
                "rebuilt code detail must retain its wrap button"
            );
            let _: () = msg_send![no_wrap_button, performClick: std::ptr::null::<AnyObject>()];
            let detail_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("wrapped detail must rebuild its scroll view")
                .0;
            let detail_clip: *mut AnyObject = msg_send![detail_scroll, contentView];
            let (min_y, max_y) =
                detail_scroll_range(detail_scroll).expect("detail must have a legal range");
            assert!(max_y > min_y, "long detail must overflow");
            let opened_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                opened_bounds.origin.y, min_y,
                "detail must open at its real top"
            );
            let has_scroller: bool = msg_send![detail_scroll, hasVerticalScroller];
            let has_horizontal_scroller: bool = msg_send![detail_scroll, hasHorizontalScroller];
            let autohides: bool = msg_send![detail_scroll, autohidesScrollers];
            let scroller_style: isize = msg_send![detail_scroll, scrollerStyle];
            let elasticity: isize = msg_send![detail_scroll, verticalScrollElasticity];
            let detail_doc: *mut AnyObject = msg_send![detail_scroll, documentView];
            let layout: *mut AnyObject = msg_send![detail_doc, layoutManager];
            let noncontiguous: bool = msg_send![layout, allowsNonContiguousLayout];
            let background: bool = msg_send![layout, backgroundLayoutEnabled];
            assert!(
                !has_scroller,
                "detail must disable the native vertical scroller"
            );
            assert!(
                !has_horizontal_scroller,
                "soft-wrap detail must not use a horizontal scroller"
            );
            assert!(
                autohides,
                "detail keeps native scroller auto-hide enabled as a defensive fallback"
            );
            assert_eq!(scroller_style, 1, "detail must use overlay scrollers");
            // 0 = NSScrollElasticityAutomatic: endpoint rubber banding is native; the
            // bounds notification must never rewrite the clip view (hard-clamping fights
            // momentum and twitches the scrollbar).
            assert_eq!(
                elasticity, 0,
                "detail must keep native automatic rubber-band elasticity"
            );
            assert!(!noncontiguous, "detail layout must be contiguous");
            assert!(!background, "detail background layout must be disabled");
            // The bounds callback may update only the capsule, never the clip view. Some
            // macOS versions synchronously clamp setBoundsOrigin, so compare the actual
            // origin before and after the callback instead of assuming overscroll can always
            // be constructed.
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, min_y - 30.0)
            ];
            let top_before_callback: NSRect = msg_send![detail_clip, bounds];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let top_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                top_bounds.origin.y, top_before_callback.origin.y,
                "top bounds callback must not rewrite the clip view"
            );
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, max_y + 30.0)
            ];
            let bottom_before_callback: NSRect = msg_send![detail_clip, bounds];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let bottom_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                bottom_bounds.origin.y, bottom_before_callback.origin.y,
                "bottom bounds callback must not rewrite the clip view"
            );
            scroll_detail_to_top(detail_scroll);
            // Left: pin the selected entry while the detail stays open and follows its new row.
            let pinned_text = {
                let sel_idx = super::picker_selection();
                let hist = CLIP_HISTORY.lock().unwrap();
                mapped_index(sel_idx)
                    .and_then(|h| hist.get(h))
                    .map(|e| e.text.clone())
            };
            let ev = make_key_event(123);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
                "left arrow must keep the detail panel open"
            );
            {
                let hist = CLIP_HISTORY.lock().unwrap();
                assert!(
                    pinned_text
                        .as_deref()
                        .is_some_and(|t| hist.iter().any(|e| e.pinned && e.text == t)),
                    "left arrow must pin the selected entry"
                );
            }
            // Right with the detail open: close the detail panel.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !super::detail_visible(),
                "right arrow must close the detail when it is open"
            );
            // Reopen with →, then close with → again to cover the detail toggle path.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(super::detail_visible());
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !super::detail_visible(),
                "right arrow must close the detail when it is open"
            );
            // Esc with the detail open: level one closes the detail; the picker stays.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(super::detail_visible());
            let ev = make_key_event(53);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(!super::detail_visible(), "Esc must close the detail first");
        }
        // Image entry: locate its display index, → expands it -> the lazy .detail preview is
        // generated and cached.
        let img_h = {
            let hist = CLIP_HISTORY.lock().unwrap();
            hist.iter().position(|e| e.image.is_some())
        };
        if let Some(h_idx) = img_h {
            let d_idx = super::with_clipboard_ui(|ui| ui.filtered.iter().position(|&h| h == h_idx));
            if let Some(d_idx) = d_idx {
                super::set_picker_selection(d_idx);
                rebuild_rows();
                let c_opt = *PICKER_CONTAINER.lock().unwrap();
                if let Some(c) = c_opt {
                    let ev = make_key_event(124);
                    container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
                    assert!(super::detail_visible(), "image detail must open");
                    // The hi-res detail image is generated on a worker; allow a short window
                    // for disk delivery so asynchronous work is not mistaken for a UI failure.
                    for _ in 0..20 {
                        if cache_read_detail_preview(tiny_hash).is_some() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    assert!(
                        cache_read_detail_preview(tiny_hash).is_some(),
                        "the lazy .detail preview must be generated on first open"
                    );
                }
            }
        }
    }
    // Empty-state placement: the hint's vertical center and the document height are derived from
    // the live viewport, but the row cache key carries no geometry and the clear actions rebuild in
    // place without resizing the panel. Reproduces the reported "empty picker with no hint and a
    // phantom scrollbar": drop the fixtures while the panel is still tall, then summon (which
    // shrinks it to the minimal height) and require the hint to be re-centered inside the viewport.
    unsafe {
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            remove_history_scope(&mut hist, true, ClipFilter::All, "");
        }
        clear_search();
        *CLIP_FILTER.lock().unwrap() = ClipFilter::All;
        rebuild_rows();
        hide_picker();
        show_picker();
        assert!(
            empty_state_layout_is_sane(),
            "the empty-state hint must be re-centered inside the summoned viewport"
        );
    }
    // Storage-failure notice: an unavailable session records nothing, so the copy belongs in the
    // empty-state hint -- and the panel must say why instead of looking like an unused history.
    unsafe {
        let empty_hint_text = || -> Option<String> {
            let label = (*EMPTY_STATE_VIEW.lock().unwrap())?.0;
            if label.is_null() {
                return None;
            }
            let hidden: bool = msg_send![label, isHidden];
            if hidden {
                return None;
            }
            Some(crate::ffi::nsstring_to_rust(msg_send![label, stringValue]))
        };
        // The button that offers the way out exists exactly while the storage is unavailable, sits
        // centred under the sentence, and carries the action that re-reads the key.
        let action_button = || -> Option<*mut AnyObject> {
            let button = (*EMPTY_STATE_ACTION.lock().unwrap())?.0;
            (!button.is_null() && !msg_send![button, isHidden]).then_some(button)
        };
        // 1) Healthy and empty: the ordinary empty text, and no way-out button.
        super::storage::set_ready(super::keyring::test_key());
        rebuild_rows();
        assert_eq!(
            empty_hint_text(),
            Some(t("clipboard.empty")),
            "a healthy empty history must show the ordinary empty text"
        );
        assert!(
            action_button().is_none(),
            "a healthy empty history must not offer keychain access"
        );

        // 2) Unavailable and empty: the hint carries the copy instead.
        super::storage::set_unavailable(super::keyring::KeyUnavailable::Simulated);
        let unavailable_copy = super::unavailable_copy().expect("the storage is unavailable here");
        rebuild_rows();
        assert_eq!(
            empty_hint_text(),
            Some(unavailable_copy),
            "an unavailable storage must say so in the empty-state hint"
        );

        // 2b) The button appears, centred, below the hint.
        let button = action_button().expect("an unavailable storage must offer the way out");
        {
            let frame: NSRect = msg_send![button, frame];
            let title: *mut AnyObject = msg_send![button, title];
            let title = crate::ffi::nsstring_to_rust(title);
            let action: objc2::runtime::Sel = msg_send![button, action];
            let hint = (*EMPTY_STATE_VIEW.lock().unwrap())
                .expect("the hint exists")
                .0;
            let hint_frame: NSRect = msg_send![hint, frame];
            assert_eq!(
                title,
                t("clipboard.grant_keychain_access"),
                "the button must name the action it performs"
            );
            assert_eq!(
                action,
                sel!(retryKeychainAccess:),
                "the button must re-read the key"
            );
            assert!(
                (frame.origin.x + frame.size.width / 2.0 - super::PICKER_W / 2.0).abs() <= 0.5,
                "the button must be centred in the panel (x {} w {})",
                frame.origin.x,
                frame.size.width
            );
            // Flipped coordinates: the hint's top edge is its origin.y, and the button must start
            // below it.
            assert!(
                frame.origin.y >= hint_frame.origin.y + hint_frame.size.height,
                "the button must sit under the sentence (button top {} hint bottom {})",
                frame.origin.y,
                hint_frame.origin.y + hint_frame.size.height
            );
        }

        // 2c) Unavailable with records somehow present: the sentence still wins (the panel must not
        //     list records next to a notice that says nothing can be recorded), and the button stays
        //     with it.
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            record_text(&mut hist, "row while unavailable", "", "", 50);
        }
        rebuild_rows();
        assert_eq!(
            empty_hint_text(),
            super::unavailable_copy(),
            "the failure sentence outranks a stale row"
        );
        assert!(
            action_button().is_some(),
            "the way out stays offered while the storage is unavailable"
        );

        // 3) Recovery through the button (a successful key read) lands on a NON-empty list: the
        //    empty-state layout is skipped then, so the rebuild itself has to hide the button -- the
        //    defect this asserts was found in review.
        super::storage::set_ready(super::keyring::test_key());
        rebuild_rows();
        assert_eq!(
            empty_hint_text(),
            None,
            "a non-empty list has no hint to render"
        );
        assert!(
            action_button().is_none(),
            "a recovered, non-empty list must not keep the way-out button over its rows"
        );

        // 4) And with the list emptied again, the ordinary empty text returns.
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            hist.clear();
        }
        rebuild_rows();
        assert_eq!(empty_hint_text(), Some(t("clipboard.empty")));
        assert!(
            action_button().is_none(),
            "a healthy session must not offer keychain access"
        );
    }
    // A hi-res preview that finished generating before a discard must not be delivered: the slot
    // carries the generation it was generated in, and the main-thread consumer refuses a stale one
    // (the worker stores through the same check, so neither end of the pipe can resurrect a deleted
    // entry's preview).
    {
        // Both axes stale: a discard is only one of the ways a preview loses its right to be shown.
        let stale = super::cache_generation().wrapping_sub(1);
        *DETAIL_PENDING_HD.lock().unwrap() = Some((12345, stale, 0, vec![1, 2, 3]));
        detail_preview_ready(
            std::ptr::null_mut(),
            sel!(detailPreviewReady:),
            std::ptr::null_mut(),
        );
        assert!(
            DETAIL_PENDING_HD.lock().unwrap().is_none(),
            "a preview generated before a discard must be refused and released"
        );
    }
    // Switch-off versus queued image work (the reviewer's scenario: copy an image, turn the feature
    // off immediately). Recording hands the original bytes to a background cache job; discarding the
    // history must invalidate those jobs and wait out a write in flight, or the file comes back
    // after the wipe. The wait below gives the worker time to run a stale job if the generation
    // guard is missing.
    {
        // Its own payload, so the fixture image above cannot mask the result.
        let bytes = std::sync::Arc::new(b"switch-off-during-queue".to_vec());
        let hash = crate::hash::fnv1a64(&bytes);
        super::schedule_image_cache_write(
            hash,
            Some(bytes.clone()),
            std::sync::Arc::new(bytes.as_ref().clone()),
            None,
            true,
        );
        super::clear_history_state();
        assert!(
            !super::clip_image_path(hash).exists(),
            "discarding the history must not leave the image cache behind"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !super::clip_image_path(hash).exists(),
            "a job queued before the discard must not recreate the cache file"
        );
        // Files only: in a smoke run the harness keeps the history file in a subdirectory of the
        // cache directory, and the wipe removes files, not directories.
        let leftovers: Vec<String> = std::fs::read_dir(super::clip_image_cache_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().is_file())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the image cache must be empty after a discard, found {leftovers:?}"
        );
    }
    // The header's settings button: run the action the live button targets and require the whole
    // promise -- the panel goes away, and the settings window lands on the clipboard page. It starts on
    // a DIFFERENT page, because a sidebar already sitting on the target page cannot tell a button that
    // navigated from one whose action did nothing.
    //
    // A1 rather than A2: opening the settings window fronts the app, which the focus-safe A2 scenarios
    // must not do, while the button's existence, frame, visibility and action selector are asserted on
    // the live view tree by `assert_header_controls_are_live` above.
    {
        crate::settings::show_settings_page(0);
        assert_eq!(
            crate::settings::e2e_selected_sidebar(),
            0,
            "the settings window must start on the General page"
        );
        show_picker();
        super::notifications::open_clipboard_settings(
            std::ptr::null_mut(),
            sel!(openClipboardSettings:),
            std::ptr::null_mut(),
        );
        assert!(
            !super::picker_is_visible(),
            "opening the settings page must take the panel down"
        );
        assert_eq!(
            crate::settings::e2e_selected_sidebar(),
            crate::settings::SETTINGS_CLIPBOARD_PAGE_INDEX,
            "the settings button must park the sidebar on the clipboard page"
        );
        // Whether the window APPEARED on that page (rather than being switched to it afterwards) is asserted
        // by the app-reopen smoke: this process never runs the lifecycle that presents it, so the page it
        // appears on is not observable here (the presented-page static stays unset).
        // The settings *window* being on screen is asserted by the app-reopen smoke instead: this
        // process never ran the lifecycle that builds it, so `isVisible` here would test the harness,
        // not the button. What this process uniquely owns is the panel half (above) and that the
        // navigation parks the sidebar on the page the header promises.
    }
    hide_picker();
    true
}

/// Build an arrow-key NSEvent (for the smoke run).
unsafe fn make_key_event(keycode: u16) -> *mut AnyObject {
    make_key_event_with_modifiers(keycode, 0)
}

/// Build an NSEvent with modifier flags for smoke tests.
unsafe fn make_key_event_with_modifiers(keycode: u16, modifiers: u64) -> *mut AnyObject {
    let chars = make_nsstring("x");
    // keyEventWithType: takes NSEventType (unsigned long), location, modifierFlags, timestamp,
    // windowNumber (NSInteger), context, characters, charactersIgnoringModifiers, isARepeat,
    // keyCode (unsigned short).
    let ev: *mut AnyObject = msg_send![
        class!(NSEvent),
        keyEventWithType: 10u64,
        location: NSPoint::new(0.0, 0.0),
        modifierFlags: modifiers,
        timestamp: 0.0f64,
        windowNumber: 0isize,
        context: std::ptr::null::<AnyObject>(),
        characters: chars,
        charactersIgnoringModifiers: chars,
        isARepeat: false,
        keyCode: keycode
    ];
    CFRelease(chars as *const c_void);
    ev
}

/// Rasterise an active detail icon and verify its two halves: the chip is filled with the accent
/// and its glyph is drawn in `accent_text` over it. Reading the pixels catches a mismatched role
/// (a fill the glyph cannot be seen against), which a token-level check cannot express.
unsafe fn active_detail_icon_glyph_is_legible_on_its_fill() -> bool {
    // Both appearances, not just the one this process resolves to: the defect was dark-only.
    let mut ok = true;
    for dark in [false, true] {
        if !active_detail_icon_glyph_is_legible_for_mode(dark) {
            ok = false;
        }
    }
    ok
}

/// Render an active detail icon and require its glyph to be legible *against its own fill*.
///
/// Measured from the pixels because the defect was a wrong role: the chip was filled with
/// `primary_text` while the glyph stayed `accent_text`. In light mode that is a dark chip with a
/// white glyph (13.91:1, fine), but dark `primary_text` is near-white, so the glyph measured
/// 1.09:1 and the icon showed as a blank disc. A token-level check cannot express "these two
/// roles are readable together"; the dominant colour is the fill, and the glyph is whatever
/// other colour the drawing put on top of it.
unsafe fn active_detail_icon_glyph_is_legible_for_mode(dark: bool) -> bool {
    // The floor for a non-text indicator (design-style §3.3).
    const MIN_GLYPH_CONTRAST: f64 = 3.0;
    let image = super::text_style::make_detail_action_icon_for_mode(dark, true, false);
    if image.is_null() {
        return false;
    }
    let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
    if tiff.is_null() {
        release_obj(image);
        return false;
    }
    let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
    let rep: *mut AnyObject = msg_send![rep, initWithData: tiff];
    if rep.is_null() {
        release_obj(image);
        return false;
    }
    let width: usize = msg_send![rep, pixelsWide];
    let height: usize = msg_send![rep, pixelsHigh];
    let mut histogram: Vec<([f64; 3], usize)> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let color: *mut AnyObject = msg_send![rep, colorAtX: x, y: y];
            if color.is_null() {
                continue;
            }
            let alpha: f64 = msg_send![color, alphaComponent];
            if alpha < 0.9 {
                continue;
            }
            let rgb: [f64; 3] = [
                msg_send![color, redComponent],
                msg_send![color, greenComponent],
                msg_send![color, blueComponent],
            ];
            let mut bucket: Option<usize> = None;
            for (index, (existing, _)) in histogram.iter().enumerate() {
                if (0..3usize).all(|channel| (existing[channel] - rgb[channel]).abs() < 0.04) {
                    bucket = Some(index);
                    break;
                }
            }
            match bucket {
                Some(index) => histogram[index].1 += 1,
                None => histogram.push((rgb, 1)),
            }
        }
    }
    release_obj(rep);
    release_obj(image);
    let Some((fill, fill_count)) = histogram.iter().max_by_key(|(_, count)| *count).cloned() else {
        return false;
    };
    if fill_count < 20 {
        eprintln!(
            "[smoke-clipboard] active detail icon has no filled chip in {} mode",
            if dark { "dark" } else { "light" }
        );
        return false;
    }
    // The glyph is the most prominent *other* colour: a solid disc with no visible glyph fails.
    let glyph = histogram
        .iter()
        .filter(|(rgb, count)| *count >= 4 && (0..3usize).any(|c| (rgb[c] - fill[c]).abs() > 0.04))
        .max_by(|(a, _), (b, _)| {
            let ca = crate::theme::contrast_ratio_srgb(*a, fill);
            let cb = crate::theme::contrast_ratio_srgb(*b, fill);
            ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned();
    let Some((glyph, _)) = glyph else {
        eprintln!(
            "[smoke-clipboard] active detail icon is a blank disc in {} mode",
            if dark { "dark" } else { "light" }
        );
        return false;
    };
    let contrast = crate::theme::contrast_ratio_srgb(glyph, fill);
    if contrast < MIN_GLYPH_CONTRAST {
        eprintln!(
            "[smoke-clipboard] active detail icon glyph measures {contrast:.2}:1 on its own fill in {} mode (min {MIN_GLYPH_CONTRAST})",
            if dark { "dark" } else { "light" }
        );
        return false;
    }
    true
}
