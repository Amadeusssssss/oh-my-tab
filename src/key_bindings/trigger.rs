//! Key-binding triggers: a modifier chord ("option+e") or a double-tapped modifier
//! ("double-control").
//!
//! The chord grammar itself lives in `mouse::shortcut` (shared with the clipboard shortcut and the
//! mouse button mappings); this module adds the gesture form, the config round-trip, and the
//! rules that decide whether a trigger is usable as a global binding at all.

use crate::mouse::shortcut::{self, Shortcut};

/// The four modifier bits that participate in exact chord matching. A real event also carries
/// unrelated state (Caps Lock, Fn, numeric pad); only these four decide a match, exactly as
/// `mouse::shortcut::matches_shortcut` treats the clipboard binding.
pub(crate) const MODIFIER_MASK: u32 =
    shortcut::FLAG_CMD | shortcut::FLAG_ALT | shortcut::FLAG_CTRL | shortcut::FLAG_SHIFT;

/// A parsed binding trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// Main key plus an exact modifier set.
    Chord(Shortcut),
    /// Double-tap on one modifier; the payload is that modifier's flag bit. Only ever observed
    /// through `flagsChanged`, and never swallowed.
    DoubleModifier(u32),
}

/// Gesture spellings accepted in the config, with the modifier flag they mean.
///
/// Shift is deliberately absent: two consecutive Shift presses are ordinary typing (capitalizing
/// two words), so a double-Shift gesture would fire while the user writes text. Shift chords keep
/// working; only the gesture form is refused.
const DOUBLE_MODIFIERS: [(&str, u32); 3] = [
    ("double-command", shortcut::FLAG_CMD),
    ("double-option", shortcut::FLAG_ALT),
    ("double-control", shortcut::FLAG_CTRL),
];

impl Trigger {
    /// Parse a config value: a gesture spelling, or a `mouse::shortcut` chord description.
    /// Err carries an English reason for validation to wrap in an i18n message.
    pub(crate) fn parse(desc: &str) -> Result<Self, String> {
        let trimmed = desc.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some((_, flag)) = DOUBLE_MODIFIERS.iter().find(|(name, _)| *name == lower) {
            return Ok(Self::DoubleModifier(*flag));
        }
        if lower.starts_with("double-") {
            return Err(format!("unknown gesture: {trimmed}"));
        }
        shortcut::parse_shortcut(trimmed).map(Self::Chord)
    }

    /// Serialize back to config form (the inverse of `parse` for every trigger this app writes).
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Chord(chord) => shortcut::describe_shortcut(chord.keycode, chord.flags),
            Self::DoubleModifier(flag) => DOUBLE_MODIFIERS
                .iter()
                .find(|(_, f)| f == flag)
                .map(|(name, _)| (*name).to_string())
                .unwrap_or_default(),
        }
    }

    /// The keycap-style label shown in the settings list ("⌥E", "⌃⌃"). Language-neutral: the
    /// gesture form reuses the modifier symbols the chord display already uses.
    pub(crate) fn keycap(&self) -> String {
        match self {
            Self::Chord(_) => shortcut::display_shortcut(&self.describe()),
            Self::DoubleModifier(flag) => {
                let symbol = shortcut::modifier_display(*flag);
                format!("{symbol}{symbol}")
            }
        }
    }

    /// Whether this trigger is a chord for the engine's (keycode, modifier set) lookup.
    pub(crate) fn chord(&self) -> Option<Shortcut> {
        match self {
            Self::Chord(chord) => Some(*chord),
            Self::DoubleModifier(_) => None,
        }
    }

    /// A chord whose only modifier is Shift, or none at all. Global bindings are swallowed
    /// system-wide, so such a chord would take plain typing, capital letters and Tab navigation
    /// away from every app; validation refuses it. One of Command/Option/Control is required.
    pub(crate) fn is_bare_key(&self) -> bool {
        match self {
            Self::Chord(chord) => {
                chord.flags & (shortcut::FLAG_CMD | shortcut::FLAG_ALT | shortcut::FLAG_CTRL) == 0
            }
            Self::DoubleModifier(_) => false,
        }
    }
}

/// Modifier flag for a config modifier name (`keyboard.modifier` values, and the gesture names).
pub(crate) fn modifier_flag(name: &str) -> Option<u32> {
    match name.trim().to_ascii_lowercase().as_str() {
        "command" | "cmd" => Some(shortcut::FLAG_CMD),
        "option" | "alt" => Some(shortcut::FLAG_ALT),
        "control" | "ctrl" => Some(shortcut::FLAG_CTRL),
        "shift" => Some(shortcut::FLAG_SHIFT),
        _ => None,
    }
}

/// The Tab keycode, taken from the chord grammar so the switcher reservation cannot drift from it.
pub(crate) fn tab_keycode() -> u16 {
    shortcut::parse_shortcut("tab")
        .expect("the chord grammar must know Tab")
        .keycode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_trigger_forms() {
        let chord = Trigger::parse("option+e").unwrap();
        assert_eq!(chord.chord().map(|c| c.keycode), Some(0x0E));
        assert!(chord.chord().unwrap().flags & shortcut::FLAG_ALT != 0);
        assert_eq!(
            Trigger::parse("double-control").unwrap(),
            Trigger::DoubleModifier(shortcut::FLAG_CTRL)
        );
        assert_eq!(
            Trigger::parse(" Option + E ").unwrap(),
            Trigger::parse("option+e").unwrap()
        );
    }

    #[test]
    fn rejects_unknown_or_bare_values() {
        assert!(Trigger::parse("").is_err());
        assert!(Trigger::parse("option+notakey").is_err());
        assert!(Trigger::parse("double-shift").is_err());
        assert!(Trigger::parse("double-").is_err());
    }

    #[test]
    fn describe_roundtrips_through_parse() {
        for desc in [
            "option+e",
            "option+shift+up",
            "cmd+ctrl+1",
            "double-control",
        ] {
            let trigger = Trigger::parse(desc).unwrap();
            let rewritten = trigger.describe();
            assert_eq!(Trigger::parse(&rewritten).unwrap(), trigger, "{desc}");
        }
    }

    #[test]
    fn keycap_is_symbolic_for_both_forms() {
        assert_eq!(Trigger::parse("option+e").unwrap().keycap(), "⌥E");
        // Modifier order is the shared chord display's (⌘⇧⌥⌃), so a binding's keycap reads like
        // the clipboard row and the mouse mapping rows.
        assert_eq!(Trigger::parse("option+shift+up").unwrap().keycap(), "⇧⌥↑");
        assert_eq!(Trigger::parse("double-control").unwrap().keycap(), "⌃⌃");
    }

    #[test]
    fn bare_keys_and_the_switcher_are_identified() {
        assert!(Trigger::parse("e").unwrap().is_bare_key());
        assert!(Trigger::parse("f5").unwrap().is_bare_key());
        assert!(Trigger::parse("shift+e").unwrap().is_bare_key());
        assert!(!Trigger::parse("option+e").unwrap().is_bare_key());
        assert!(!Trigger::parse("cmd+tab").unwrap().is_bare_key());
        assert_eq!(tab_keycode(), 0x30);
    }
}
