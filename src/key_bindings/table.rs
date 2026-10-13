//! The immutable lookup table the binding event tap reads.
//!
//! Built once per config change and published as an `Arc`: the tap thread resolves a press with a
//! hash lookup and never touches the config lock, allocation, or the entries it dropped. Building
//! is the only place the master switch and the per-row switches are consulted, so the runtime never
//! has two sources of truth for "is this binding live".

use std::collections::HashMap;

use super::action::Action;
use super::trigger::{Trigger, MODIFIER_MASK};

/// One binding whose master switch and row switch are both on and whose action and trigger are
/// usable.
pub(crate) struct BindingTable {
    /// (keycode, modifier set) -> action. Later entries win, which cannot happen in practice:
    /// duplicate triggers are a validation error and the loser never reaches the table.
    chords: HashMap<(u16, u32), Action>,
    /// Double-tapped modifier flag -> action.
    gestures: HashMap<u32, Action>,
}

impl BindingTable {
    pub(crate) fn build(items: &[super::KeyBinding], enabled: bool) -> Self {
        let mut chords = HashMap::new();
        let mut gestures = HashMap::new();
        for item in items {
            if !enabled || !item.enabled {
                continue;
            }
            let Some(action) = Action::from_config_id(&item.action) else {
                continue;
            };
            let Ok(trigger) = Trigger::parse(&item.trigger) else {
                continue;
            };
            match trigger {
                // A bare key would swallow plain typing system-wide; validation refuses it and the
                // defensive check here keeps a hand-edited table from doing it anyway.
                Trigger::Chord(_) if trigger.is_bare_key() => continue,
                Trigger::Chord(chord) => {
                    chords.insert((chord.keycode, chord.flags & MODIFIER_MASK), action);
                }
                Trigger::DoubleModifier(flag) => {
                    gestures.insert(flag, action);
                }
            }
        }
        Self { chords, gestures }
    }

    /// Resolve a physical press. `flags` is the raw event flag set; unrelated bits (Caps Lock, Fn)
    /// are masked out, so a chord matches with or without them.
    pub(crate) fn lookup_chord(&self, keycode: u16, flags: u32) -> Option<Action> {
        self.chords.get(&(keycode, flags & MODIFIER_MASK)).copied()
    }

    /// Resolve a double-tap on one modifier (observed through `flagsChanged`).
    pub(crate) fn lookup_gesture(&self, modifier_flag: u32) -> Option<Action> {
        self.gestures.get(&modifier_flag).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.chords.len() + self.gestures.len()
    }

    /// How many gestures are live. `flagsChanged` handling runs on every modifier change, so it
    /// checks this once and returns before touching any state when there is nothing to match.
    pub(crate) fn gesture_count(&self) -> usize {
        self.gestures.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_bindings::KeyBinding;

    fn item(action: &str, trigger: &str, enabled: bool) -> KeyBinding {
        KeyBinding {
            action: action.to_string(),
            trigger: trigger.to_string(),
            enabled,
        }
    }

    #[test]
    fn only_enabled_entries_under_the_master_switch_are_live() {
        let items = vec![
            item("window.snap_left", "option+left", true),
            item("window.snap_right", "option+right", false),
            item("quick.open_finder", "option+e", true),
        ];
        let table = BindingTable::build(&items, true);
        let option = crate::mouse::shortcut::FLAG_ALT;
        assert_eq!(table.lookup_chord(0x7B, option), Some(Action::SnapLeft));
        assert_eq!(table.lookup_chord(0x7C, option), None, "row switch off");
        assert_eq!(table.lookup_chord(0x0E, option), Some(Action::OpenFinder));
        assert_eq!(table.len(), 2);

        // The master switch is the one gate over the whole list: it stops every group at once.
        let off = BindingTable::build(&items, false);
        assert_eq!(off.lookup_chord(0x7B, option), None, "master switch off");
        assert_eq!(off.lookup_chord(0x0E, option), None, "master switch off");
        assert_eq!(off.len(), 0);
    }

    #[test]
    fn modifier_matching_ignores_unrelated_flag_bits() {
        let items = vec![item("quick.open_finder", "option+e", true)];
        let table = BindingTable::build(&items, true);
        let option = crate::mouse::shortcut::FLAG_ALT;
        assert_eq!(table.lookup_chord(0x0E, option), Some(Action::OpenFinder));
        // Caps Lock (0x10000) rides along in real events.
        assert_eq!(
            table.lookup_chord(0x0E, option | 0x0001_0000),
            Some(Action::OpenFinder)
        );
        // A different chord with the same key: option+shift+e is not option+e.
        let shift = crate::mouse::shortcut::FLAG_SHIFT;
        assert_eq!(table.lookup_chord(0x0E, option | shift), None);
    }

    #[test]
    fn unusable_entries_never_reach_the_table() {
        let items = vec![
            item("window.nope", "option+e", true),
            item("quick.open_finder", "not-a-trigger", true),
            item("quick.open_settings", "e", true),
            item("quick.open_finder", "option+e", true),
        ];
        let table = BindingTable::build(&items, true);
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.lookup_chord(0x0E, crate::mouse::shortcut::FLAG_ALT),
            Some(Action::OpenFinder)
        );
    }

    #[test]
    fn gestures_are_resolved_by_modifier_flag() {
        let items = vec![item("quick.locate_pointer", "double-control", true)];
        let table = BindingTable::build(&items, true);
        let control = crate::mouse::shortcut::FLAG_CTRL;
        assert_eq!(table.lookup_gesture(control), Some(Action::LocatePointer));
        assert_eq!(table.lookup_gesture(crate::mouse::shortcut::FLAG_ALT), None);
        assert_eq!(
            table.lookup_chord(0x3B, control),
            None,
            "a gesture is not a chord"
        );
        assert_eq!(table.len(), 1);
    }
}
