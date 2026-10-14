//! Settings import / export: one portable TOML file for the whole configuration.
//!
//! The export is the live config written as ordinary TOML with a small comment header. The file
//! body is therefore a valid config.toml, and the header is transparent to any TOML parser.
//!
//! Two rules carry the cross-version behaviour, and both come from reusing the app's own loader
//! rather than deserializing the export directly:
//!
//! * Newer file, older app. A field the running build does not know is left alone by the type
//!   sanitizer and ignored by Serde; a field it knows but rejects falls back on its own. So a file
//!   from a later version still imports, minus what this build cannot honour.
//! * Older file, newer app. Missing fields take their defaults and the loader's own migration
//!   hooks run, including the legacy [quick_actions] / [window_control] sections that
//!   seed_key_bindings turns back into a binding list. An old backup does not lose its bindings.
//!
//! The version in the header therefore guards the *file protocol* only: it rises when the meaning
//! of the file itself changes, not when the app gains settings. The setting-level compatibility
//! policy is the loader's, and it is deliberately allowed to drop settings that this build cannot
//! represent.
//!
//! Re-exporting a file that carried unknown fields does not preserve them: the live Config has no
//! place to keep them. The round trip is lossless for what this build understands, not for the
//! bytes.

use crate::config::Config;

/// Header line that identifies an export, so a file that merely happens to be valid TOML (and
/// would otherwise import as "everything at its default") is refused instead of silently wiping
/// the user's settings.
pub(super) const EXPORT_MARKER: &str = "# oh-my-tab settings export";

/// The file protocol version this build writes and is willing to read. Raise it only for a change
/// that makes older readers misread the file; ordinary setting changes keep it at 1.
pub(super) const EXPORT_FORMAT: u32 = 1;

/// Header key holding EXPORT_FORMAT.
const FORMAT_KEY: &str = "# format = ";

/// Why an export file was refused. The message shown to the user comes from the i18n keys named
/// here, never from the raw parse error: those carry internal field paths and internal enum values
/// that must not reach the UI.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ImportError {
    /// No marker line: not one of our exports.
    NotAnExport,
    /// The header states a protocol version this build cannot read.
    UnsupportedFormat(u32),
    /// TOML syntax error, or a document that parses but carries no configuration at all.
    Unreadable,
}

/// Everything an import produces, kept separate from the side effects so the whole decision is
/// unit-testable.
#[derive(Debug, PartialEq)]
pub(super) struct ImportedConfig {
    /// The configuration to install (already adjusted for the preserved fields).
    pub config: Config,
    /// Whether the loader had to repair or fall back on anything. Reported to the user as a partial
    /// success, because the loader's own semantics is "keep every valid field".
    pub repaired: bool,
}

/// Render the export file for a config.
///
/// The app version is recorded for diagnosis only; it is never used to gate an import. The body is
/// toml::to_string_pretty of the live config, so the same bytes can be dropped in as config.toml.
pub(super) fn export_toml(config: &Config, app_version: &str, timestamp: &str) -> String {
    let body = toml::to_string_pretty(config).unwrap_or_default();
    format!(
        "{EXPORT_MARKER}\n# format = {EXPORT_FORMAT}\n# app_version = {app_version}\n# exported_at = {timestamp}\n\n{body}"
    )
}

/// What the file's leading comment block declares.
struct Header {
    /// The protocol version the header states.
    format: u32,
}

/// Parse the leading comment block.
///
/// Everything here reads only the comments before the first non-comment line. A marker or a
/// `format` key further down belongs to the configuration body: a comment inside a multi-line
/// string, or a stray table name, must not be able to identify the file or choose the reader's
/// behaviour.
///
/// A missing, duplicated, non-numeric or non-positive declaration is refused rather than
/// defaulted: guessing which protocol a damaged header meant is exactly what the version exists to
/// prevent, and an invalid line followed by a valid one must not launder the invalid one.
fn parse_header(content: &str) -> Result<Header, ImportError> {
    let mut marker = false;
    let mut format: Option<u32> = None;
    for line in content.lines() {
        let line = line.trim_end();
        if !line.starts_with('#') {
            // The header ends at the first non-comment line; the body is not consulted.
            break;
        }
        if line == EXPORT_MARKER {
            marker = true;
            continue;
        }
        let Some(rest) = line.strip_prefix(FORMAT_KEY) else {
            continue;
        };
        let version = rest
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|value| *value >= 1)
            .ok_or(ImportError::Unreadable)?;
        // A second declaration is ambiguous: refuse rather than let file order decide.
        if format.replace(version).is_some() {
            return Err(ImportError::Unreadable);
        }
    }
    if !marker {
        return Err(ImportError::NotAnExport);
    }
    Ok(Header {
        format: format.ok_or(ImportError::Unreadable)?,
    })
}

/// Whether the parsed document carries at least one real configuration section.
///
/// Judged from the parsed structure, never from text: `[colors_backup]` is a top-level table but
/// not a section this app has, and a `[appearance]` written inside a multi-line string is not a
/// section at all. Either would otherwise be accepted and then ignored by the loader, silently
/// resetting every setting.
///
/// The retired `quick_actions` and `window_control` tables count: an export written before they
/// were merged into the binding list carries only those two, and it must not be refused as an
/// empty file just because this build no longer writes them.
fn body_has_known_section(document: &toml::Value) -> bool {
    const KNOWN: [&str; 16] = [
        "appearance",
        "layout",
        "colors",
        "fonts",
        "keyboard",
        "i18n",
        "windows",
        "logging",
        "startup",
        "updates",
        "clipboard",
        "mouse",
        "key_bindings",
        "keystroke_display",
        "quick_actions",
        "window_control",
    ];
    document.as_table().is_some_and(|root| {
        root.iter()
            .any(|(key, value)| value.is_table() && KNOWN.contains(&key.as_str()))
    })
}

/// Parse an export file into the configuration it carries, before the preserved fields are
/// restored. Pure: no file access and no global state, so every acceptance rule is testable.
pub(super) fn parse_export(content: &str) -> Result<ImportedConfig, ImportError> {
    // The header first: it decides whether this is our file at all, and which protocol it speaks.
    let header = parse_header(content)?;
    if header.format != EXPORT_FORMAT {
        return Err(ImportError::UnsupportedFormat(header.format));
    }
    // The body is inspected as parsed TOML, before the loader runs: a document with no section this
    // app knows would import as "everything at its default", which is indistinguishable from a
    // deliberate wipe.
    let document = content
        .parse::<toml::Value>()
        .map_err(|_| ImportError::Unreadable)?;
    if !body_has_known_section(&document) {
        return Err(ImportError::Unreadable);
    }
    // The app's own loader, not Serde directly: field-by-field fallback, the migration hooks and
    // the legacy-section seeding are the compatibility this feature promises.
    let (config, errors, _needs_persist) =
        crate::config::parse_config_content(content).map_err(|_| ImportError::Unreadable)?;
    Ok(ImportedConfig {
        config,
        repaired: !errors.is_empty(),
    })
}

/// Restore the fields that describe this machine over a freshly imported configuration.
///
/// These are not portable settings, and one of them is destructive to carry: importing
/// clipboard.enabled = false over a machine that has it on runs the feature's own disable path,
/// which deletes the stored history. The rest are either machine-level (a login item, an absolute
/// log path, a panel position tied to this display arrangement) or personalization of the local
/// clipboard store.
pub(super) fn restore_local_fields(imported: &mut Config, local: &Config) {
    imported.startup.launch_at_login = local.startup.launch_at_login;
    imported.logging.file_path = local.logging.file_path.clone();
    imported.clipboard.enabled = local.clipboard.enabled;
    imported.keystroke_display.position = local.keystroke_display.position;
    // A tighter cap or TTL silently truncates or expires records that are already on disk, and
    // clear_on_quit deletes them at exit. Those are storage decisions about the local history, not
    // layout preferences, so they stay with the machine that owns the records.
    imported.clipboard.max_entries = local.clipboard.max_entries;
    imported.clipboard.auto_expire_days = local.clipboard.auto_expire_days;
    imported.clipboard.clear_on_quit = local.clipboard.clear_on_quit;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export_of(config: &Config) -> String {
        export_toml(config, "0.2.5", "2026-02-14T10:30:00+08:00")
    }

    #[test]
    fn an_export_round_trips_every_portable_field() {
        let mut original = Config::default();
        original.appearance.theme = "light".into();
        original.appearance.corner_radius = 16.0;
        original.layout.thumbnails_enabled = true;
        original.windows.show_other_desktops = true;
        original.i18n.locale = "zh-Hans".into();
        original.logging.level = "debug".into();
        original.clipboard.show_source_app = false;
        let local = Config::default();

        let parsed =
            parse_export(&export_of(&original)).expect("a self-written export must import");
        let mut imported = parsed.config;
        assert!(
            !parsed.repaired,
            "a clean export must not be reported as repaired"
        );
        restore_local_fields(&mut imported, &local);

        // The preserved fields are deliberately local, so compare the portable part only.
        let mut expected = original;
        restore_local_fields(&mut expected, &local);
        assert_eq!(imported, expected);
    }

    #[test]
    fn the_file_body_is_a_valid_config_document() {
        // The header must be transparent to the loader: the same bytes are a config.toml a user
        // could drop into ~/.config/oh-my-tab/.
        let mut config = Config::default();
        config.appearance.theme = "dark".into();
        config.fonts.title_size = 15.0;
        let parsed = parse_export(&export_of(&config)).expect("import");
        assert_eq!(parsed.config.appearance.theme, "dark");
        assert_eq!(parsed.config.fonts.title_size, 15.0);
    }

    #[test]
    fn a_file_without_the_marker_is_refused() {
        // Valid TOML that is not ours would import as an all-defaults config, i.e. wipe settings.
        assert_eq!(
            parse_export("foo = 1\n[appearance]\ntheme = \"dark\"\n"),
            Err(ImportError::NotAnExport)
        );
        assert_eq!(parse_export(""), Err(ImportError::NotAnExport));
    }

    #[test]
    fn a_newer_file_protocol_is_refused() {
        let content = format!("{EXPORT_MARKER}\n# format = 2\n\n[appearance]\ntheme = \"dark\"\n");
        assert_eq!(
            parse_export(&content),
            Err(ImportError::UnsupportedFormat(2))
        );
    }

    #[test]
    fn a_marker_without_a_usable_format_is_refused() {
        // Missing, zero, non-numeric and duplicate declarations are all unreadable: defaulting them
        // to a version would let a damaged header choose the reader's behaviour.
        let header_zero = format!("{EXPORT_MARKER}\n# format = 0");
        let header_negative = format!("{EXPORT_MARKER}\n# format = -1");
        let header_word = format!("{EXPORT_MARKER}\n# format = one");
        let header_duplicate = format!("{EXPORT_MARKER}\n# format = 1\n# format = 1");
        for header in [
            EXPORT_MARKER.to_string(),
            header_zero,
            header_negative,
            header_word,
            header_duplicate,
        ] {
            let content = format!("{header}\n\n[appearance]\ntheme = \"dark\"\n");
            assert_eq!(
                parse_export(&content),
                Err(ImportError::Unreadable),
                "header {header:?} must be refused"
            );
        }
    }

    #[test]
    fn a_header_with_no_recognizable_body_is_refused() {
        // An all-defaults export is legitimate and must keep working...
        assert!(parse_export(&export_of(&Config::default())).is_ok());
        // ...but a header plus an empty or unrelated body is not a configuration.
        for body in ["", "\n", "foo = 1\n", "[colors_backup]\nvalue = 1\n"] {
            let content = format!("{EXPORT_MARKER}\n# format = 1\n\n{body}");
            assert_eq!(
                parse_export(&content),
                Err(ImportError::Unreadable),
                "body {body:?} must be refused"
            );
        }
    }

    #[test]
    fn the_body_cannot_choose_the_file_protocol() {
        // A format key inside the body is an ordinary (unknown) key. Reading it as a version would
        // let a hand-edited config pick a protocol it does not speak.
        let content = format!(
            "{EXPORT_MARKER}\n# format = 1\n\nformat = 9\n[appearance]\ntheme = \"dark\"\n"
        );
        let parsed = parse_export(&content).expect("the header version is the one that counts");
        assert_eq!(parsed.config.appearance.theme, "dark");
    }

    #[test]
    fn malformed_toml_is_unreadable_rather_than_a_silent_default() {
        let content = format!("{EXPORT_MARKER}\n# format = 1\n\n[appearance\ntheme = \n");
        assert_eq!(parse_export(&content), Err(ImportError::Unreadable));
    }

    #[test]
    fn an_unknown_field_is_ignored_and_an_invalid_one_is_repaired() {
        // Newer-app -> older-app: an unknown key and an unknown value must not sink the import.
        let content = format!(
            "{EXPORT_MARKER}\n# format = 1\n\n[appearance]\ntheme = \"dark\"\npanel_material = \"from-the-future\"\nfuture_setting = 7\n"
        );
        let parsed = parse_export(&content).expect("a newer file still imports");
        assert!(
            parsed.repaired,
            "the rejected value must be reported as a repair"
        );
        assert_eq!(
            parsed.config.appearance.theme, "dark",
            "valid siblings survive"
        );
        assert_eq!(
            parsed.config.appearance.panel_material,
            Config::default().appearance.panel_material,
            "only the rejected field falls back"
        );
    }

    #[test]
    fn a_pre_release_export_still_seeds_its_bindings() {
        // Older-app -> newer-app: v0.2.5 wrote neither [key_bindings] nor the newer sections, only
        // the two legacy group tables. The loader's seeding must turn them into a binding list, or
        // an old backup would import with every custom shortcut silently gone.
        let content = format!(
            "{EXPORT_MARKER}\n# format = 1\n\n[window_control]\nenabled = true\nup = false\n[quick_actions]\nenabled = true\nopen_finder = true\n"
        );
        let parsed = parse_export(&content).expect("a legacy export must import");
        let items = parsed
            .config
            .key_bindings
            .items
            .as_ref()
            .expect("the legacy switches must seed a binding list");
        assert!(!items.is_empty(), "seeded bindings must not be empty");
        // Each legacy direction becomes its own row, and the row's own switch is preserved: the
        // file turned `window_control.up` off, so snap_up must come back disabled rather than
        // silently re-armed by the master switch.
        let by_action = |action: &str| {
            items
                .iter()
                .find(|entry| entry.action == action)
                .unwrap_or_else(|| panic!("{action} missing from seeded list: {items:?}"))
        };
        assert!(
            !by_action("window.snap_up").enabled,
            "a legacy row the file disabled must stay disabled"
        );
        assert!(
            by_action("window.snap_down").enabled,
            "a legacy row the file enabled must stay enabled"
        );
        assert!(
            by_action("quick.open_finder").enabled,
            "the quick-actions table must seed too"
        );
    }

    #[test]
    fn the_local_fields_survive_an_import() {
        let mut local = Config::default();
        local.startup.launch_at_login = true;
        local.logging.file_path = "/tmp/local-only.log".into();
        local.clipboard.enabled = true;
        local.clipboard.max_entries = 90;
        local.clipboard.auto_expire_days = 7;
        local.clipboard.clear_on_quit = false;
        local.keystroke_display.position =
            Some(crate::config::KeystrokeDisplayPosition { x: 12.0, y: 34.0 });

        // An export from another machine that disagrees about every one of them.
        let mut other = Config::default();
        other.startup.launch_at_login = false;
        other.logging.file_path = "/tmp/other.log".into();
        other.clipboard.enabled = false;
        other.clipboard.max_entries = 5;
        other.clipboard.auto_expire_days = 0;
        other.clipboard.clear_on_quit = true;
        other.keystroke_display.position = None;
        other.appearance.theme = "light".into();

        let mut imported = parse_export(&export_of(&other)).expect("import").config;
        restore_local_fields(&mut imported, &local);

        assert!(
            imported.startup.launch_at_login,
            "the login item is the local machine's"
        );
        assert_eq!(imported.logging.file_path, "/tmp/local-only.log");
        assert!(imported.clipboard.enabled);
        assert_eq!(imported.clipboard.max_entries, 90);
        assert_eq!(imported.clipboard.auto_expire_days, 7);
        assert!(!imported.clipboard.clear_on_quit);
        assert!(imported.keystroke_display.position.is_some());
        // A genuinely portable setting still follows the file.
        assert_eq!(imported.appearance.theme, "light");
    }

    #[test]
    fn the_written_header_is_the_one_the_reader_accepts() {
        // Guards the writer and the parser against drifting apart: a build that writes a header it
        // refuses to read would ship an export feature that only works until relaunch.
        let content = export_of(&Config::default());
        assert_eq!(
            parse_header(&content).ok().map(|h| h.format),
            Some(EXPORT_FORMAT)
        );
        assert!(parse_export(&content).is_ok());
    }

    #[test]
    fn an_invalid_declaration_is_not_laundered_by_a_valid_one() {
        // A damaged line ahead of a good one must not be skipped: the reader would otherwise
        // believe the file declared a protocol it only mentioned after something unreadable.
        for bad in ["broken", "0", "-1", "1.0"] {
            let content = format!(
                "{EXPORT_MARKER}\n# format = {bad}\n# format = 1\n\n[appearance]\ntheme = \"dark\"\n",
            );
            assert_eq!(
                parse_export(&content),
                Err(ImportError::Unreadable),
                "`# format = {bad}` ahead of a valid line must be refused",
            );
        }
    }

    #[test]
    fn the_marker_must_be_in_the_header() {
        // A marker inside the body (a comment after the first table, or inside a multi-line string)
        // must not identify the file: the identity gate would otherwise be satisfied by content the
        // loader never reads as a header.
        let bodies = [
            "[appearance]\ntheme = \"dark\"\n# oh-my-tab settings export\n",
            "note = \"\n# oh-my-tab settings export\n\"\n[appearance]\ntheme = \"dark\"\n",
        ];
        for body in bodies {
            let content = format!("# format = 1\n\n{body}");
            assert_eq!(
                parse_export(&content),
                Err(ImportError::NotAnExport),
                "a marker outside the header must not count",
            );
        }
    }

    #[test]
    fn a_lookalike_table_is_not_a_known_section() {
        // Text-prefix matching accepted a table that merely starts like a section name, which the
        // loader then ignored: the document imported as all-defaults and silently reset every
        // setting. Identity is decided from the parsed structure, so that is refused.
        let bodies = [
            "[colors_backup]\nvalue = 1\n",
            "[appearance_extra]\nvalue = 1\n",
        ];
        for body in bodies {
            let content = format!("{EXPORT_MARKER}\n# format = 1\n\n{body}");
            assert_eq!(
                parse_export(&content),
                Err(ImportError::Unreadable),
                "body {body:?} has no real section and must be refused",
            );
        }
    }

    #[test]
    fn the_header_survives_crlf_and_extra_blank_lines() {
        let content = format!(
            "{EXPORT_MARKER}\r\n# format = 1\r\n\r\n\r\n[appearance]\r\ntheme = \"dark\"\r\n"
        );
        let parsed = parse_export(&content).expect("a Windows-edited export must import");
        assert_eq!(parsed.config.appearance.theme, "dark");
    }
}
