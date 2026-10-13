//! Key-bindings page: the dynamic binding rows and the add-a-binding flow.
//!
//! Rows are data, not build-time furniture: adding, deleting or toggling a binding rebuilds the
//! affected group's rows from the config and hands the new height to the page canvas. Two
//! invariants from AGENTS.md shape the code:
//!
//! * a `SettingsButton`'s `tag` selects its hover/normal palette, so an item index must never
//!   travel in `setTag:` — the row controls are looked up by their own pointer instead;
//! * every view about to be released is unregistered first (`SettingsRow::forget`), because the
//!   disabled-hint/tooltip registries are keyed by the view's address.

use super::*;

use crate::key_bindings::{Action, Trigger};

/// The binding row's height: the shared settings row height, so a binding row is the same height as
/// every other settings row (style doc §5, "Rows | 52 tall, control 32 tall, vertically centred").
/// It is an alias rather than its own number because a separate 40pt table row was exactly the
/// drift the layout grid exists to prevent.
pub(crate) const BINDING_ROW_H: f64 = SettingsLayout::SINGLE_LINE_ROW_H;
// The alias has to stay an alias: a private row height is the drift this replaced.
const _: () = assert!(BINDING_ROW_H == SettingsLayout::SINGLE_LINE_ROW_H);
// The footer band has to fit the shared control with the band's padding on both sides, or the rows
// below shift and the control is not vertically centred.
const _: () =
    assert!(BINDING_FOOTER_H >= SettingsActionSegments::HEIGHT + BINDING_FOOTER_PAD * 2.0);
const BINDING_CAP_SIZE: f64 = 20.0;
const BINDING_CAP_GAP: f64 = 2.0;
const BINDING_SWITCH_W: f64 = 44.0;
/// The card's inner padding: `SettingsLayout::label_x` horizontally and its card bottom inset
/// vertically, so the rows line up with every other card's content. `padding_matches_the_shared_layout`
/// pins the two against those tokens, so moving a token cannot silently misalign this table.
pub(crate) const BINDING_PAD_X: f64 = 16.0;
pub(crate) const BINDING_PAD_Y: f64 = 8.0;
const BINDING_COL_GAP: f64 = 10.0;
/// The add panel's geometry. Named here rather than at the call sites so the panel's size, its
/// padding and its control rows stay one set of numbers (the mouse page's panel has its own; the
/// two are separate surfaces).
const PANEL_W: f64 = 440.0;
const PANEL_H: f64 = 200.0;
/// One control row's height inside the panel, and the panel's own edge padding.
const PANEL_ROW_H: f64 = 26.0;
/// The width of a panel row's leading label column, and the record button's own width.
const PANEL_LABEL_W: f64 = 88.0;
/// The height of one panel label line.
const PANEL_LABEL_H: f64 = 20.0;
const PANEL_RECORD_W: f64 = 134.0;
/// The gap between the panel's Cancel and OK buttons.
const PANEL_BTN_GAP: f64 = 8.0;
const PANEL_PAD: f64 = 16.0;
/// The panel's action/OK/Cancel button size.
const PANEL_BTN_H: f64 = 28.0;
const PANEL_BTN_W: f64 = 88.0;
/// Distance from the panel's top edge to each control row.
const PANEL_ACTION_Y: f64 = 150.0;
const PANEL_CHORD_Y: f64 = 110.0;
const PANEL_MESSAGE_Y: f64 = 76.0;
const PANEL_BUTTON_Y: f64 = 24.0;

/// The footer band that carries the ＋ / − segmented control, below the last row and inside the
/// same card (System-Settings style). It is part of the card's height, so the canvas moves the rows
/// below it by exactly this much.
pub(crate) const BINDING_FOOTER_H: f64 = 44.0;
/// The footer control's segment indices. They are named because the render pass, the enable pass and
/// the click handler all address the same two halves, and an off-by-one between them would silently
/// wire "add" to the delete path.
const FOOTER_SEGMENT_ADD: usize = 0;
const FOOTER_SEGMENT_REMOVE: usize = 1;

/// The footer band's padding above and below its control. Equal on both sides is what centres the
/// control in the band; the band's height and this padding are the only two numbers involved, so
/// changing either can only move the control symmetrically.
const BINDING_FOOTER_PAD: f64 = (BINDING_FOOTER_H - SettingsActionSegments::HEIGHT) / 2.0;
/// Where the footer's control sits, measured from the band's top edge. Production uses this and the
/// test asserts it, so the centring rule has exactly one definition.
fn footer_control_offset() -> f64 {
    BINDING_FOOTER_PAD
}
// "Centred" is only meaningful when the band is taller than what it holds.
const _: () = assert!(BINDING_FOOTER_H > SettingsActionSegments::HEIGHT);

/// A rendered row. Owns every view it created so a rebuild can unregister and remove them.
pub(crate) struct BindingRow {
    /// Index into `key_bindings.items`: what this row edits, how a config change finds the switch to
    /// sync, and what the footer's − deletes when this row is the selection.
    item: usize,
    keycaps: Vec<*mut AnyObject>,
    label: *mut AnyObject,
    toggle: *mut AnyObject,
    /// The full-row click surface that makes this row the selection. Transparent and borderless, so
    /// it adds a hit target without adding chrome; it sits below the row's own views, which is why
    /// the label and the switch still receive their own clicks.
    select: *mut AnyObject,
    separator: *mut AnyObject,
}

impl BindingRow {
    /// The controls that carry a target/action, and so are the ones a click can arrive on.
    fn interactive(&self) -> [*mut AnyObject; 2] {
        [self.toggle, self.select]
    }

    /// Every view that may be registered in the address-keyed tooltip/hint registries. The label
    /// joins the controls because the master switch dims it.
    fn registered(&self) -> [*mut AnyObject; 3] {
        [self.toggle, self.select, self.label]
    }

    fn views(&self) -> Vec<*mut AnyObject> {
        let mut views = self.keycaps.clone();
        views.push(self.label);
        views.push(self.toggle);
        views.push(self.select);
        views.push(self.separator);
        views
    }
}

/// A recorded chord waiting for the user's confirmation: the action it would run and the chord as
/// it will be written. Nothing reaches the config until the confirm button is pressed, so the add
/// flow keeps its "choose, record, then agree" shape.
static BINDING_DRAFT: LazyLock<Mutex<Option<(Action, String)>>> =
    LazyLock::new(|| Mutex::new(None));

/// Sender view address -> item index, for the row controls. Rebuilt with the rows and cleared
/// before they are released, so a click can never reach a freed address.
static ROW_CONTROLS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The add panel and the views inside it whose text or enabled state changes while it is open.
///
/// Each is a bare pointer into a window this module owns; every one is cleared in
/// `close_binding_panel` *before* the panel is released, because releasing the panel deallocates its
/// content view's whole subtree. They live in `MainThreadSlot`s like the mouse page's panel slots,
/// so a background thread cannot reach them and the borrow is checked at the call site.
static PANEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_ACTION: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_CHORD_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_RECORD: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_MESSAGE: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_OK: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static PANEL_DIM: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

fn register_control(view: *mut AnyObject, item: usize) {
    if view.is_null() {
        return;
    }
    if let Ok(mut controls) = ROW_CONTROLS.lock() {
        controls.insert(view as usize, item);
    }
}

/// The item index a row control belongs to, or `None` when the sender is not one of ours.
fn control_item(sender: *mut AnyObject) -> Option<usize> {
    ROW_CONTROLS
        .lock()
        .ok()
        .and_then(|controls| controls.get(&(sender as usize)).copied())
}

/// The selected row index, as `--e2e-state` publishes it.
pub(crate) fn selected_binding_index() -> Option<usize> {
    with_settings_ui(|ui| ui.as_ref().and_then(|ui| ui.key_binding_selected))
}

/// Select the row a click landed on and repaint the highlight. Selecting the already-selected row
/// keeps it selected: a second click is not a deselect, or − would lose its target to a stray
/// double-click.
fn select_binding(item: usize) {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_mut() else {
            return;
        };
        if ui.key_binding_selected == Some(item) {
            return;
        }
        ui.key_binding_selected = Some(item);
        crate::log_debug!("[keybind] selected row {}", item);
        let items = match CONFIG.read() {
            Ok(cfg) => cfg.key_bindings.items.clone().unwrap_or_default(),
            Err(_) => return,
        };
        unsafe {
            repaint_selection(ui, items.len());
            // Selecting is what gives − a target, so it has to arm it: the footer's enablement is
            // otherwise only recomputed when the rows are rebuilt.
            update_footer_remove_enabled(ui);
        }
    });
}

/// Clear the selection and repaint. Called from every path that changes the list's shape behind the
/// page's back, so a stale index can never aim − at the wrong binding.
pub(crate) fn clear_selection() {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_mut() else {
            return;
        };
        if ui.key_binding_selected.is_none() && ui.key_binding_selection_bg.is_null() {
            return;
        }
        ui.key_binding_selected = None;
        unsafe {
            repaint_selection(ui, 0);
            update_footer_remove_enabled(ui);
        }
    });
}

/// Smoke check that the row's selection overlay sits *below* the row's own controls.
///
/// This is the assertion for a defect introduced while this row was moved onto the shared row
/// builders: the whole-row selection surface was appended with `addSubview:`, and `addSubview:` puts
/// the new view on TOP. The overlay is transparent but still hit-tests, so it covered the switch and
/// swallowed its clicks -- clicking the switch selected the row instead. The layout validator cannot
/// see it (the overlay is a registered hit target, so the overlap pass skips it by design), so the
/// ordering is asserted directly.
///
/// Subview order is the deterministic fact here: `hitTest:` depends on the window's current
/// coordinate state and returned an unrelated view when driven from a smoke runner, so the check is
/// on the ordering the defect was actually about.
pub(crate) unsafe fn debug_validate_row_overlay_is_below_controls() -> bool {
    let (pairs, doc) = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| {
            (
                ui.key_binding_rows
                    .iter()
                    .map(|row| (row.toggle, row.select))
                    .collect::<Vec<_>>(),
                ui.key_bindings_doc,
            )
        })
    })
    .unwrap_or((Vec::new(), std::ptr::null_mut()));
    if doc.is_null() || pairs.is_empty() {
        return true; // Nothing built: nothing to validate.
    }
    let subviews: *mut AnyObject = msg_send![doc, subviews];
    let count: usize = msg_send![subviews, count];
    for (toggle, select) in pairs {
        if toggle.is_null() || select.is_null() {
            continue;
        }
        let mut overlay = usize::MAX;
        let mut control = usize::MAX;
        for index in 0..count {
            let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
            if view == select {
                overlay = index;
            }
            if view == toggle {
                control = index;
            }
        }
        if overlay == usize::MAX || control == usize::MAX {
            continue; // Not both in this document (a rebuild may have replaced one).
        }
        // A HIGHER index draws on top, so the overlay must have the lower one.
        if overlay >= control {
            log_info!(
                "[smoke-settings-layout] the row overlay is above the row's switch (overlay index {overlay}, switch index {control}), so the switch cannot be clicked"
            );
            return false;
        }
    }
    true
}

/// Smoke check for the footer's segment gating.
///
/// The two halves of the joined control have different availability: `＋` is available whenever the
/// feature is on, and `−` additionally needs a selection. Three places decide that -- the footer's
/// own render, the selection-change path, and the master-switch pass -- so the risk is a *missing or
/// wrong* per-segment call rather than a lost one. (Measured 2026-10-13: a whole-control
/// `setEnabled:` does not reset the per-segment flags, so it cannot clobber them; see the note on
/// `update_binding_controls_enabled`.) This asserts the two flags against the state the page is in.
///
/// It is checked on the real view tree because per-segment enablement lives in AppKit's cell, which
/// a pure rule cannot observe. The caller drives the production enable pass first, so an absent or
/// incorrect per-segment call is what this sees.
pub(crate) unsafe fn debug_validate_footer_segment_gating() -> bool {
    let (control, selected) = with_settings_ui(|ui| {
        ui.as_ref()
            .map(|ui| (ui.key_bindings_footer_segments, ui.key_binding_selected))
    })
    .unwrap_or((std::ptr::null_mut(), None));
    if control.is_null() {
        return true; // The page is not built: nothing to validate.
    }
    let master_on = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false);
    let add_enabled: bool = msg_send![control, isEnabledForSegment: FOOTER_SEGMENT_ADD as isize];
    let remove_enabled: bool =
        msg_send![control, isEnabledForSegment: FOOTER_SEGMENT_REMOVE as isize];
    let expected_remove = master_on && selected.is_some();
    if add_enabled != master_on || remove_enabled != expected_remove {
        log_info!(
            "[smoke-settings-layout] footer segments gated wrong: add={add_enabled} (want {master_on}), remove={remove_enabled} (want {expected_remove}), selected={selected:?}"
        );
        return false;
    }
    true
}

/// Arm or disarm the footer's −. It may act only when the master switch is on *and* a row is
/// selected, so both the render pass and every selection change go through here rather than each
/// re-deriving the rule.
unsafe fn update_footer_remove_enabled(ui: &SettingsUi) {
    if ui.key_bindings_footer_segments.is_null() {
        return;
    }
    let master_on = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false);
    let can_remove = master_on && ui.key_binding_selected.is_some();
    // The tooltip lives on the control, which AppKit owns: a disabled segment does not carry a
    // per-segment hint, so the whole control states why it is unavailable. This call sets the
    // control's own state; the per-segment calls below decide each half and come after it.
    SettingsRow::set_view_enabled_with_tooltip(
        ui.key_bindings_footer_segments,
        master_on,
        Some(&t("settings.tooltip_key_bindings_disabled")),
    );
    // `＋` is always available while the feature is on: adding the first binding is exactly what the
    // user needs with an empty list. `−` needs a selection.
    SettingsActionSegments::set_segment_enabled(
        ui.key_bindings_footer_segments,
        FOOTER_SEGMENT_ADD,
        master_on,
    );
    SettingsActionSegments::set_segment_enabled(
        ui.key_bindings_footer_segments,
        FOOTER_SEGMENT_REMOVE,
        can_remove,
    );
}

/// Remove the current highlight and draw one for the current selection. `items_len` is only used to
/// decide whether the selected index is still drawable; `0` therefore draws nothing, which is what
/// a clear wants.
unsafe fn repaint_selection(ui: &mut SettingsUi, items_len: usize) {
    if !ui.key_binding_selection_bg.is_null() {
        let _: () = msg_send![ui.key_binding_selection_bg, removeFromSuperview];
        ui.key_binding_selection_bg = std::ptr::null_mut();
    }
    let doc = ui.key_bindings_doc;
    if doc.is_null() {
        return;
    }
    let doc_frame: NSRect = msg_send![doc, frame];
    render_selection_bg(ui, doc, doc_frame.size.width.max(1.0), items_len);
}

/// Rebuild the page's rows from `cfg`. Called on every open, after every edit, and after restore
/// defaults; the page canvas re-flows the single card from the height handed over here.
pub(crate) unsafe fn render_binding_rows(ui: &mut SettingsUi, cfg: &Config) {
    // The footer is one container view holding its joined +/− control and the caption. It is removed
    // as a unit: rebuilding just the rows would leave every previous band in place, and the views
    // would accumulate on each edit.
    if !ui.key_bindings_footer.is_null() {
        // Unregister before removing: the two buttons carry disabled-hint/tooltip registrations keyed
        // by their address, and those tables outlive the views. A stale key would later be handed a
        // recycled address, so a new button would inherit the old one's hint state.
        if !ui.key_bindings_footer_segments.is_null() {
            SettingsRow::forget(ui.key_bindings_footer_segments);
        }
        let _: () = msg_send![ui.key_bindings_footer, removeFromSuperview];
        ui.key_bindings_footer = std::ptr::null_mut();
        ui.key_bindings_footer_segments = std::ptr::null_mut();
    }
    // Tear the previous rows down first: unregister, then remove. The order matters (see the module
    // comment), and the address map is cleared in the same pass.
    for row in ui.key_binding_rows.drain(..) {
        for control in row.registered() {
            if !control.is_null() {
                SettingsRow::forget(control);
            }
        }
        for view in row.views() {
            if !view.is_null() {
                let _: () = msg_send![view, removeFromSuperview];
            }
        }
        // A separator-less last row carries a null view; the loop's null check skips it.
    }
    if let Ok(mut controls) = ROW_CONTROLS.lock() {
        controls.clear();
    }

    let items = cfg.key_bindings.items.clone().unwrap_or_default();
    let doc = ui.key_bindings_doc;
    let _: () = msg_send![ui.key_bindings_empty, setHidden: !items.is_empty()];

    // The selection is an index into the list being rendered, so it is settled here rather than at
    // each caller. A changed shape drops it entirely: every path that rebuilds the list (restore
    // defaults, an external edit, an add) renders through this function, and an index that survived
    // a reorder would aim the footer's − at a different binding. A same-shape rebuild (a switch
    // flip) keeps it, clamped, so − stays usable while the user flips rows.
    let shape = binding_list_shape(&items);
    if shape != ui.key_binding_items_rendered {
        ui.key_binding_selected = None;
    }
    ui.key_binding_selected = clamp_selection(ui.key_binding_selected, items.len());

    let doc_frame: NSRect = msg_send![doc, frame];
    let width = doc_frame.size.width.max(1.0);
    // The rows fill the card from its top inset down; the card's own surface is the only chrome, and
    // its section header sits above the card, drawn by the page canvas.
    let mut y = BINDING_PAD_Y;
    for (item, binding) in items.iter().enumerate() {
        // A flat list in config order: the user's arrangement is the page's arrangement, and an
        // entry that names an unknown action stays visible (its raw id is shown) instead of
        // vanishing from a list that is the only place to delete it. Config validation drops such
        // an entry on the next load, so it never survives a restart.
        let row = render_row(cfg, doc, width, y, item, binding);
        ui.key_binding_rows.push(row);
        y += BINDING_ROW_H;
    }
    // One row of height even when empty: the empty-state hint sits in that band, and a
    // zero-height card would clip it away.
    let rows_h = (y - BINDING_PAD_Y).max(BINDING_ROW_H);
    let card_h = card_height(rows_h);
    render_footer(cfg, ui, doc, width, rows_h);
    // The highlight goes on last so it is drawn behind the rows it covers, and `repaint_selection`
    // owns the removal of any previous one.
    repaint_selection(ui, items.len());
    let _: () = msg_send![doc, setFrameSize: NSSize::new(width, card_h)];
    // The canvas owns the card's place: hand it the height and it shifts the card's origin down
    // by the growth, so the top edge stays put and the rows below follow.
    ui.page_canvases[SETTINGS_KEY_BINDINGS_PAGE_INDEX].set_row_consume(ui.key_bindings_row, card_h);
    ui.key_binding_items_rendered = binding_list_shape(&items);
}

/// Draw the selection highlight behind the selected row, if any. A plain layer-backed view added
/// below the rows, so it never takes a click of its own: what selects a row is the row's own
/// transparent hit target, not this wash.
unsafe fn render_selection_bg(
    ui: &mut SettingsUi,
    doc: *mut AnyObject,
    width: f64,
    items_len: usize,
) {
    let Some(selected) = ui.key_binding_selected else {
        return;
    };
    if selected >= items_len {
        return;
    }
    // Inset by the card's horizontal padding plus a hair, so the wash reads as a row highlight
    // rather than as a full-bleed band touching the card's border.
    let x = BINDING_PAD_X - 4.0;
    let w = (width - (BINDING_PAD_X - 4.0) * 2.0).max(1.0);
    let top = BINDING_PAD_Y + selected as f64 * BINDING_ROW_H;
    let bg: *mut AnyObject = msg_send![class!(NSView), alloc];
    let bg: *mut AnyObject = msg_send![bg, initWithFrame: NSRect::new(
        NSPoint::new(x, top),
        NSSize::new(w, BINDING_ROW_H)
    )];
    let _: () = msg_send![bg, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![bg, layer];
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![layer, setMasksToBounds: true];
    layer_set_background(layer, hex_to_cg_color(settings_palette().selection_bg));
    // Added *below* every sibling: a plain `addSubview:` would put the wash on top of the row it
    // highlights, and a real click on that row's label or switch would land on the wash instead.
    // NSWindowBelow = -1, relativeTo nil = the bottom of the list.
    let _: () = msg_send![doc, addSubview: bg, positioned: -1isize, relativeTo: std::ptr::null::<AnyObject>()];
    // The document view owns it now; the field keeps a bare pointer only so the next render can
    // remove it.
    release_obj(bg);
    ui.key_binding_selection_bg = bg;
}

/// The card's height for a given rows band: the rows, plus exactly one footer band, plus the card's
/// bottom inset. One function so the renderer and its assertion cannot drift apart.
fn card_height(rows_h: f64) -> f64 {
    rows_h + BINDING_FOOTER_H + BINDING_PAD_Y
}

/// The identity of the rendered list: which action each row runs and which chord it holds, in
/// order. `enabled` is deliberately absent — a switch flip needs no rebuild, and rebuilding would
/// destroy the switch that is currently dispatching its own action.
fn binding_list_shape(items: &[crate::key_bindings::KeyBinding]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|item| (item.action.clone(), item.trigger.clone()))
        .collect()
}

/// Whether a config change altered the list's identity (order, actions or chords) rather than just
/// its switch states. The caller uses this to decide whether the selection can survive: an index is
/// only meaningful against the list it was chosen from.
fn shape_changed(rendered: &[(String, String)], now: &[(String, String)]) -> bool {
    rendered != now
}

/// Where the selection goes after the row at `item` is deleted from a list of `before` rows. The row
/// that slides into the gap is chosen, the new last row when the deleted one was last, and `None`
/// when nothing is left — which is what disables the footer's −.
fn selection_after_delete(item: usize, before: usize) -> Option<usize> {
    let remaining = before.saturating_sub(1);
    if remaining == 0 {
        None
    } else {
        Some(item.min(remaining - 1))
    }
}

/// The selection clamped to a list of `len` rows: an index past the end becomes the new last row, and
/// an empty list has no selection at all.
fn clamp_selection(selected: Option<usize>, len: usize) -> Option<usize> {
    match selected {
        _ if len == 0 => None,
        None => None,
        Some(index) if index >= len => Some(len - 1),
        Some(index) => Some(index),
    }
}

/// Sync the page from a config that changed outside this page (a menu toggle, a reloaded file):
/// the master switch and the add row always follow, the rows only when their shape changed, and a
/// same-shape change just moves each row's switch to the new state.
pub(crate) unsafe fn sync_from_config(ui: &mut SettingsUi, cfg: &Config) {
    let _: () = msg_send![
        ui.key_bindings_enabled,
        setState: if cfg.key_bindings.enabled { 1isize } else { 0isize }
    ];
    let items = cfg.key_bindings.items.clone().unwrap_or_default();
    if shape_changed(&ui.key_binding_items_rendered, &binding_list_shape(&items)) {
        // The shape changed behind the page's back (an external edit, a reload, restore defaults).
        // The selection is an index into this list, so it cannot survive that: dropping it is what
        // keeps the footer's − from deleting a binding the user never picked.
        ui.key_binding_selected = None;
        render_binding_rows(ui, cfg);
    } else {
        for row in &ui.key_binding_rows {
            let Some(binding) = items.get(row.item) else {
                continue;
            };
            if !row.toggle.is_null() {
                let _: () = msg_send![
                    row.toggle,
                    setState: if binding.enabled { 1isize } else { 0isize }
                ];
            }
        }
    }
    update_binding_controls_enabled(ui);
}

/// Render one row and return its handle. `y` is the row's top edge in the (flipped) document.
unsafe fn render_row(
    cfg: &Config,
    doc: *mut AnyObject,
    width: f64,
    y: f64,
    item: usize,
    binding: &crate::key_bindings::KeyBinding,
) -> BindingRow {
    let target = MENU_TARGET
        .lock()
        .unwrap()
        .map(|t| t.0)
        .unwrap_or(std::ptr::null_mut());
    let action = Action::from_config_id(&binding.action);
    // The row's own switch gates the binding; the master switch still applies on top (the runtime
    // table ANDs them), which is what the disabled state below communicates.
    let master_on = cfg.key_bindings.enabled;

    // Keycaps: one cell per symbol, laid out by the shared row's leading legend rather than by
    // hand. Only the count is ours to decide; the vertical centering is the row's business.
    let keycap_text = Trigger::parse(&binding.trigger)
        .map(|trigger| trigger.keycap())
        .unwrap_or_else(|_| binding.trigger.clone());
    let mut keycaps = Vec::new();
    let mut cap_x = BINDING_PAD_X;
    for ch in keycap_text.chars() {
        let cap = SettingsRow::legend_chip(
            doc,
            cap_x,
            y,
            BINDING_ROW_H,
            BINDING_CAP_SIZE,
            &ch.to_string(),
        );
        keycaps.push(cap);
        cap_x += BINDING_CAP_SIZE + BINDING_CAP_GAP;
    }

    // The trailing switch is built first: the shared row derives the label's width from the
    // control's leading edge, so the control has to exist before the row is laid out.
    let switch_x = width - BINDING_PAD_X - BINDING_SWITCH_W;
    let toggle = SettingsControl::switch(
        switch_x + BINDING_SWITCH_W,
        0.0,
        SettingsLayout::CONTROL_H,
        binding.enabled,
    );
    let _: () = msg_send![toggle, setTarget: target];
    let _: () = msg_send![toggle, setAction: sel!(handleBindingToggle:)];
    register_control(toggle, item);

    // Action name: the registry's localized label, never a config id.
    let text = action
        .map(crate::key_bindings::action_label)
        .unwrap_or_else(|| binding.action.clone());
    let (label, toggle) = SettingsRow::with_leading_keycaps(
        doc,
        cap_x + BINDING_COL_GAP,
        y,
        BINDING_ROW_H,
        &text,
        toggle,
    );

    // A switch has no visible title, so its accessible name carries the whole statement: what it
    // controls, and with which chord.
    let toggle_name = tf("settings.binding_toggle_named", &[("action", &text)]);
    let toggle_name_ns = make_nsstring(&toggle_name);
    let _: () = msg_send![toggle, setAccessibilityLabel: toggle_name_ns];
    CFRelease(toggle_name_ns as *const c_void);
    // The full action name while the pointer rests on the (truncating) label.
    super::tooltip::SettingsTooltip::attach_hover(label, &text);

    // The row's selection surface: the whole row. It is inserted at the BOTTOM of the z-order
    // (`positioned: -1`) rather than simply added, because `addSubview:` appends to the end and the
    // end is the top: adding it after the row's own views would cover the switch and swallow its
    // clicks, and a transparent view still hit-tests. The keycaps, label and switch are built above,
    // so they keep their own clicks and everything else in the row selects.
    //
    // A borderless, transparent button is the clickable view AppKit already routes `mouseDown:`
    // through, so this needs no new class or event plumbing.
    let select = SettingsButton::action(
        NSRect::new(
            NSPoint::new(BINDING_PAD_X - 4.0, y),
            NSSize::new(
                (width - (BINDING_PAD_X - 4.0) * 2.0).max(1.0),
                BINDING_ROW_H,
            ),
        ),
        "",
        target,
        sel!(handleBindingSelect:),
        SettingsButtonRole::Transparent,
    );
    // It is a hit target, not a control the user reads: keep it out of the tab order and give it the
    // accessible statement of what selecting it does.
    let _: () = msg_send![select, setRefusesFirstResponder: true];
    // Mark it as this row's hit target so the layout validator does not read the keycaps, label and
    // switch sitting inside it as a defect (see `ROW_HIT_TARGET_IDENTIFIER`).
    let hit_target_id = make_nsstring(ROW_HIT_TARGET_IDENTIFIER);
    let _: () = msg_send![select, setIdentifier: hit_target_id];
    CFRelease(hit_target_id as *const c_void);
    let select_name = tf("settings.binding_select_named", &[("action", &text)]);
    let select_name_ns = make_nsstring(&select_name);
    let _: () = msg_send![select, setAccessibilityLabel: select_name_ns];
    CFRelease(select_name_ns as *const c_void);
    let _: () = msg_send![
        doc,
        addSubview: select,
        positioned: -1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];
    release_obj(select);
    register_control(select, item);

    // A divider under every row, including the last: the footer band below draws its own top border,
    // so the last row's divider is what separates the rows from that band.
    let separator = SettingsRow::separator(doc, y + BINDING_ROW_H - 1.0, width);

    let row = BindingRow {
        item,
        keycaps,
        label,
        toggle,
        select,
        separator,
    };
    let tooltip_key = if master_on {
        "settings.binding_toggle"
    } else {
        "settings.tooltip_key_bindings_disabled"
    };
    for control in row.interactive() {
        SettingsRow::set_view_enabled_with_tooltip(control, master_on, Some(&t(tooltip_key)));
    }
    row
}

/// The footer band below the rows: a joined ＋ / − segmented control plus a caption naming what each
/// half does. Both halves act on the page's state, not on a row, so neither carries an item index.
///
/// The whole band lives in one container view (`ui.key_bindings_footer`). That is what makes a
/// rebuild safe: the renderer removes the previous container and builds a fresh one, so repeated
/// edits cannot accumulate footer views on top of each other.
///
/// A − with nothing selected has no target, so it says so by being disabled; ＋ is never disabled for
/// an empty list, because adding the first binding is exactly what the user needs there.
unsafe fn render_footer(
    cfg: &Config,
    ui: &mut SettingsUi,
    doc: *mut AnyObject,
    width: f64,
    rows_h: f64,
) {
    let has_selection = ui.key_binding_selected.is_some();
    let target = MENU_TARGET
        .lock()
        .unwrap()
        .map(|t| t.0)
        .unwrap_or(std::ptr::null_mut());
    let master_on = cfg.key_bindings.enabled;
    let y = BINDING_PAD_Y + rows_h;

    let container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
        NSPoint::new(0.0, y),
        NSSize::new(width, BINDING_FOOTER_H)
    )];
    // No separator inside the band: the band's top edge is where the rows end, and the row above
    // already drew its own divider. A second line here reads as an extra rule under the buttons.
    let _: () = msg_send![doc, addSubview: container];

    // The two glyphs are one joined control, its band vertically centred so the control shares the
    // rows' center line instead of hugging either edge.
    let control_y = footer_control_offset();
    let segments = [
        SegmentSpec {
            symbol: "plus",
            description: &t("settings.binding_add"),
        },
        SegmentSpec {
            symbol: "minus",
            description: &t("settings.binding_remove"),
        },
    ];
    let segments_control = SettingsActionSegments::create(
        BINDING_PAD_X,
        control_y,
        &segments,
        target,
        sel!(handleBindingFooterAction:),
    );
    if segments_control.is_null() {
        log_debug!("[keybind] footer segments unavailable");
        release_obj(container);
        return;
    }
    let _: () = msg_send![container, addSubview: segments_control];
    // `SettingsActionSegments::create` returns +1. The container retained it, so the creator's
    // reference is balanced here; without this every rebuild (each edit, each restore) leaks a
    // control. The page keeps the bare pointer, which stays valid while the container lives.
    release_obj(segments_control);

    // The caption: one line naming both halves, so the glyphs need no legend. It is chrome, never an
    // instruction the user must act on.
    let caption_x = BINDING_PAD_X + SettingsActionSegments::WIDTH + BINDING_COL_GAP + 4.0;
    let caption: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let caption: *mut AnyObject = msg_send![caption, initWithFrame: NSRect::new(
        NSPoint::new(caption_x, (BINDING_FOOTER_H - 18.0) / 2.0),
        NSSize::new((width - caption_x - BINDING_PAD_X).max(1.0), 18.0)
    )];
    set_field(caption, t("settings.binding_footer_hint"));
    let _: () = msg_send![caption, setBezeled: false];
    let _: () = msg_send![caption, setDrawsBackground: false];
    let _: () = msg_send![caption, setEditable: false];
    let _: () = msg_send![caption, setLineBreakMode: 4isize];
    let caption_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![caption, setFont: caption_font];
    apply_settings_text_role(caption, SettingsTextRole::Secondary);
    let _: () = msg_send![container, addSubview: caption];
    release_obj(caption);

    // `−` is armed by a selection; the master switch narrows that further (see
    // `update_binding_controls_enabled`), and `＋` is never disabled for an empty list because
    // adding the first binding is exactly what the user needs there.
    SettingsActionSegments::set_segment_enabled(
        segments_control,
        FOOTER_SEGMENT_REMOVE,
        has_selection,
    );
    if !master_on {
        for index in 0..segments.len() {
            SettingsActionSegments::set_segment_enabled(segments_control, index, false);
        }
    }
    // The document view owns the container now; the page keeps the bare pointers so a rebuild can
    // remove the band and the master switch can freeze its control.
    release_obj(container);
    ui.key_bindings_footer = container;
    ui.key_bindings_footer_segments = segments_control;
}

/// Freeze the row controls while the master switch is off, and enable them again when it is on.
pub(crate) unsafe fn update_binding_controls_enabled(ui: &SettingsUi) {
    let master_on = match CONFIG.read() {
        Ok(cfg) => cfg.key_bindings.enabled,
        Err(_) => return,
    };
    let enabled = t("settings.binding_toggle");
    let disabled = t("settings.tooltip_key_bindings_disabled");
    let tooltip = if master_on { &enabled } else { &disabled };
    for row in &ui.key_binding_rows {
        for control in row.interactive() {
            if !control.is_null() {
                SettingsRow::set_view_enabled_with_tooltip(control, master_on, Some(tooltip));
            }
        }
        if !row.label.is_null() {
            SettingsRow::set_view_enabled_with_tooltip(row.label, master_on, Some(tooltip));
        }
    }
    // The whole-control pass sets the control's own enabled state (it also drives the disabled-hint
    // tooltip); the per-segment calls below are what make the two halves differ, so they come last.
    // Measured 2026-10-13: `NSControl::setEnabled:` does NOT reset per-segment flags, so the order is
    // not load-bearing -- the per-segment calls simply have to be present.
    if !ui.key_bindings_footer_segments.is_null() {
        SettingsRow::set_view_enabled_with_tooltip(
            ui.key_bindings_footer_segments,
            master_on,
            Some(&disabled),
        );
        SettingsActionSegments::set_segment_enabled(
            ui.key_bindings_footer_segments,
            FOOTER_SEGMENT_ADD,
            master_on,
        );
        SettingsActionSegments::set_segment_enabled(
            ui.key_bindings_footer_segments,
            FOOTER_SEGMENT_REMOVE,
            master_on && ui.key_binding_selected.is_some(),
        );
    }
    // An open panel is one of this page's editors, so the master switch freezes it too: turning the
    // feature off from the status-bar menu must not leave a live OK button that writes a binding.
    update_panel_enabled();
}

/// The add panel's action popup: the same list, read off the panel's own control.
pub(crate) fn panel_popup_action() -> Option<Action> {
    let popup = PANEL_ACTION.lock().ok().and_then(|slot| *slot)?;
    let index: isize = unsafe { msg_send![popup.0, indexOfSelectedItem] };
    add_action_at(index.max(0) as usize)
}

/// The add panel: one floating window where the action is chosen and the chord recorded, so the page
/// itself stays a plain list (macOS System Settings' "＋ opens a small window" model, which is what
/// the mouse page's mapping panel established).
///
/// Ownership: the panel is created `alloc`/`init` (+1) and `setReleasedWhenClosed: false`, so
/// `close_binding_panel` is the only thing that releases it. Every view it stores is taken out of its
/// slot *before* that release, because a slot is a bare pointer and the panel deallocates its content
/// view's subtree with it. Each subview's own `+1` is balanced right after `addSubview`, so the
/// panel's release is the last reference and nothing else may release them.
pub(crate) fn open_binding_panel() {
    unsafe {
        // A recording in flight belongs to the panel that started it; reopening must not leave two.
        if PANEL.lock().unwrap().is_some() {
            close_binding_panel();
        }
        clear_draft();
        let target = match MENU_TARGET.lock().unwrap().map(|t| t.0) {
            Some(target) => target,
            None => return,
        };
        let win = match with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) {
            Some(win) if !win.is_null() => win,
            _ => return,
        };
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H));
        let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
        let panel: *mut AnyObject = msg_send![panel, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
        apply_settings_window_appearance(panel);
        let _: () = msg_send![panel, setReleasedWhenClosed: false];
        let _: () = msg_send![panel, setOpaque: false];
        let _: () = msg_send![panel, setLevel: 3isize]; // NSFloatingWindowLevel
        let clear_ns: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![panel, setBackgroundColor: clear_ns];
        let ve: *mut AnyObject = msg_send![class!(NSView), alloc];
        let ve: *mut AnyObject = msg_send![ve, initWithFrame: frame];
        let _: () = msg_send![ve, setWantsLayer: true];
        let ve_layer: *mut AnyObject = msg_send![ve, layer];
        // A dropdown/dialog panel takes `radius-panel`, not `radius-control` (style doc §6).
        let _: () = msg_send![ve_layer, setCornerRadius: crate::theme::RADIUS_PANEL];
        // The shadow is drawn outside the panel's bounds, so the layer must not clip to them.
        let _: () = msg_send![ve_layer, setMasksToBounds: false];
        layer_set_background(
            ve_layer,
            crate::ffi::hex_to_cg_color(settings_palette().card_bg),
        );
        let _: () = msg_send![ve_layer, setBorderWidth: 1.0f64];
        crate::ffi::layer_set_border(
            ve_layer,
            crate::ffi::hex_to_cg_color(settings_palette().card_border),
        );
        // A floating panel carries the medium elevation (style doc §7: popovers and dropdowns).
        crate::ffi::layer_set_shadow_color(
            ve_layer,
            crate::ffi::hex_to_cg_color(crate::theme::ELEVATION_MED_SHADOW_COLOR),
        );
        let _: () =
            msg_send![ve_layer, setShadowOpacity: crate::theme::ELEVATION_MED_SHADOW_OPACITY];
        let _: () = msg_send![ve_layer, setShadowRadius: crate::theme::ELEVATION_MED_SHADOW_RADIUS];
        let _: () = msg_send![ve_layer, setShadowOffset: NSSize::new(0.0, crate::theme::ELEVATION_MED_SHADOW_OFFSET_Y)];
        let _: () = msg_send![panel, setContentView: ve];
        release_obj(ve);

        // Action row.
        let action_label = make_label_with_role(
            PANEL_PAD,
            PANEL_ACTION_Y,
            PANEL_LABEL_W,
            PANEL_LABEL_H,
            &t("settings.binding_panel_action"),
            SettingsTextRole::Secondary,
        );
        // The shared label helper returns +1 and leaves attaching to the caller, so each label is
        // added and then balanced: the panel's content view owns it from here.
        let _: () = msg_send![ve, addSubview: action_label];
        release_obj(action_label);
        let titles: Vec<String> = picker_actions()
            .iter()
            .map(|action| crate::key_bindings::action_label(*action))
            .collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let popup = make_popup(
            PANEL_PAD + PANEL_LABEL_W,
            PANEL_ACTION_Y - 2.0,
            PANEL_W - PANEL_PAD * 2.0 - PANEL_LABEL_W,
            PANEL_ROW_H,
            &titles,
            0,
        );
        let _: () = msg_send![popup, setTarget: target];
        let _: () = msg_send![popup, setAction: sel!(handleBindingPanelAction:)];
        let _: () = msg_send![ve, addSubview: popup];
        release_obj(popup);
        *PANEL_ACTION.lock().unwrap() = Some(ObjPtr::new(popup));

        // Chord row: the recorded chord (or "not recorded yet") plus its record button.
        let chord_label = make_label_with_role(
            PANEL_PAD,
            PANEL_CHORD_Y,
            PANEL_LABEL_W,
            20.0,
            &t("settings.binding_panel_chord"),
            SettingsTextRole::Secondary,
        );
        let _: () = msg_send![ve, addSubview: chord_label];
        release_obj(chord_label);
        let chord_value = make_label_with_role(
            PANEL_PAD + PANEL_LABEL_W,
            PANEL_CHORD_Y,
            PANEL_W - PANEL_PAD * 2.0 - PANEL_LABEL_W - PANEL_RECORD_W - PANEL_BTN_GAP,
            20.0,
            &t("settings.binding_panel_no_chord"),
            SettingsTextRole::Primary,
        );
        let _: () = msg_send![ve, addSubview: chord_value];
        // Balance the helper's +1: the content view owns the label now, and the slot holds a bare
        // alias so `update_panel_ok` can rewrite its text.
        release_obj(chord_value);
        *PANEL_CHORD_LABEL.lock().unwrap() = Some(ObjPtr::new(chord_value));
        let record = SettingsButton::action(
            NSRect::new(
                NSPoint::new(PANEL_W - PANEL_PAD - PANEL_RECORD_W, PANEL_CHORD_Y - 4.0),
                NSSize::new(PANEL_RECORD_W, PANEL_BTN_H),
            ),
            &t("settings.binding_add_record"),
            target,
            sel!(handleBindingPanelRecord:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![ve, addSubview: record];
        release_obj(record);
        *PANEL_RECORD.lock().unwrap() = Some(ObjPtr::new(record));

        // The panel's own message line: a refused chord is reported here, where the user is looking,
        // instead of in a page row behind the dim layer.
        let message = make_label_with_role(
            PANEL_PAD,
            PANEL_MESSAGE_Y,
            PANEL_W - PANEL_PAD * 2.0,
            20.0,
            "",
            SettingsTextRole::Secondary,
        );
        let _: () = msg_send![ve, addSubview: message];
        release_obj(message);
        *PANEL_MESSAGE.lock().unwrap() = Some(ObjPtr::new(message));

        // Cancel / OK.
        let cancel = SettingsButton::action(
            NSRect::new(
                NSPoint::new(
                    PANEL_W - PANEL_PAD - PANEL_BTN_W * 2.0 - PANEL_BTN_GAP,
                    PANEL_BUTTON_Y,
                ),
                NSSize::new(PANEL_BTN_W, PANEL_BTN_H),
            ),
            &t("settings.recording_cancel"),
            target,
            sel!(handleBindingPanelCancel:),
            SettingsButtonRole::Footer,
        );
        let _: () = msg_send![ve, addSubview: cancel];
        release_obj(cancel);
        let ok = SettingsButton::action(
            NSRect::new(
                NSPoint::new(PANEL_W - PANEL_PAD - PANEL_BTN_W, PANEL_BUTTON_Y),
                NSSize::new(PANEL_BTN_W, PANEL_BTN_H),
            ),
            &t("settings.ok"),
            target,
            sel!(handleBindingPanelConfirm:),
            SettingsButtonRole::Primary,
        );
        let _: () = msg_send![ve, addSubview: ok];
        release_obj(ok);
        *PANEL_OK.lock().unwrap() = Some(ObjPtr::new(ok));
        *PANEL.lock().unwrap() = Some(ObjPtr::new(panel));

        // Centre on the settings window, then dim behind it (the panel floats above the dim).
        let win_frame: NSRect = msg_send![win, frame];
        let pf: NSRect = msg_send![panel, frame];
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(
            win_frame.origin.x + (win_frame.size.width - pf.size.width) / 2.0,
            win_frame.origin.y + (win_frame.size.height - pf.size.height) / 2.0
        )];
        let content: *mut AnyObject = msg_send![win, contentView];
        let content_bounds: NSRect = msg_send![content, bounds];
        let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
        let dim: *mut AnyObject = msg_send![dim, initWithFrame: content_bounds];
        let _: () = msg_send![dim, setWantsLayer: true];
        let dim_layer: *mut AnyObject = msg_send![dim, layer];
        layer_set_background(dim_layer, hex_to_cg_color(crate::theme::MODAL_SCRIM));
        let _: () = msg_send![content, addSubview: dim];
        release_obj(dim);
        *PANEL_DIM.lock().unwrap() = Some(ObjPtr::new(dim));
        let _: () = msg_send![panel, orderFrontRegardless];
    }
    update_panel_ok();
    log_debug!("[keybind] binding add panel opened");
}

/// Whether the add panel is open (the page switch uses this to close it).
pub(crate) fn panel_is_open() -> bool {
    PANEL.lock().map(|slot| slot.is_some()).unwrap_or(false)
}

/// Whether each panel control may act, given the master switch, the draft and the recorder.
///
/// One pure predicate owns the rule, and **both** the UI (`update_panel_enabled`) and the handlers
/// (`handle_panel_record`, `handle_panel_confirm`) read it, so a regression test exercises the same
/// decision the product makes instead of a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelGates {
    pub(crate) record: bool,
    pub(crate) ok: bool,
    pub(crate) popup: bool,
}

/// `master` = the feature switch, `draft` = a recorded chord is held, `recording` = the recorder is
/// currently running.
pub(crate) fn panel_gates(master: bool, draft: bool, recording: bool) -> PanelGates {
    PanelGates {
        record: master && !recording,
        // OK needs a chord to write *and* the recorder stopped: confirming mid-recording would write
        // a draft the recorder is about to replace.
        ok: master && draft && !recording,
        popup: master,
    }
}

/// Whether the master switch currently allows editing bindings at all.
pub(crate) fn master_switch_on() -> bool {
    CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false)
}

/// Freeze or unfreeze the add panel to match the master switch. Called from the switch handler and
/// from `update_panel_ok`, so a panel left open while the feature is turned off cannot write, and
/// turning it back on restores the panel without reopening it.
pub(crate) fn update_panel_enabled() {
    let gates = panel_gates(
        master_switch_on(),
        has_draft(),
        super::mapping::binding_recording_pending(),
    );
    if let Some(record) = PANEL_RECORD.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let tip = t("settings.tooltip_key_bindings_disabled");
            SettingsRow::set_view_enabled_with_tooltip(record.0, gates.record, Some(&tip));
        }
    }
    if let Some(ok) = PANEL_OK.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let tip = t("settings.tooltip_key_bindings_disabled");
            SettingsRow::set_view_enabled_with_tooltip(ok.0, gates.ok, Some(&tip));
            apply_panel_button_dim(ok.0, gates.ok);
        }
    }
    if let Some(popup) = PANEL_ACTION.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let _: () = msg_send![popup.0, setEnabled: gates.popup];
        }
    }
}

/// Refresh the chord row and the buttons after any change to the draft or the recorder.
pub(crate) fn update_panel_ok() {
    // The buttons' enabled state has one owner (`update_panel_enabled`), because it depends on the
    // master switch, the recorder and the draft together.
    update_panel_enabled();
    if let Some(label) = PANEL_CHORD_LABEL.lock().ok().and_then(|slot| *slot) {
        let text = match BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone()) {
            Some((_, trigger)) => Trigger::parse(&trigger)
                .map(|parsed| parsed.keycap())
                .unwrap_or(trigger),
            None => t("settings.binding_panel_no_chord"),
        };
        unsafe { set_field(label.0, text) };
    }
}

/// Close the add panel and release it (idempotent). The recording, if any, is cancelled first so no
/// late result can reach a panel that is gone.
pub(crate) fn close_binding_panel() {
    // Cancel outside the slot-taking below: the recorder's cleanup marshals to this same thread and
    // must not observe a half-dismantled panel.
    if super::mapping::binding_recording_pending() {
        super::mapping::cancel_recording_from_main();
    }
    let panel = PANEL.lock().unwrap().take();
    let action = PANEL_ACTION.lock().unwrap().take();
    let chord_label = PANEL_CHORD_LABEL.lock().unwrap().take();
    let record = PANEL_RECORD.lock().unwrap().take();
    let message = PANEL_MESSAGE.lock().unwrap().take();
    let ok = PANEL_OK.lock().unwrap().take();
    let dim = PANEL_DIM.lock().unwrap().take();
    // Every view registered in an address-keyed table is unregistered while it is still alive: those
    // tables outlive the views, so a stale key either reaches freed memory or -- once a new view
    // reuses the address -- hands a new view's state to an old entry.
    unsafe {
        if let Some(popup) = action {
            super::select::forget_settings_select(popup.0);
        }
        for view in [record, ok].into_iter().flatten() {
            if !view.0.is_null() {
                SettingsRow::forget(view.0);
            }
        }
    }
    // The slots are empty now; the handles themselves are Copy, so this is a plain discard.
    let _ = (action, chord_label, record, message, ok);
    unsafe {
        if let Some(d) = dim {
            let _: () = msg_send![d.0, removeFromSuperview];
        }
        if let Some(p) = panel {
            let _: () = msg_send![p.0, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(p.0);
        }
    }
    clear_draft();
}

/// Show a styled panel button as disabled or usable.
///
/// These buttons carry their own layer fill, and `setEnabled:` does not dim a layer fill, so a
/// disabled primary button keeps its full accent and reads as pressable. The design system's
/// disabled rule is half opacity on the control's fill (style doc §10), applied here to the panel's
/// own two buttons rather than to every styled button in the app.
unsafe fn apply_panel_button_dim(button: *mut AnyObject, enabled: bool) {
    if button.is_null() {
        return;
    }
    let _: () = msg_send![button, setAlphaValue: if enabled {
        1.0f64
    } else {
        crate::theme::DISABLED_FILL_OPACITY
    }];
}

/// The panel's message line: a refusal, or empty.
pub(crate) fn set_panel_message(message: Option<&str>) {
    let Some(label) = PANEL_MESSAGE.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    let text = message.unwrap_or("").to_string();
    unsafe {
        set_field(label.0, &text);
        let role = if message.is_some() {
            SettingsTextRole::Destructive
        } else {
            SettingsTextRole::Secondary
        };
        apply_settings_text_role(label.0, role);
    }
    if let Some(message) = message {
        crate::log_info!("[keybind] recording refused: {}", message);
    }
}

/// The add panel "Record chord": start the recorder for the action the panel has selected.
pub(crate) extern "C" fn handle_panel_record(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // Same predicate as the button's own enablement: a click can arrive after the switch changed.
    if !panel_gates(
        master_switch_on(),
        has_draft(),
        super::mapping::binding_recording_pending(),
    )
    .record
    {
        return;
    }
    // The chord must be recorded for the action the panel is showing *now*; a stale draft from an
    // earlier action is dropped rather than confirmed against the new one.
    clear_draft_state();
    set_panel_message(None);
    let Some(action) = panel_popup_action() else {
        return;
    };
    unsafe { super::mapping::start_key_binding_recording(action) };
    update_panel_ok();
}

/// The panel's action popup changed: any recorded chord belonged to the previous action, so the
/// draft is dropped and the chord row goes back to its empty text.
pub(crate) extern "C" fn handle_panel_action_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if has_draft() {
        clear_draft_state();
        set_panel_message(None);
    }
    update_panel_ok();
}

/// The panel "OK": write the draft.
pub(crate) extern "C" fn handle_panel_confirm(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // Re-read here, not trusted from the button's state: a config reload can turn the feature off
    // between the render and the click. Same predicate as the button's enablement.
    if !panel_gates(
        master_switch_on(),
        has_draft(),
        super::mapping::binding_recording_pending(),
    )
    .ok
    {
        return;
    }
    let Some((action, trigger)) = BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone()) else {
        return;
    };
    // Re-validate against the config as it is now, and against the action the popup is showing: the
    // draft is only worth writing while those two still agree.
    if panel_popup_action() != Some(action) {
        clear_draft_state();
        update_panel_ok();
        return;
    }
    match add_binding(action, &trigger) {
        Ok(()) => close_binding_panel(),
        Err(error) => set_panel_message(Some(&error)),
    }
}

/// The panel "Cancel".
pub(crate) extern "C" fn handle_panel_cancel(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    close_binding_panel();
}

/// The action at one popup row, in the order the picker builds its menu.
pub(crate) fn add_action_at(index: usize) -> Option<Action> {
    picker_actions().get(index).copied()
}

/// Every bindable action, in picker order (the registry order grouped for reading).
pub(crate) fn picker_actions() -> Vec<Action> {
    crate::key_bindings::ALL.to_vec()
}

/// Append a binding for `action` with a freshly recorded `trigger`, or report why it was refused.
///
/// The chord is checked against the effective config before it is written, so a refused recording
/// changes nothing: the same reserved-chord rules the config loader enforces apply here, and the
/// user hears about it in the add row instead of finding a binding missing after a reload.
pub(crate) fn add_binding(action: Action, trigger: &str) -> Result<(), String> {
    let parsed = Trigger::parse(trigger).map_err(|_| {
        tf(
            "errors.key_binding_trigger_invalid",
            &[("value", trigger.trim())],
        )
    })?;
    if parsed.is_bare_key() {
        return Err(tf(
            "errors.key_binding_trigger_bare",
            &[("value", &parsed.describe())],
        ));
    }
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let items = old.key_bindings.items.clone().unwrap_or_default();
    if crate::key_bindings::trigger_is_taken(&items, parsed) {
        return Err(tf(
            "errors.key_binding_trigger_duplicate",
            &[("value", &parsed.describe()), ("index", "")],
        ));
    }
    let reserved = crate::key_bindings::Reserved::from_effective(
        &old.keyboard.modifier,
        &old.clipboard.shortcut,
    );
    let mut probe = items.clone();
    probe.push(crate::key_bindings::KeyBinding {
        action: action.config_id().to_string(),
        trigger: parsed.describe(),
        enabled: true,
    });
    if let Some(last) = probe.last().cloned() {
        let mut only_last = vec![last];
        if !crate::key_bindings::drop_reserved_conflicts(&mut only_last, &reserved).is_empty() {
            return Err(crate::key_bindings::reserved_trigger_message(
                &parsed.describe(),
            ));
        }
    }
    let mut updated = old.clone();
    updated
        .key_bindings
        .items
        .get_or_insert_with(Vec::new)
        .push(crate::key_bindings::KeyBinding {
            action: action.config_id().to_string(),
            trigger: parsed.describe(),
            enabled: true,
        });
    // The apply pass inside `write_binding_config` re-renders the rows (their shape changed), so
    // the registry is rebuilt before the next click can reach a stale index.
    write_binding_config(&old, updated);
    Ok(())
}

/// Delete the binding at `item`, keeping every other entry (and its order) untouched.
///
/// The selection moves to the row that takes its place (or to the new last row when the deleted one
/// was last), so − stays usable for a run of deletions; an emptied list clears it, which is what
/// disables −. `write_binding_config` rebuilds the rows, and the rebuild clamps whatever this sets.
fn delete_binding(item: usize) {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let mut updated = old.clone();
    let Some(items) = updated.key_bindings.items.as_mut() else {
        return;
    };
    if item >= items.len() {
        return;
    }
    items.remove(item);
    let next = selection_after_delete(item, items.len() + 1);
    // The write goes through the ordinary config path, and that path deliberately drops the
    // selection whenever the list's shape changed -- a deletion is exactly such a change, so the
    // neighbour is restored *afterwards*. Setting it before the write would be undone by the sync
    // that the write itself triggers (the row count changed, so `sync_from_config` clears it).
    write_binding_config(&old, updated);
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_mut() else {
            return;
        };
        ui.key_binding_selected = clamp_selection(next, items_len_now());
        unsafe {
            repaint_selection(ui, items_len_now());
            update_footer_remove_enabled(ui);
        }
    });
}

/// The current binding count, for the selection refresh after an edit.
fn items_len_now() -> usize {
    CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.items.as_ref().map_or(0, Vec::len))
        .unwrap_or(0)
}

/// Toggle the binding at `item`.
fn toggle_binding(item: usize, enabled: bool) {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let mut updated = old.clone();
    let Some(items) = updated.key_bindings.items.as_mut() else {
        return;
    };
    let Some(binding) = items.get_mut(item) else {
        return;
    };
    if binding.enabled == enabled {
        return;
    }
    binding.enabled = enabled;
    write_binding_config(&old, updated);
}

/// Write an edited binding list and make it live: in-memory config, the usual apply pass (which
/// republishes the engine's table and starts or stops the tap), a debounced persist, and a rebuild
/// of the rows.
fn write_binding_config(old: &Config, updated: Config) {
    if let Ok(mut config) = CONFIG.write() {
        *config = updated.clone();
    }
    schedule_config_persist();
    apply_config_change(old, &updated, ConfigChangeSource::Settings);
    log_debug!(
        "[keybind] bindings edited: {} entr(ies)",
        updated
            .key_bindings
            .items
            .as_ref()
            .map(Vec::len)
            .unwrap_or(0)
    );
}

/// The add panel's message line: an error from a refused recording, or empty.
pub(crate) fn set_add_message(message: Option<&str>) {
    set_panel_message(message);
}

/// A recording that was capturing for this page is over without a chord to confirm (cancelled, or
/// stopped by a permission revocation on another thread, which marshals here): drop the draft and
/// put the panel back in its "no chord yet" state.
pub(crate) fn recording_ended() {
    clear_draft();
}

/// The recording captured a chord: hold it as a draft until the panel's OK writes it.
pub(crate) fn recording_captured(action: Action, trigger: &str) -> Result<(), String> {
    // Validate before asking: telling the user now is kinder than after they press OK, and the
    // confirm step re-validates against the config as it is at that moment.
    if let Err(error) = validate_draft(action, trigger) {
        clear_draft_state();
        set_panel_message(Some(&error));
        update_panel_ok();
        return Err(error);
    }
    if let Ok(mut draft) = BINDING_DRAFT.lock() {
        *draft = Some((action, trigger.to_string()));
    }
    // The panel names both halves before OK writes them, so the user sees what they are agreeing to.
    show_draft(action, trigger);
    Ok(())
}

/// Whether a recorded chord is waiting for confirmation.
pub(crate) fn has_draft() -> bool {
    BINDING_DRAFT
        .lock()
        .map(|draft| draft.is_some())
        .unwrap_or(false)
}

/// Drop the draft and put the panel's controls back to their "nothing recorded yet" state.
fn clear_draft() {
    clear_draft_state();
    set_panel_message(None);
    update_panel_ok();
}

/// Write the draft, or report why it was refused (keeping it so the user can cancel or retry).
fn confirm_draft() {
    let Some((action, trigger)) = BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone()) else {
        return;
    };
    match add_binding(action, &trigger) {
        Ok(()) => clear_draft(),
        Err(error) => set_add_message(Some(&error)),
    }
}

/// Forget the draft without touching the UI (the UI half is `clear_draft`).
fn clear_draft_state() {
    if let Ok(mut draft) = BINDING_DRAFT.lock() {
        *draft = None;
    }
}

/// Show the draft in the panel: the chord row shows the recorded chord and the message line names
/// both halves, so the user can see what OK would write.
fn show_draft(action: Action, trigger: &str) {
    let sentence = tf(
        "settings.binding_draft",
        &[
            (
                "trigger",
                &Trigger::parse(trigger)
                    .map(|t| t.keycap())
                    .unwrap_or_else(|_| trigger.to_string()),
            ),
            ("action", &crate::key_bindings::action_label(action)),
        ],
    );
    set_panel_message(Some(&sentence));
    update_panel_ok();
}

/// The validation the record and confirm steps share: parseable, not a bare key, not already
/// taken, and not reserved. Pure over the effective config's binding list and reserved set, so the
/// add flow refuses exactly what the config loader would drop.
fn draft_rejection(
    action: Action,
    trigger: &str,
    items: &[crate::key_bindings::KeyBinding],
    reserved: &crate::key_bindings::Reserved,
) -> Option<String> {
    let parsed = match Trigger::parse(trigger) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Some(tf(
                "errors.key_binding_trigger_invalid",
                &[("value", trigger.trim())],
            ))
        }
    };
    if parsed.is_bare_key() {
        return Some(tf(
            "errors.key_binding_trigger_bare",
            &[("value", &parsed.describe())],
        ));
    }
    if crate::key_bindings::trigger_is_taken(items, parsed) {
        return Some(tf(
            "errors.key_binding_trigger_duplicate",
            &[("value", &parsed.describe()), ("index", "")],
        ));
    }
    // The reserved check runs on the entry that would be written, so a chord the switcher or the
    // clipboard shortcut owns is refused here rather than silently dropped at the next load.
    let mut probe = vec![crate::key_bindings::KeyBinding {
        action: action.config_id().to_string(),
        trigger: parsed.describe(),
        enabled: true,
    }];
    if !crate::key_bindings::drop_reserved_conflicts(&mut probe, reserved).is_empty() {
        return Some(crate::key_bindings::reserved_trigger_message(
            &parsed.describe(),
        ));
    }
    None
}

/// The same check against the live config.
fn validate_draft(action: Action, trigger: &str) -> Result<(), String> {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let items = old.key_bindings.items.clone().unwrap_or_default();
    let reserved = crate::key_bindings::Reserved::from_effective(
        &old.keyboard.modifier,
        &old.clipboard.shortcut,
    );
    match draft_rejection(action, trigger, &items, &reserved) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// Row switch: turn one binding on or off.
pub(crate) extern "C" fn handle_binding_toggle(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let sender = sender as *mut AnyObject;
    let Some(item) = control_item(sender) else {
        return;
    };
    let state: isize = unsafe { msg_send![sender, state] };
    toggle_binding(item, state == 1);
}

/// The footer's joined ＋ / − control: segment 0 adds, segment 1 removes.
///
/// One selector rather than two, because that is what the native control gives: it reports which
/// segment was clicked and the two halves are the same control. The index is mapped here, so the
/// segments' meaning lives in one place instead of in two target/action pairs.
pub(crate) extern "C" fn handle_binding_footer_action(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    // `−` is gated by a selection and by the master switch; the control disables the segment, but a
    // keyboard or programmatic invocation can still arrive, so the rule is checked here too.
    match unsafe { SettingsActionSegments::selected_segment(sender as *mut AnyObject) } {
        Some(FOOTER_SEGMENT_ADD) => open_binding_panel(),
        Some(FOOTER_SEGMENT_REMOVE) => remove_selected_binding(),
        // "No segment selected" and any index we do not own: nothing to do.
        _ => {}
    }
}

/// A row's click surface: make that row the selection.
pub(crate) extern "C" fn handle_binding_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let Some(item) = control_item(sender as *mut AnyObject) else {
        return;
    };
    select_binding(item);
}

/// Delete the selected binding, refusing a stale index.
///
/// The target is re-read from the config at click time rather than trusted from the highlight: the
/// index is only meaningful against the list that is current *now*, so this is the second check the
/// shape-invalidation rule needs.
fn remove_selected_binding() {
    let Some(item) = with_settings_ui(|ui| ui.as_ref().and_then(|ui| ui.key_binding_selected))
    else {
        return;
    };
    let len = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.items.as_ref().map_or(0, Vec::len))
        .unwrap_or(0);
    if item >= len {
        // The list shrank since the selection was drawn: drop it rather than delete a stranger.
        clear_selection();
        return;
    }
    log_debug!("[keybind] removing selected row {}", item);
    delete_binding(item);
}

/// "Add": write the recorded draft.
pub(crate) extern "C" fn handle_binding_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    confirm_draft();
}

/// "Cancel": drop the draft and go back to the record button.
pub(crate) extern "C" fn handle_binding_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    clear_draft();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the add flow refuses, and why: the same rules the config loader enforces, so a chord
    /// refused here is one a reload would have dropped anyway.
    #[test]
    fn the_add_flow_refuses_bare_duplicate_and_reserved_chords() {
        use crate::key_bindings::KeyBinding;
        let existing = vec![KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "option+e".into(),
            enabled: true,
        }];
        let reserved = crate::key_bindings::Reserved::from_effective("command", "option+v");
        let check = |trigger: &str| draft_rejection(Action::Center, trigger, &existing, &reserved);

        assert!(check("cmd+ctrl+c").is_none());
        assert!(check("option+e").is_some(), "already bound");
        assert!(check("e").is_some(), "a bare key is refused");
        assert!(check("shift+e").is_some(), "shift alone is still typing");
        assert!(check("not-a-trigger").is_some());
        assert!(check("cmd+tab").is_some(), "the switcher chord");
        assert!(check("option+v").is_some(), "the clipboard chord");
    }

    /// The draft is state the cancellation paths can see and clear without a window: that is what
    /// lets a window close, a permission revocation or a page switch clear a chord the user
    /// recorded but never confirmed.
    #[test]
    fn a_draft_is_visible_and_clearable_without_ui() {
        clear_draft_state();
        assert!(!has_draft());
        if let Ok(mut draft) = BINDING_DRAFT.lock() {
            *draft = Some((Action::OpenFinder, "option+e".into()));
        }
        assert!(has_draft());
        clear_draft_state();
        assert!(!has_draft());
    }

    /// Every editor on the page is gated by the master switch, and the panel is one of them: a
    /// switch that freezes the rows and the footer but left a live OK button would let a binding be
    /// written with the feature off. This pins the rule the two paths share — OK needs the switch
    /// *and* a draft and a stopped recorder; the record button needs the switch and a stopped
    /// recorder; the action popup needs only the switch.
    #[test]
    fn the_master_switch_gates_every_panel_control() {
        // (master_on, has_draft, recording) -> (record, ok, popup)
        // The production predicate decides this, not a copy of it: deleting its master-switch check
        // fails these assertions.
        let gate = |master: bool, draft: bool, recording: bool| {
            let gates = panel_gates(master, draft, recording);
            (gates.record, gates.ok, gates.popup)
        };
        // Feature off: nothing in the panel may act, whatever else is true.
        assert_eq!(gate(false, true, false), (false, false, false));
        assert_eq!(gate(false, true, true), (false, false, false));
        // Feature on, no chord yet: the popup and the record button work, OK does not.
        assert_eq!(gate(true, false, false), (true, false, true));
        // Feature on, recorder running: neither the record button nor OK may run again.
        assert_eq!(gate(true, false, true), (false, false, true));
        assert_eq!(gate(true, true, true), (false, false, true));
        // Feature on, a chord is held and the recorder stopped: everything is usable.
        assert_eq!(gate(true, true, false), (true, true, true));
    }

    /// The card is exactly its rows, one footer band and the bottom inset. The card's height comes
    /// from production code, so the assertion is against that function rather than a copy of its
    /// arithmetic.
    #[test]
    fn the_card_reserves_one_footer_band() {
        let rows_h = BINDING_ROW_H;
        // The card is taller than its rows by exactly the footer band plus the bottom inset: the
        // delta is what the canvas is handed, so it is asserted as a delta rather than as a
        // restatement of the function.
        assert_eq!(
            card_height(rows_h) - rows_h,
            BINDING_FOOTER_H + BINDING_PAD_Y,
            "the card must reserve exactly one footer band and the bottom inset beyond its rows"
        );
    }

    /// The vertical placement the footer uses for its control.
    ///
    /// This is the production rule, not a restatement of it: the renderer positions the control with
    /// this offset from the band's top, so a change here moves the control and nothing else. The
    /// defect it fixes was an edge-anchored coordinate that left 5pt above the buttons and 15pt
    /// below; deriving the offset as half the slack makes the two sides equal by construction.
    #[test]
    fn the_footer_offset_centres_the_control_in_its_band() {
        assert_eq!(
            footer_control_offset(),
            BINDING_FOOTER_PAD,
            "the control's offset must be the band's padding"
        );
        // Falsify the old rule explicitly: the offset is NOT measured from the band's bottom edge,
        // which is what put the buttons too high (44 - 10 - 24 = 10 from the bottom vs 10 from the
        // top only when the two agree; the old code used 15 from the bottom).
        let from_bottom =
            BINDING_FOOTER_H - footer_control_offset() - SettingsActionSegments::HEIGHT;
        assert_eq!(
            from_bottom,
            footer_control_offset(),
            "the space above and below the control must be equal"
        );
    }

    /// The footer is ONE control, not two buttons: the two glyphs are the two segments of a single
    /// native control, which is what gives them one outer border and a shared corner radius.
    #[test]
    fn the_footer_is_one_joined_control() {
        // The joined control's width is exactly its two cells, so the caption after it starts at a
        // position that cannot drift from the control it follows.
        assert_eq!(
            SettingsActionSegments::WIDTH,
            SettingsActionSegments::CELL_W * 2.0
        );
        // Two segments, addressed by index: 0 adds, 1 removes.
        assert_eq!(FOOTER_SEGMENT_ADD, 0);
        assert_eq!(FOOTER_SEGMENT_REMOVE, 1);
    }

    /// The table's inner padding is the shared card padding. If a layout token moves, this fails
    /// instead of letting the bindings list drift out of line with every other card.
    #[test]
    fn padding_matches_the_shared_layout() {
        let layout = SettingsLayout::new(512.0);
        assert_eq!(BINDING_PAD_X, layout.label_x);
        assert_eq!(BINDING_PAD_Y, layout.card_bottom_inset);
    }

    /// The selection is an index into a list that changes size, so the two rules that keep − aimed at
    /// the row the user picked are pure and pinned here: deleting selects the row that took the
    /// deleted one's place (or the new last), and an emptied list clears the selection outright.
    #[test]
    fn deleting_a_row_moves_the_selection_to_a_row_that_exists() {
        // Deleting from the middle: the following row slides into the gap and is selected.
        assert_eq!(selection_after_delete(1, 3), Some(1));
        // Deleting the last row: there is no following row, so the new last is selected.
        assert_eq!(selection_after_delete(2, 3), Some(1));
        // Deleting the only row: nothing is left to select, and − must go dead.
        assert_eq!(selection_after_delete(0, 1), None);
        assert_eq!(selection_after_delete(0, 0), None);
    }

    /// A stale index must never be used to draw a highlight or to aim −: rendering clamps it to the
    /// list it is drawn against, and an empty list clears it.
    #[test]
    fn a_stale_selection_index_is_clamped_or_cleared() {
        assert_eq!(clamp_selection(Some(0), 2), Some(0));
        assert_eq!(clamp_selection(Some(1), 2), Some(1));
        // The list shrank under the selection: aim at the new last row, not past the end.
        assert_eq!(clamp_selection(Some(7), 3), Some(2));
        // An empty list has no row to select.
        assert_eq!(clamp_selection(Some(0), 0), None);
        assert_eq!(clamp_selection(None, 3), None);
    }

    /// A config edit that changes the list's *shape* must drop the selection: the index no longer
    /// means what it did, so keeping it would let − delete a binding the user never chose. A
    /// switch-only edit keeps it, which is what keeps − usable while the user flips rows.
    #[test]
    fn only_a_shape_change_invalidates_the_selection() {
        // The shape is the rendered identity: action + trigger per row, in order.
        let shape = |rows: &[(&str, &str, bool)]| {
            rows.iter()
                .map(|(action, trigger, _enabled)| (action.to_string(), trigger.to_string()))
                .collect::<Vec<_>>()
        };
        let base = shape(&[("quick.open_finder", "option+e", true)]);
        // A switch flip: same shape, so the selection survives.
        let flipped = shape(&[("quick.open_finder", "option+e", false)]);
        assert!(!shape_changed(&base, &flipped));
        // A reorder, an insertion and a removal all change the shape.
        let reordered = shape(&[
            ("window.snap_left", "option+left", true),
            ("quick.open_finder", "option+e", true),
        ]);
        assert!(shape_changed(&base, &reordered));
        let mut removed = base.clone();
        removed.clear();
        assert!(shape_changed(&base, &removed));
        // A re-chorded row is a shape change too: the row's identity moved with its trigger.
        let rekeyed = shape(&[("quick.open_finder", "option+g", true)]);
        assert!(shape_changed(&base, &rekeyed));
    }

    /// Flipping a row's switch must not change the list's shape: a rebuild there would destroy the
    /// control that is dispatching its own action. Adding, deleting, or changing a chord does.
    #[test]
    fn the_list_shape_ignores_the_per_row_switch() {
        let binding =
            |action: &str, trigger: &str, enabled: bool| crate::key_bindings::KeyBinding {
                action: action.to_string(),
                trigger: trigger.to_string(),
                enabled,
            };
        let base = vec![
            binding("quick.open_finder", "option+e", true),
            binding("window.snap_left", "option+left", true),
        ];
        let flipped = vec![
            binding("quick.open_finder", "option+e", false),
            binding("window.snap_left", "option+left", true),
        ];
        assert_eq!(binding_list_shape(&base), binding_list_shape(&flipped));
        let mut removed = base.clone();
        removed.remove(0);
        assert_ne!(binding_list_shape(&base), binding_list_shape(&removed));
        let mut rekeyed = base.clone();
        rekeyed[0].trigger = "option+g".into();
        assert_ne!(binding_list_shape(&base), binding_list_shape(&rekeyed));
        let mut added = base.clone();
        added.push(binding("window.center", "cmd+ctrl+c", true));
        assert_ne!(binding_list_shape(&base), binding_list_shape(&added));
    }

    /// One row of height is reserved even when a group has no bindings: the empty-state hint lives
    /// in that band, and a zero-height card would clip it away.
    #[test]
    fn an_empty_group_still_has_one_row_of_height() {
        let rows_h = (BINDING_PAD_Y - BINDING_PAD_Y).max(BINDING_ROW_H);
        assert_eq!(rows_h, BINDING_ROW_H);
        assert_eq!(rows_h + BINDING_PAD_Y * 2.0, BINDING_ROW_H + 16.0);
    }
}
