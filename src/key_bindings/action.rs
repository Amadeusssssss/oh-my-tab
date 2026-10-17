//! The action registry: every behaviour a global binding can run.
//!
//! Each action carries a stable config id (what `[[key_bindings.items]].action` holds), the group
//! whose switch gates it, the i18n key of its display name, and — for the few that care — whether
//! it acts on the frontmost window. Adding an action means adding a variant, an `ALL` entry, a
//! `config_id`, a `label_key` and an `execute` arm; nothing else in the codebase enumerates
//! actions.

use crate::window_management::Direction;

/// Which family an action belongs to. This is a classification, not a gate: one master switch owns
/// the whole list now (`KeyBindingsSection::enabled`), and the group only decides where an action
/// sits in the picker and whether it acts on the frontmost window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    WindowControl,
    QuickActions,
}

/// Every bindable action.
///
/// The numeric values cross the thread bridge (`GlobalEvent::BindingAction`) as `u16`, so the
/// order is **append-only**: never reorder, never reuse a value, always add new variants at the
/// end. `indices_are_frozen` pins the current numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    // ---- Window control (group switch: window control) ----
    /// Arrow-style snapping: the direction is interpreted against the window's inferred state, so
    /// repeated presses cycle (normal -> half -> quarter -> minimized) instead of forcing one rect.
    SnapUp = 0,
    SnapDown = 1,
    SnapLeft = 2,
    SnapRight = 3,
    /// Move the window to the neighbouring display in that direction.
    DisplayUp = 4,
    DisplayDown = 5,
    DisplayLeft = 6,
    DisplayRight = 7,
    /// Direct, idempotent targets (no cycling): what a user means by "maximize this" rather than
    /// "keep pressing up".
    Maximize = 8,
    Minimize = 9,
    Center = 10,
    HalfTop = 11,
    HalfBottom = 12,
    HalfLeft = 13,
    HalfRight = 14,
    QuarterTopLeft = 15,
    QuarterTopRight = 16,
    QuarterBottomLeft = 17,
    QuarterBottomRight = 18,

    // ---- Quick actions (group switch: quick actions) ----
    OpenSettings = 19,
    OpenFinder = 20,
    ShowDesktop = 21,
    LockScreen = 22,
    LocatePointer = 23,
    /// Dock notifications, the same four the mouse button mappings can fire.
    MissionControl = 24,
    AppExpose = 25,
    Launchpad = 26,
    /// Activate the most recently observed still-running regular application.
    PreviousApp = 27,
    /// Feature switches; each goes through the full config-apply path so the runtime, the open
    /// settings window and the menu all follow.
    ToggleMouse = 28,
    ToggleKeystrokeDisplay = 29,
    ToggleClipboardHistory = 30,
    OpenClipboardPanel = 31,
    /// Put the frontmost window back to the frame it had before its last arrange (the app-wide
    /// counterpart of the switcher's per-window restore). Re-enters native full screen when that is
    /// what the window was in.
    RestorePrevious = 32,
    /// Hide or unhide every regular application. These act on applications, not the frontmost
    /// window, so they belong to QuickActions and are not window-scoped.
    HideAllWindows = 33,
    ShowAllWindows = 34,
    /// Open a new Finder window at a small fixed size instead of the maximized one
    /// (`OpenFinder`); the two are separate bindings.
    OpenFinderSmall = 35,
}

/// Every action, in the frozen index order. The settings picker walks this list.
pub(crate) const ALL: [Action; 36] = [
    Action::SnapUp,
    Action::SnapDown,
    Action::SnapLeft,
    Action::SnapRight,
    Action::DisplayUp,
    Action::DisplayDown,
    Action::DisplayLeft,
    Action::DisplayRight,
    Action::Maximize,
    Action::Minimize,
    Action::Center,
    Action::HalfTop,
    Action::HalfBottom,
    Action::HalfLeft,
    Action::HalfRight,
    Action::QuarterTopLeft,
    Action::QuarterTopRight,
    Action::QuarterBottomLeft,
    Action::QuarterBottomRight,
    Action::OpenSettings,
    Action::OpenFinder,
    Action::ShowDesktop,
    Action::LockScreen,
    Action::LocatePointer,
    Action::MissionControl,
    Action::AppExpose,
    Action::Launchpad,
    Action::PreviousApp,
    Action::ToggleMouse,
    Action::ToggleKeystrokeDisplay,
    Action::ToggleClipboardHistory,
    Action::OpenClipboardPanel,
    Action::RestorePrevious,
    Action::HideAllWindows,
    Action::ShowAllWindows,
    Action::OpenFinderSmall,
];

impl Action {
    /// The value carried across the thread bridge (`GlobalEvent::BindingAction`).
    pub(crate) fn index(self) -> u16 {
        self as u16
    }

    /// Rebuild an action from the bridge value; unknown values are dropped, exactly as
    /// `QuickAction::from_isize` drops unknown quick-action ids.
    pub(crate) fn from_index(value: u16) -> Option<Self> {
        ALL.get(value as usize).copied()
    }

    /// The stable config id. Never derived from the display name and never renumbered.
    pub(crate) fn config_id(self) -> &'static str {
        match self {
            Self::SnapUp => "window.snap_up",
            Self::SnapDown => "window.snap_down",
            Self::SnapLeft => "window.snap_left",
            Self::SnapRight => "window.snap_right",
            Self::DisplayUp => "window.display_up",
            Self::DisplayDown => "window.display_down",
            Self::DisplayLeft => "window.display_left",
            Self::DisplayRight => "window.display_right",
            Self::Maximize => "window.maximize",
            Self::Minimize => "window.minimize",
            Self::Center => "window.center",
            Self::HalfTop => "window.half_top",
            Self::HalfBottom => "window.half_bottom",
            Self::HalfLeft => "window.half_left",
            Self::HalfRight => "window.half_right",
            Self::QuarterTopLeft => "window.quarter_top_left",
            Self::QuarterTopRight => "window.quarter_top_right",
            Self::QuarterBottomLeft => "window.quarter_bottom_left",
            Self::QuarterBottomRight => "window.quarter_bottom_right",
            Self::OpenSettings => "quick.open_settings",
            Self::OpenFinder => "quick.open_finder",
            Self::ShowDesktop => "quick.show_desktop",
            Self::LockScreen => "quick.lock_screen",
            Self::LocatePointer => "quick.locate_pointer",
            Self::MissionControl => "system.mission_control",
            Self::AppExpose => "system.app_expose",
            Self::Launchpad => "system.launchpad",
            Self::PreviousApp => "app.previous",
            Self::ToggleMouse => "feature.toggle_mouse",
            Self::ToggleKeystrokeDisplay => "feature.toggle_keystroke_display",
            Self::ToggleClipboardHistory => "feature.toggle_clipboard_history",
            Self::OpenClipboardPanel => "feature.open_clipboard_panel",
            Self::RestorePrevious => "window.restore_previous",
            Self::HideAllWindows => "quick.hide_all_windows",
            Self::ShowAllWindows => "quick.show_all_windows",
            Self::OpenFinderSmall => "quick.open_finder_small",
        }
    }

    pub(crate) fn from_config_id(id: &str) -> Option<Self> {
        ALL.iter().copied().find(|action| action.config_id() == id)
    }

    pub(crate) fn group(self) -> Group {
        match self {
            Self::SnapUp
            | Self::SnapDown
            | Self::SnapLeft
            | Self::SnapRight
            | Self::DisplayUp
            | Self::DisplayDown
            | Self::DisplayLeft
            | Self::DisplayRight
            | Self::Maximize
            | Self::Minimize
            | Self::Center
            | Self::HalfTop
            | Self::HalfBottom
            | Self::HalfLeft
            | Self::HalfRight
            | Self::QuarterTopLeft
            | Self::QuarterTopRight
            | Self::QuarterBottomLeft
            | Self::QuarterBottomRight
            | Self::RestorePrevious => Group::WindowControl,
            _ => Group::QuickActions,
        }
    }

    /// i18n key of the display name. The four system actions reuse the mouse mapping panel's keys
    /// so one product term has one string.
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::SnapUp => "settings.binding_action_snap_up",
            Self::SnapDown => "settings.binding_action_snap_down",
            Self::SnapLeft => "settings.binding_action_snap_left",
            Self::SnapRight => "settings.binding_action_snap_right",
            Self::DisplayUp => "settings.binding_action_display_up",
            Self::DisplayDown => "settings.binding_action_display_down",
            Self::DisplayLeft => "settings.binding_action_display_left",
            Self::DisplayRight => "settings.binding_action_display_right",
            Self::Maximize => "settings.binding_action_maximize",
            Self::Minimize => "settings.binding_action_minimize",
            Self::Center => "settings.binding_action_center",
            Self::HalfTop => "settings.binding_action_half_top",
            Self::HalfBottom => "settings.binding_action_half_bottom",
            Self::HalfLeft => "settings.binding_action_half_left",
            Self::HalfRight => "settings.binding_action_half_right",
            Self::QuarterTopLeft => "settings.binding_action_quarter_top_left",
            Self::QuarterTopRight => "settings.binding_action_quarter_top_right",
            Self::QuarterBottomLeft => "settings.binding_action_quarter_bottom_left",
            Self::QuarterBottomRight => "settings.binding_action_quarter_bottom_right",
            Self::OpenSettings => "settings.binding_action_open_settings",
            Self::OpenFinder => "settings.binding_action_open_finder",
            Self::ShowDesktop => "settings.mapping_action_showdesktop",
            Self::LockScreen => "settings.binding_action_lock_screen",
            Self::LocatePointer => "settings.binding_action_locate_pointer",
            Self::MissionControl => "settings.mapping_action_missioncontrol",
            Self::AppExpose => "settings.mapping_action_appexpose",
            Self::Launchpad => "settings.mapping_action_launchpad",
            Self::PreviousApp => "settings.binding_action_previous_app",
            Self::ToggleMouse => "settings.binding_action_toggle_mouse",
            Self::ToggleKeystrokeDisplay => "settings.binding_action_toggle_keystroke_display",
            Self::ToggleClipboardHistory => "settings.binding_action_toggle_clipboard_history",
            Self::OpenClipboardPanel => "settings.binding_action_open_clipboard_panel",
            Self::RestorePrevious => "settings.binding_action_restore_previous",
            Self::HideAllWindows => "settings.binding_action_hide_all_windows",
            Self::ShowAllWindows => "settings.binding_action_show_all_windows",
            Self::OpenFinderSmall => "settings.binding_action_open_finder_small",
        }
    }

    /// Whether the action operates on the frontmost window. Those actions must not act on this
    /// app's own windows (see the eligibility rule in the engine's execution path).
    pub(crate) fn is_window_scoped(self) -> bool {
        self.group() == Group::WindowControl
    }

    /// The arrow-style direction of a cycling snap action, if it has one.
    pub(crate) fn snap_direction(self) -> Option<Direction> {
        match self {
            Self::SnapUp => Some(Direction::Up),
            Self::SnapDown => Some(Direction::Down),
            Self::SnapLeft => Some(Direction::Left),
            Self::SnapRight => Some(Direction::Right),
            _ => None,
        }
    }

    /// The display-move direction, if this action moves the window across displays.
    pub(crate) fn display_direction(self) -> Option<Direction> {
        match self {
            Self::DisplayUp => Some(Direction::Up),
            Self::DisplayDown => Some(Direction::Down),
            Self::DisplayLeft => Some(Direction::Left),
            Self::DisplayRight => Some(Direction::Right),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge payload is the registry index: a reorder or a removal silently re-points every
    /// binding, so the numbering is pinned here and new actions must go at the end.
    #[test]
    fn indices_are_frozen() {
        assert_eq!(Action::SnapUp.index(), 0);
        assert_eq!(Action::SnapRight.index(), 3);
        assert_eq!(Action::Maximize.index(), 8);
        assert_eq!(Action::OpenSettings.index(), 19);
        assert_eq!(Action::LocatePointer.index(), 23);
        assert_eq!(Action::OpenClipboardPanel.index(), 31);
        // Appended after OpenClipboardPanel: the earlier values must not move.
        assert_eq!(Action::RestorePrevious.index(), 32);
        assert_eq!(Action::HideAllWindows.index(), 33);
        assert_eq!(Action::ShowAllWindows.index(), 34);
        assert_eq!(Action::OpenFinderSmall.index(), 35);
        assert_eq!(ALL.len(), 36);
        for (index, action) in ALL.iter().enumerate() {
            assert_eq!(action.index() as usize, index, "{action:?}");
            assert_eq!(Action::from_index(index as u16), Some(*action));
        }
        assert_eq!(Action::from_index(36), None);
        assert_eq!(Action::from_index(u16::MAX), None);
    }

    #[test]
    fn config_ids_are_unique_and_roundtrip() {
        let mut seen = std::collections::HashSet::new();
        for action in ALL {
            let id = action.config_id();
            assert!(seen.insert(id), "duplicate config id {id}");
            assert_eq!(Action::from_config_id(id), Some(action));
        }
        assert_eq!(Action::from_config_id("window.nope"), None);
        assert_eq!(Action::from_config_id(""), None);
    }

    #[test]
    fn window_scoped_actions_are_the_window_group() {
        for action in ALL {
            assert_eq!(
                action.is_window_scoped(),
                action.group() == Group::WindowControl,
                "{action:?}"
            );
        }
        assert!(Action::Center.is_window_scoped());
        assert!(Action::RestorePrevious.is_window_scoped());
        assert!(!Action::ShowDesktop.is_window_scoped());
        // These act on every application, not on the frontmost window, so they must not be
        // suppressed by the "our own window is frontmost" rule.
        assert!(!Action::HideAllWindows.is_window_scoped());
        assert!(!Action::ShowAllWindows.is_window_scoped());
    }
}
