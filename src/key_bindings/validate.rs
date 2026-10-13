//! Pure validation for the binding list.
//!
//! Two passes, because the chords other global input paths own are only known once the rest of the
//! config has settled:
//!
//! * `validate_items` runs with the rest of `Config::validate`: it checks that each entry names a
//!   known action, parses as a trigger, is not a bare key, and does not repeat another usable
//!   entry's trigger. Entries that fail are reported by path and dropped by `merge_valid`, one by
//!   one, so a single bad entry cannot discard its siblings.
//! * `drop_reserved_conflicts` runs after per-field fallback, against the **effective** config: an
//!   invalid `clipboard.shortcut` has already fallen back to its default there, so a binding that
//!   only collides with the fallback value is still caught.
//!
//! The reserved set is computed from those effective values — never from a hardcoded list — so the
//! app switcher and the clipboard shortcut have exactly one definition each.

use crate::i18n::{t, tf};
use crate::mouse::shortcut::{self, Shortcut};

use super::action::Action;
use super::trigger::{tab_keycode, Trigger, MODIFIER_MASK};

/// The shortcuts other global input paths own, as the effective config describes them.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reserved {
    /// `keyboard.modifier` as a flag: that modifier plus Tab belongs to the app switcher, and
    /// extra modifiers do not change that (the switcher's own match is `flag & mod_mask != 0`).
    pub(crate) switcher_modifier: Option<u32>,
    /// The effective clipboard panel chord, matched exactly (same key, same modifier set).
    pub(crate) clipboard_chord: Option<Shortcut>,
}

/// Why a clipboard shortcut is unusable. Mirrors the config's two clipboard error keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardStatus {
    Invalid,
    Conflict,
}

impl Reserved {
    /// Build the reserved set from the effective config values.
    pub(crate) fn from_effective(keyboard_modifier: &str, clipboard_shortcut: &str) -> Self {
        Self {
            switcher_modifier: super::trigger::modifier_flag(keyboard_modifier),
            clipboard_chord: shortcut::parse_shortcut(clipboard_shortcut).ok(),
        }
    }

    /// The reason a chord is reserved, or `None` when it is free.
    fn chord_conflict(&self, chord: Shortcut) -> Option<&'static str> {
        if chord.keycode == tab_keycode() {
            if let Some(flag) = self.switcher_modifier {
                if chord.flags & flag != 0 {
                    return Some("errors.key_binding_trigger_reserved");
                }
            }
        }
        if let Some(clipboard) = self.clipboard_chord {
            if chord.keycode == clipboard.keycode
                && (chord.flags & MODIFIER_MASK) == (clipboard.flags & MODIFIER_MASK)
            {
                return Some("errors.key_binding_trigger_reserved");
            }
        }
        None
    }

    /// The reason a trigger is reserved, or `None` when it is free. Gestures are never reserved:
    /// they are observed through `flagsChanged` and never swallowed.
    fn conflict(&self, trigger: Trigger) -> Option<&'static str> {
        trigger.chord().and_then(|chord| self.chord_conflict(chord))
    }
}

/// Every parsed, non-bare trigger the list claims, whatever the row or group switches say: a
/// disabled binding is still a chord its owner may switch on later, so a new binding (or a new
/// clipboard shortcut) must not silently steal it.
///
/// Only an entry that is itself usable claims its chord. An unknown action is dropped by
/// `merge_valid` once validation is over, so letting it reserve a chord here would make one broken
/// entry push a perfectly good clipboard shortcut off its own chord — the per-field fallback this
/// module exists to keep. Bare-key triggers are excluded for the same reason.
fn claimed_triggers(items: &[super::KeyBinding]) -> Vec<Trigger> {
    items
        .iter()
        .filter(|item| super::Action::from_config_id(&item.action).is_some())
        .filter_map(|item| Trigger::parse(&item.trigger).ok())
        .filter(|trigger| !trigger.is_bare_key())
        .collect()
}

/// Whether a clipboard shortcut may be used, and why not when it may not.
pub(crate) fn clipboard_shortcut_status(
    desc: &str,
    items: &[super::KeyBinding],
    switcher_modifier: Option<u32>,
) -> Result<Shortcut, ClipboardStatus> {
    let chord = shortcut::parse_shortcut(desc).map_err(|_| ClipboardStatus::Invalid)?;
    let reserved = Reserved {
        switcher_modifier,
        // The clipboard's own chord is not a conflict with itself.
        clipboard_chord: None,
    };
    if reserved.chord_conflict(chord).is_some()
        || claimed_triggers(items)
            .iter()
            .any(|trigger| trigger.chord() == Some(chord))
    {
        return Err(ClipboardStatus::Conflict);
    }
    Ok(chord)
}

/// Whether a trigger is already claimed by a binding in the list. The settings panel asks this
/// before it records, so the user is told at once instead of after the config round-trip.
pub(crate) fn trigger_is_taken(items: &[super::KeyBinding], trigger: Trigger) -> bool {
    claimed_triggers(items).contains(&trigger)
}

/// Per-entry errors for the whole list. Empty means every entry is usable and uncollided.
pub(crate) fn validate_items(items: Option<&Vec<super::KeyBinding>>) -> Vec<String> {
    let Some(items) = items else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    let mut claimed: Vec<(Trigger, usize)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let field = |name: &str| format!("key_bindings.items[{index}].{name}");
        if Action::from_config_id(&item.action).is_none() {
            errors.push(format!(
                "{}: {}",
                field("action"),
                tf(
                    "errors.key_binding_action_unknown",
                    &[("value", item.action.trim())]
                )
            ));
            continue;
        }
        let trigger = match Trigger::parse(&item.trigger) {
            Ok(trigger) => trigger,
            Err(_) => {
                errors.push(format!(
                    "{}: {}",
                    field("trigger"),
                    tf(
                        "errors.key_binding_trigger_invalid",
                        &[("value", item.trigger.trim())]
                    )
                ));
                continue;
            }
        };
        if trigger.is_bare_key() {
            errors.push(format!(
                "{}: {}",
                field("trigger"),
                tf(
                    "errors.key_binding_trigger_bare",
                    &[("value", &trigger.describe())]
                )
            ));
            continue;
        }
        // Only entries that are usable themselves take part in duplicate detection: a dropped
        // entry must not make its valid twin look like a duplicate.
        if let Some((_, first_index)) = claimed.iter().find(|(seen, _)| *seen == trigger) {
            errors.push(format!(
                "{}: {}",
                field("trigger"),
                tf(
                    "errors.key_binding_trigger_duplicate",
                    &[
                        ("value", &trigger.describe()),
                        ("index", &(first_index + 1).to_string())
                    ]
                )
            ));
            continue;
        }
        claimed.push((trigger, index));
    }
    errors
}

/// Drop the entries whose trigger an effective reserved shortcut owns, reporting each one. Returns
/// the errors so the caller can persist the repair and surface it.
pub(crate) fn drop_reserved_conflicts(
    items: &mut Vec<super::KeyBinding>,
    reserved: &Reserved,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut kept = Vec::with_capacity(items.len());
    for (index, item) in std::mem::take(items).into_iter().enumerate() {
        let conflict = Trigger::parse(&item.trigger)
            .ok()
            .and_then(|trigger| reserved.conflict(trigger));
        match conflict {
            Some(key) => errors.push(format!("key_bindings.items[{index}].trigger: {}", t(key))),
            None => kept.push(item),
        }
    }
    *items = kept;
    errors
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

    fn conflicts(action: &str, trigger: &str) -> Vec<String> {
        let mut items = vec![item(action, trigger)];
        drop_reserved_conflicts(&mut items, &Reserved::from_effective("command", "option+v"))
    }

    #[test]
    fn entries_are_checked_field_by_field() {
        let items = vec![
            item("window.nope", "option+e"),
            item("quick.open_finder", "not-a-trigger"),
            item("quick.open_finder", "e"),
        ];
        let errors = validate_items(Some(&items));
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].starts_with("key_bindings.items[0].action"));
        assert!(errors[1].starts_with("key_bindings.items[1].trigger"));
        assert!(errors[2].starts_with("key_bindings.items[2].trigger"));
    }

    #[test]
    fn an_unusable_entry_does_not_claim_its_chord_from_the_clipboard() {
        // The second entry is dropped by `merge_valid`, so its chord is free: letting it reserve
        // Option+E would push a perfectly good clipboard shortcut off it -- one broken entry
        // breaking a second, valid field.
        let items = vec![
            item("window.nope", "option+e"),
            item("quick.open_finder", "option+j"),
        ];
        assert_eq!(
            clipboard_shortcut_status("option+e", &items, None),
            Ok(crate::mouse::shortcut::parse_shortcut("option+e").unwrap()),
            "an unknown action must not hold a chord the clipboard wants"
        );
        // The same rule for a trigger that cannot be parsed at all.
        let items = vec![item("quick.open_finder", "not-a-trigger")];
        assert!(clipboard_shortcut_status("option+e", &items, None).is_ok());
        // But a usable entry still does claim its chord.
        let items = vec![item("quick.open_finder", "option+e")];
        assert_eq!(
            clipboard_shortcut_status("option+e", &items, None),
            Err(ClipboardStatus::Conflict)
        );
    }

    #[test]
    fn duplicates_are_reported_once_and_only_among_usable_entries() {
        let items = vec![
            item("quick.open_finder", "option+e"),
            // Same trigger, but this entry is unusable on its own: it must not make the third
            // entry look like a duplicate.
            item("window.nope", "option+e"),
            item("quick.open_finder", "option+e"),
        ];
        let errors = validate_items(Some(&items));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].starts_with("key_bindings.items[1].action"));
        assert!(errors[1].starts_with("key_bindings.items[2].trigger"));
    }

    #[test]
    fn the_switcher_chord_is_reserved_with_extra_modifiers() {
        assert_eq!(conflicts("quick.open_finder", "cmd+tab").len(), 1);
        assert_eq!(conflicts("quick.open_finder", "cmd+shift+tab").len(), 1);
        assert!(conflicts("quick.open_finder", "option+tab").is_empty());
        assert_eq!(conflicts("quick.open_finder", "option+tab").len(), 0);
    }

    #[test]
    fn reserved_triggers_are_dropped_whatever_the_action_is() {
        // This pass is deliberately trigger-only: `merge_valid` has already dropped entries with an
        // unknown action, so an entry that is both unknown and reserved is never reported twice. A
        // caller that skipped that step still gets the entry removed rather than silently kept.
        let mut items = vec![item("window.nope", "cmd+tab")];
        let errors =
            drop_reserved_conflicts(&mut items, &Reserved::from_effective("command", "option+v"));
        assert_eq!(errors.len(), 1);
        assert!(items.is_empty());
    }

    #[test]
    fn the_clipboard_chord_is_reserved_exactly() {
        assert!(conflicts("quick.open_finder", "option+v").len() == 1);
        assert!(conflicts("quick.open_finder", "option+shift+v").is_empty());
        assert!(conflicts("quick.open_finder", "option+e").is_empty());
    }

    #[test]
    fn dropped_entries_leave_their_siblings_alone() {
        let mut items = vec![
            item("quick.open_finder", "cmd+tab"),
            item("quick.lock_screen", "option+l"),
        ];
        let errors =
            drop_reserved_conflicts(&mut items, &Reserved::from_effective("command", "option+v"));
        assert_eq!(errors.len(), 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].action, "quick.lock_screen");
    }

    #[test]
    fn clipboard_shortcut_status_reports_invalid_and_conflict() {
        let items = vec![item("quick.open_finder", "option+e")];
        assert_eq!(
            clipboard_shortcut_status("not-a-key", &items, Some(shortcut::FLAG_CMD)),
            Err(ClipboardStatus::Invalid)
        );
        // Collides with a binding.
        assert_eq!(
            clipboard_shortcut_status("option+e", &items, Some(shortcut::FLAG_CMD)),
            Err(ClipboardStatus::Conflict)
        );
        // Collides with the switcher.
        assert_eq!(
            clipboard_shortcut_status("cmd+tab", &items, Some(shortcut::FLAG_CMD)),
            Err(ClipboardStatus::Conflict)
        );
        assert!(clipboard_shortcut_status("option+v", &items, Some(shortcut::FLAG_CMD)).is_ok());
    }

    #[test]
    fn disabled_bindings_still_reserve_their_chord() {
        let items = vec![KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "option+e".into(),
            enabled: false,
        }];
        assert_eq!(
            clipboard_shortcut_status("option+e", &items, Some(shortcut::FLAG_CMD)),
            Err(ClipboardStatus::Conflict)
        );
    }
}
