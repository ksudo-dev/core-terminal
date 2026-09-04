//! Profile values and loading for Terminal.
//!
//! The source profile documents are macOS compatibility inputs and are never
//! read by the application.  Runtime profiles are a small, project-owned JSON
//! document with the fields represented by [`TerminalProfile`]. Built-in
//! profiles keep protected names but may be edited and reset by the user.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Write},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const DEFAULT_PROFILE_NAME: &str = "Homebrew";
pub const PROFILE_SETTINGS_FILENAME: &str = "profiles.json";
pub const BUILTIN_PROFILE_NAMES: [&str; 10] = [
    "Basic",
    "Grass",
    "Homebrew",
    "Man Page",
    "Novel",
    "Ocean",
    "Pro",
    "Red Sands",
    "Silver Aerogel",
    "Solid Colors",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorShape {
    #[default]
    Block,
    IBeam,
    Underline,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellExitAction {
    #[default]
    Ask,
    CloseWindow,
    CloseTab,
    Keep,
}

/// Policy for a profile's startup child after it exits.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseOnExit {
    #[default]
    Never,
    Clean,
    Always,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskBeforeClosePolicy {
    #[default]
    Never,
    Always,
    NonExempt,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundImageMode {
    #[default]
    Tile,
    Scale,
    Center,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabTitlePolicy {
    #[default]
    Components,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeyMapping {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default)]
    pub action: String,
}

pub fn default_palette() -> Vec<String> {
    [
        "#000000", "#cc0000", "#4e9a06", "#c4a000", "#3465a4", "#75507b", "#06989a", "#d3d7cf",
        "#555753", "#ef2929", "#8ae234", "#fce94f", "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_key_mappings() -> Vec<KeyMapping> {
    [
        ("F1", "", r"\eOP"),
        ("F2", "", r"\eOQ"),
        ("F3", "", r"\eOR"),
        ("F4", "", r"\eOS"),
        ("F5", "", r"\e[15~"),
        ("F6", "", r"\e[17~"),
        ("F7", "", r"\e[18~"),
        ("F8", "", r"\e[19~"),
        ("F9", "", r"\e[20~"),
        ("F10", "", r"\e[21~"),
        ("F11", "", r"\e[23~"),
        ("F12", "", r"\e[24~"),
        ("F5", "Shift", r"\e[25~"),
        ("F6", "Shift", r"\e[26~"),
        ("F7", "Shift", r"\e[28~"),
        ("F8", "Shift", r"\e[29~"),
        ("F9", "Shift", r"\e[31~"),
        ("F10", "Shift", r"\e[32~"),
        ("F11", "Shift", r"\e[33~"),
        ("F12", "Shift", r"\e[34~"),
    ]
    .into_iter()
    .map(|(key, modifier, action)| KeyMapping {
        key: key.to_owned(),
        modifiers: (!modifier.is_empty())
            .then(|| modifier.to_owned())
            .into_iter()
            .collect(),
        action: action.to_owned(),
    })
    .collect()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TerminalProfile {
    pub name: String,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    #[serde(default = "default_bold_color", alias = "bold")]
    pub bold_color: String,
    #[serde(alias = "font_family")]
    pub font: String,
    pub font_size: f64,
    #[serde(default)]
    #[serde(alias = "cursor_style")]
    pub cursor_shape: CursorShape,
    #[serde(default = "default_true")]
    pub cursor_blink: bool,
    #[serde(default = "default_true", alias = "font_antialias")]
    pub antialias: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: u32,
    #[serde(default = "default_alpha")]
    pub background_alpha: f64,
    #[serde(default = "default_palette", alias = "ansi_colors", alias = "palette")]
    pub ansi_palette: Vec<String>,
    #[serde(default = "default_true")]
    pub use_ansi_colors: bool,
    #[serde(default = "default_true")]
    pub bold_is_bright: bool,
    #[serde(default = "default_true")]
    pub text_blink: bool,
    #[serde(default = "default_true", alias = "show_profile_name_in_title")]
    pub title_show_profile: bool,
    #[serde(default = "default_true", alias = "show_shell_in_title")]
    pub title_show_shell: bool,
    #[serde(default = "default_true", alias = "show_directory_in_title")]
    pub title_show_directory: bool,
    #[serde(default = "default_true", alias = "show_profile_name_in_tab")]
    pub tab_title_show_profile: bool,
    #[serde(default = "default_true", alias = "show_shell_in_tab")]
    pub tab_title_show_shell: bool,
    #[serde(default = "default_true", alias = "show_directory_in_tab")]
    pub tab_title_show_directory: bool,
    #[serde(default = "default_true", alias = "show_job_in_tab")]
    pub tab_title_show_job: bool,
    #[serde(default = "default_columns")]
    pub columns: u32,
    #[serde(default = "default_rows")]
    pub rows: u32,
    #[serde(default, alias = "scrollback_is_unlimited")]
    pub scrollback_unlimited: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback_limit: u32,
    #[serde(default)]
    pub restore_rows: bool,
    #[serde(default, alias = "shell_exit_behavior")]
    pub shell_exit_action: ShellExitAction,
    #[serde(default)]
    pub close_on_clean_exit: bool,
    #[serde(default)]
    pub close_on_error: bool,
    #[serde(default, alias = "ask_before_closing")]
    pub ask_before_close: bool,
    #[serde(default, alias = "ask_before_close_processes")]
    pub ask_before_close_exceptions: Vec<String>,
    #[serde(default, alias = "option_is_meta")]
    pub option_as_meta: bool,
    #[serde(default, alias = "alternate_screen_scroll_mode")]
    pub alternate_screen_scroll: bool,
    #[serde(default = "default_terminal_type")]
    pub terminal_type: String,
    #[serde(default, alias = "delete_sends_backspace")]
    pub delete_sends_control_h: bool,
    #[serde(default, alias = "escape_non_ascii_input")]
    pub escape_non_ascii: bool,
    #[serde(default, alias = "paste_newlines_carriage_return")]
    pub paste_newlines_as_cr: bool,
    #[serde(default = "default_true", alias = "application_keypad_mode")]
    pub application_keypad: bool,
    #[serde(default = "default_true")]
    pub scroll_on_input: bool,
    #[serde(default)]
    pub audible_bell: bool,
    #[serde(default, alias = "visual_bell_enabled")]
    pub visual_bell: bool,
    #[serde(default)]
    pub background_notifications: bool,
    #[serde(default, alias = "urgency")]
    pub urgency_hint: bool,
    #[serde(default = "default_true", alias = "utf8_mode")]
    pub utf8: bool,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_ambiguous_width", alias = "ambiguous_char_width")]
    pub ambiguous_width: u8,
    #[serde(default, alias = "command", alias = "startup_command")]
    pub shell_command: String,
    /// Optional login shell executable for this profile; empty uses `$SHELL`.
    #[serde(default, alias = "shell_executable", alias = "shell_path")]
    pub shell: String,
    #[serde(default = "default_true", alias = "run_command_inside_shell")]
    pub run_inside_shell: bool,
    #[serde(default)]
    pub close_on_exit: CloseOnExit,
    #[serde(default)]
    pub ask_before_close_policy: AskBeforeClosePolicy,
    #[serde(default)]
    pub custom_window_title: String,
    #[serde(default = "default_true")]
    pub title_show_working_directory: bool,
    #[serde(default = "default_true")]
    pub title_show_path: bool,
    #[serde(default = "default_true")]
    pub title_show_tty: bool,
    #[serde(default = "default_true")]
    pub title_show_process: bool,
    #[serde(default = "default_true")]
    pub title_show_arguments: bool,
    #[serde(default = "default_true")]
    pub title_show_dimensions: bool,
    #[serde(default = "default_true")]
    pub title_show_ctrl_key: bool,
    #[serde(default)]
    pub custom_tab_title: String,
    #[serde(default)]
    pub tab_title_policy: TabTitlePolicy,
    #[serde(default)]
    pub tab_title_show_other_items: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_activity: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_process: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_arguments: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_path: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_dimensions: bool,
    #[serde(default = "default_true")]
    pub tab_title_show_ctrl_key: bool,
    #[serde(default = "default_true")]
    pub smooth_resize: bool,
    #[serde(default = "default_scrollback")]
    pub restore_rows_limit: u32,
    #[serde(default)]
    pub restore_rows_bookmark: String,
    #[serde(default)]
    pub background_image_path: Option<String>,
    #[serde(default)]
    pub background_image_mode: BackgroundImageMode,
    #[serde(default = "default_true")]
    pub use_bold_fonts: bool,
    #[serde(default)]
    pub dynamic_colors: bool,
    #[serde(default = "default_key_mappings")]
    pub key_mappings: Vec<KeyMapping>,
    #[serde(default)]
    pub visual_bell_only_if_muted: bool,
    #[serde(default)]
    pub set_locale_environment: bool,
}

fn default_true() -> bool {
    true
}

fn default_scrollback() -> u32 {
    10_000
}

fn default_bold_color() -> String {
    "#ffffffff".into()
}

fn default_columns() -> u32 {
    80
}

fn default_rows() -> u32 {
    24
}

fn default_terminal_type() -> String {
    "xterm-256color".into()
}

fn default_locale() -> String {
    "".into()
}

fn default_ambiguous_width() -> u8 {
    1
}

fn sanitize_terminal_type(value: &str) -> String {
    let value = value.trim();
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_+.-".contains(character))
    {
        value.to_owned()
    } else {
        default_terminal_type()
    }
}

fn default_alpha() -> f64 {
    1.0
}

impl TerminalProfile {
    pub fn homebrew() -> Self {
        Self {
            name: DEFAULT_PROFILE_NAME.into(),
            foreground: "#00ff00ff".into(),
            background: "#000000ff".into(),
            cursor: "#00ff00ff".into(),
            selection: "#14532dff".into(),
            bold_color: "#ffffffff".into(),
            font: "Monospace 12".into(),
            font_size: 12.0,
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            antialias: true,
            scrollback_lines: default_scrollback(),
            background_alpha: 0.96,
            ansi_palette: default_palette(),
            use_ansi_colors: true,
            bold_is_bright: true,
            text_blink: true,
            title_show_profile: true,
            title_show_shell: true,
            title_show_directory: true,
            tab_title_show_profile: true,
            tab_title_show_shell: true,
            tab_title_show_directory: true,
            tab_title_show_job: true,
            columns: default_columns(),
            rows: default_rows(),
            scrollback_unlimited: false,
            scrollback_limit: default_scrollback(),
            restore_rows: false,
            shell_exit_action: ShellExitAction::Ask,
            close_on_clean_exit: false,
            close_on_error: false,
            ask_before_close: false,
            ask_before_close_exceptions: Vec::new(),
            option_as_meta: false,
            alternate_screen_scroll: false,
            terminal_type: default_terminal_type(),
            delete_sends_control_h: false,
            escape_non_ascii: false,
            paste_newlines_as_cr: false,
            application_keypad: true,
            scroll_on_input: true,
            audible_bell: false,
            visual_bell: false,
            background_notifications: false,
            urgency_hint: false,
            utf8: true,
            locale: default_locale(),
            ambiguous_width: default_ambiguous_width(),
            shell_command: String::new(),
            shell: String::new(),
            run_inside_shell: true,
            close_on_exit: CloseOnExit::Never,
            ask_before_close_policy: AskBeforeClosePolicy::Never,
            custom_window_title: String::new(),
            title_show_working_directory: true,
            title_show_path: true,
            title_show_tty: false,
            title_show_process: true,
            title_show_arguments: true,
            title_show_dimensions: false,
            title_show_ctrl_key: false,
            custom_tab_title: String::new(),
            tab_title_policy: TabTitlePolicy::Components,
            tab_title_show_other_items: false,
            tab_title_show_activity: true,
            tab_title_show_process: true,
            tab_title_show_arguments: false,
            tab_title_show_path: true,
            tab_title_show_dimensions: false,
            tab_title_show_ctrl_key: false,
            smooth_resize: true,
            restore_rows_limit: default_scrollback(),
            restore_rows_bookmark: String::new(),
            background_image_path: None,
            background_image_mode: BackgroundImageMode::Tile,
            use_bold_fonts: true,
            dynamic_colors: false,
            key_mappings: default_key_mappings(),
            visual_bell_only_if_muted: false,
            set_locale_environment: false,
        }
    }

    /// Return a usable profile even when a field is missing from a hand-edited
    /// project JSON file.
    pub fn normalized(mut self) -> Self {
        let defaults = Self::homebrew();
        if self.font.trim().is_empty() {
            self.font = "Monospace".into();
        }
        if !self.font_size.is_finite() {
            self.font_size = defaults.font_size;
        }
        self.font_size = self.font_size.clamp(6.0, 96.0);
        self.foreground = normalize_color(&self.foreground, &defaults.foreground);
        self.background = normalize_color(&self.background, &defaults.background);
        self.cursor = normalize_color(&self.cursor, &defaults.cursor);
        self.selection = normalize_color(&self.selection, &defaults.selection);
        self.bold_color = normalize_color(&self.bold_color, &defaults.bold_color);
        if !self.background_alpha.is_finite() {
            self.background_alpha = defaults.background_alpha;
        }
        self.background_alpha = self.background_alpha.clamp(0.0, 1.0);
        if self.ansi_palette.len() != 16 {
            self.ansi_palette = default_palette();
        } else {
            let defaults = default_palette();
            for (color, fallback) in self.ansi_palette.iter_mut().zip(defaults) {
                *color = normalize_color(color, &fallback);
            }
        }
        self.columns = self.columns.clamp(1, 1_000);
        self.rows = self.rows.clamp(1, 1_000);
        self.scrollback_limit = self.scrollback_limit.clamp(100, 1_000_000);
        self.terminal_type = sanitize_terminal_type(&self.terminal_type);
        self.shell_command = self.shell_command.trim().to_owned();
        self.shell = self.shell.trim().to_owned();
        if !self.shell.is_empty()
            && (!self.shell.starts_with('/') || self.shell.chars().any(char::is_whitespace))
        {
            self.shell.clear();
        }
        self.custom_window_title = self.custom_window_title.trim().to_owned();
        self.custom_tab_title = self.custom_tab_title.trim().to_owned();
        self.restore_rows_limit = self.restore_rows_limit.clamp(100, 1_000_000);
        self.restore_rows_bookmark = self.restore_rows_bookmark.trim().to_owned();
        self.background_image_path = self
            .background_image_path
            .map(|path| path.trim().to_owned())
            .filter(|path| !path.is_empty());
        self.key_mappings = self
            .key_mappings
            .into_iter()
            .map(|mut mapping| {
                mapping.key = mapping.key.trim().to_owned();
                mapping.modifiers = mapping
                    .modifiers
                    .into_iter()
                    .map(|modifier| modifier.trim().to_owned())
                    .filter(|modifier| !modifier.is_empty())
                    .collect();
                mapping.action = mapping.action.trim().to_owned();
                mapping
            })
            .filter(|mapping| !mapping.key.is_empty() && !mapping.action.is_empty())
            .collect();
        if self.close_on_exit == CloseOnExit::Never {
            self.close_on_exit = match (self.close_on_clean_exit, self.close_on_error) {
                (true, true) => CloseOnExit::Always,
                (true, false) => CloseOnExit::Clean,
                _ => CloseOnExit::Never,
            };
        }
        if self.ask_before_close_policy == AskBeforeClosePolicy::Never && self.ask_before_close {
            self.ask_before_close_policy = AskBeforeClosePolicy::Always;
        }
        self.locale = self.locale.trim().to_owned();
        if !self.locale.is_empty()
            && !self
                .locale
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_.@+-".contains(character))
        {
            self.locale.clear();
        }
        self.ambiguous_width = self.ambiguous_width.clamp(1, 2);
        self.ask_before_close_exceptions = self
            .ask_before_close_exceptions
            .into_iter()
            .map(|process| process.trim().to_owned())
            .filter(|process| !process.is_empty())
            .collect();
        self
    }
}

fn normalize_color(value: &str, fallback: &str) -> String {
    let value = value.trim();
    let valid_hex = matches!(value.len(), 7 | 9)
        && value.starts_with('#')
        && value.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit());
    if valid_hex {
        value.to_owned()
    } else {
        fallback.to_owned()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ProfileDocument {
    #[serde(default)]
    pub default_profile: Option<String>,
    pub profiles: Vec<TerminalProfile>,
    #[serde(default)]
    pub window_groups: Vec<WindowGroup>,
}

/// A terminal instance in a saved window group. The working directory is
/// data only: callers decide whether it still exists before spawning a child.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WindowGroupEntry {
    pub profile: String,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default = "default_columns")]
    pub columns: u32,
    #[serde(default = "default_rows")]
    pub rows: u32,
}

impl WindowGroupEntry {
    fn normalized(mut self) -> Self {
        self.profile = self.profile.trim().to_owned();
        self.working_directory = self
            .working_directory
            .map(|path| path.trim().to_owned())
            .filter(|path| !path.is_empty());
        self.columns = self.columns.clamp(1, 1_000);
        self.rows = self.rows.clamp(1, 1_000);
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WindowGroup {
    pub name: String,
    #[serde(default)]
    pub entries: Vec<WindowGroupEntry>,
}

impl WindowGroup {
    fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_owned();
        self.entries = self
            .entries
            .into_iter()
            .map(WindowGroupEntry::normalized)
            .collect();
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("could not read profile file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid profile JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("profile document contains no profiles")]
    Empty,
    #[error("profile file is larger than the safe limit")]
    TooLarge,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileMutationError {
    #[error("profile name cannot be empty")]
    EmptyName,
    #[error("a profile named `{0}` already exists")]
    DuplicateName(String),
    #[error("profile `{0}` does not exist")]
    Missing(String),
    #[error("built-in profile `{0}` is protected")]
    Protected(String),
    #[error("at least one profile must remain")]
    LastProfile,
    #[error("profile `{profile}` is used by window group `{group}`")]
    ProfileUsedByWindowGroup { profile: String, group: String },
    #[error("window group name cannot be empty")]
    EmptyWindowGroupName,
    #[error("window group must contain at least one entry")]
    EmptyWindowGroup,
    #[error("window group `{0}` already exists")]
    DuplicateWindowGroup(String),
    #[error("window group `{0}` does not exist")]
    MissingWindowGroup(String),
    #[error("window group entry refers to missing profile `{0}`")]
    MissingWindowGroupProfile(String),
    #[error("window group entry dimensions must be between 1 and 1000 columns/rows")]
    InvalidWindowGroupDimensions,
    #[error("window group working directories must be absolute, bounded paths without control characters")]
    InvalidWindowGroupDirectory,
}

#[derive(Clone, Debug)]
pub struct ProfileStore {
    profiles: Vec<TerminalProfile>,
    selected: String,
    builtins: Vec<String>,
    builtin_defaults: Vec<TerminalProfile>,
    window_groups: Vec<WindowGroup>,
}

fn validate_window_group_shape(group: &WindowGroup) -> Result<(), ProfileMutationError> {
    if group.name.trim().is_empty() {
        return Err(ProfileMutationError::EmptyWindowGroupName);
    }
    if group.entries.is_empty() {
        return Err(ProfileMutationError::EmptyWindowGroup);
    }
    if group
        .entries
        .iter()
        .any(|entry| !(1..=1_000).contains(&entry.columns) || !(1..=1_000).contains(&entry.rows))
    {
        return Err(ProfileMutationError::InvalidWindowGroupDimensions);
    }
    if group.entries.iter().any(|entry| {
        entry
            .working_directory
            .as_deref()
            .is_some_and(|path| !valid_window_group_directory(path))
    }) {
        return Err(ProfileMutationError::InvalidWindowGroupDirectory);
    }
    Ok(())
}

fn valid_window_group_directory(path: &str) -> bool {
    const MAX_DIRECTORY_BYTES: usize = 4096;
    let path = path.trim();
    path.is_empty()
        || (path.len() <= MAX_DIRECTORY_BYTES
            && Path::new(path).is_absolute()
            && !path.chars().any(char::is_control))
}

impl ProfileStore {
    pub fn new(profiles: Vec<TerminalProfile>) -> Result<Self, ProfileError> {
        Self::new_with_window_groups(profiles, Vec::new())
    }

    fn new_with_window_groups(
        profiles: Vec<TerminalProfile>,
        window_groups: Vec<WindowGroup>,
    ) -> Result<Self, ProfileError> {
        if profiles.is_empty() {
            return Err(ProfileError::Empty);
        }
        let profiles = profiles
            .into_iter()
            .map(TerminalProfile::normalized)
            .collect::<Vec<_>>();
        let selected = profiles
            .iter()
            .find(|profile| profile.name == DEFAULT_PROFILE_NAME)
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| profiles[0].name.clone());
        let builtins = profiles
            .iter()
            .filter(|profile| BUILTIN_PROFILE_NAMES.contains(&profile.name.as_str()))
            .map(|profile| profile.name.clone())
            .collect();
        let builtin_defaults = profiles
            .iter()
            .filter(|profile| BUILTIN_PROFILE_NAMES.contains(&profile.name.as_str()))
            .cloned()
            .collect();
        let profile_names = profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let window_groups = window_groups
            .into_iter()
            .map(WindowGroup::normalized)
            .filter(|group| {
                !group.name.is_empty()
                    && !group.entries.is_empty()
                    && group.entries.iter().all(|entry| {
                        profile_names.contains(entry.profile.as_str())
                            && (1..=1_000).contains(&entry.columns)
                            && (1..=1_000).contains(&entry.rows)
                            && entry
                                .working_directory
                                .as_deref()
                                .is_none_or(valid_window_group_directory)
                    })
            })
            .collect();
        Ok(Self {
            profiles,
            selected,
            builtins,
            builtin_defaults,
            window_groups,
        })
    }

    pub fn defaults() -> Self {
        // These values are deliberately small and self-contained so the app
        // still opens during development if the installed data file is absent.
        let mut profiles = Vec::with_capacity(BUILTIN_PROFILE_NAMES.len());
        for name in BUILTIN_PROFILE_NAMES {
            let mut profile = TerminalProfile::homebrew();
            profile.name = name.into();
            if name == "Basic" || name == "Man Page" || name == "Solid Colors" {
                profile.foreground = "#000000".into();
                profile.background = "#ffffff".into();
            }
            profiles.push(profile);
        }
        // Safe because the literal list above is non-empty.
        Self::new(profiles).expect("built-in profiles are non-empty")
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ProfileError> {
        Self::load_from_path_with_permissions(path, true)
    }

    fn load_from_path_with_permissions(
        path: impl AsRef<Path>,
        repair_private_permissions: bool,
    ) -> Result<Self, ProfileError> {
        const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;
        let path = path.as_ref();
        if fs::metadata(path)?.len() > MAX_PROFILE_BYTES {
            return Err(ProfileError::TooLarge);
        }
        let content = fs::read_to_string(path)?;
        #[cfg(unix)]
        {
            if repair_private_permissions
                && fs::symlink_metadata(path)
                    .map(|metadata| !metadata.file_type().is_symlink())
                    .unwrap_or(false)
            {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            }
        }
        Self::load_from_str(&content)
    }

    /// Load profiles from the installed data directory (or the checkout's
    /// `data/` directory during development), falling back to the safe built-in
    /// set when neither file is present.
    pub fn load_project_defaults() -> Self {
        let mut candidates = std::env::var_os("XDG_DATA_DIRS")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join("core-terminal/default-profiles.json"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        candidates.push(Path::new("/usr/share/core-terminal/default-profiles.json").to_path_buf());
        candidates.push(Path::new("data/default-profiles.json").to_path_buf());
        candidates
            .into_iter()
            .find_map(|path| Self::load_from_path_with_permissions(path, false).ok())
            .unwrap_or_else(Self::defaults)
    }

    pub fn config_path() -> Option<std::path::PathBuf> {
        if let Ok(path) = std::env::var("XDG_CONFIG_HOME") {
            if !path.trim().is_empty() && Path::new(&path).is_absolute() {
                return Some(
                    Path::new(&path)
                        .join("core-terminal")
                        .join(PROFILE_SETTINGS_FILENAME),
                );
            }
        }
        std::env::var_os("HOME").map(|home| {
            Path::new(&home)
                .join(".config")
                .join("core-terminal")
                .join(PROFILE_SETTINGS_FILENAME)
        })
    }

    /// Load project defaults, then merge user-owned profile overrides, custom
    /// profiles, and window groups. Built-in names remain protected from
    /// deletion while their editable settings persist like Terminal profiles.
    pub fn load_user_or_defaults() -> Self {
        let mut store = Self::load_project_defaults();
        let Some(path) = Self::config_path() else {
            return store;
        };
        let Ok(user) = Self::load_from_path(path) else {
            return store;
        };
        store.merge_user(user);
        store
    }

    fn merge_user(&mut self, user: Self) {
        let selected = user.selected.clone();
        for profile in user.profiles {
            if self.is_builtin(&profile.name) {
                if let Some(index) = self
                    .profiles
                    .iter()
                    .position(|candidate| candidate.name == profile.name)
                {
                    self.profiles[index] = profile.normalized();
                }
            } else {
                let _ = self.add_profile(profile);
            }
        }
        for group in user.window_groups {
            let _ = self.add_window_group(group);
        }
        if self.profile(&selected).is_some() {
            let _ = self.set_default(&selected);
        }
    }

    pub fn load_from_str(content: &str) -> Result<Self, ProfileError> {
        let document: ProfileDocument = serde_json::from_str(content)?;
        let mut store = Self::new_with_window_groups(document.profiles, document.window_groups)?;
        if let Some(default_profile) = document.default_profile {
            // A malformed default should not prevent the application from
            // opening; use the first/Homebrew profile selected by `new`.
            store.select(&default_profile);
        }
        Ok(store)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.profiles.iter().map(|profile| profile.name.as_str())
    }

    pub fn profiles(&self) -> &[TerminalProfile] {
        &self.profiles
    }

    pub fn window_groups(&self) -> &[WindowGroup] {
        &self.window_groups
    }

    pub fn window_group(&self, name: &str) -> Option<&WindowGroup> {
        self.window_groups.iter().find(|group| group.name == name)
    }

    pub fn add_window_group(&mut self, group: WindowGroup) -> Result<(), ProfileMutationError> {
        validate_window_group_shape(&group)?;
        let group = group.normalized();
        self.validate_window_group(&group)?;
        if self.window_group(&group.name).is_some() {
            return Err(ProfileMutationError::DuplicateWindowGroup(group.name));
        }
        self.window_groups.push(group);
        Ok(())
    }

    pub fn update_window_group(&mut self, group: WindowGroup) -> Result<(), ProfileMutationError> {
        validate_window_group_shape(&group)?;
        let group = group.normalized();
        self.validate_window_group(&group)?;
        let index = self
            .window_groups
            .iter()
            .position(|candidate| candidate.name == group.name)
            .ok_or_else(|| ProfileMutationError::MissingWindowGroup(group.name.clone()))?;
        self.window_groups[index] = group;
        Ok(())
    }

    /// Replace a window group in place, including an optional name change.
    /// Validation completes before the stored group is mutated, so a failed
    /// rename cannot leave both the old and new names behind.
    pub fn rename_window_group(
        &mut self,
        old_name: &str,
        group: WindowGroup,
    ) -> Result<(), ProfileMutationError> {
        validate_window_group_shape(&group)?;
        let group = group.normalized();
        self.validate_window_group(&group)?;
        let index = self
            .window_groups
            .iter()
            .position(|candidate| candidate.name == old_name)
            .ok_or_else(|| ProfileMutationError::MissingWindowGroup(old_name.to_owned()))?;
        if group.name != old_name
            && self
                .window_groups
                .iter()
                .enumerate()
                .any(|(candidate_index, candidate)| {
                    candidate_index != index && candidate.name == group.name
                })
        {
            return Err(ProfileMutationError::DuplicateWindowGroup(group.name));
        }
        self.window_groups[index] = group;
        Ok(())
    }

    pub fn delete_window_group(&mut self, name: &str) -> Result<WindowGroup, ProfileMutationError> {
        let index = self
            .window_groups
            .iter()
            .position(|group| group.name == name)
            .ok_or_else(|| ProfileMutationError::MissingWindowGroup(name.to_owned()))?;
        Ok(self.window_groups.remove(index))
    }

    fn validate_window_group(&self, group: &WindowGroup) -> Result<(), ProfileMutationError> {
        for entry in &group.entries {
            if self.profile(&entry.profile).is_none() {
                return Err(ProfileMutationError::MissingWindowGroupProfile(
                    entry.profile.clone(),
                ));
            }
            if !(1..=1_000).contains(&entry.columns) || !(1..=1_000).contains(&entry.rows) {
                return Err(ProfileMutationError::InvalidWindowGroupDimensions);
            }
        }
        Ok(())
    }

    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtins.iter().any(|builtin| builtin == name)
    }

    pub fn selected_name(&self) -> &str {
        &self.selected
    }

    pub fn default_profile_name(&self) -> &str {
        &self.selected
    }

    pub fn selected(&self) -> &TerminalProfile {
        self.profile(&self.selected)
            .expect("selected profile is always present")
    }

    pub fn profile(&self, name: &str) -> Option<&TerminalProfile> {
        self.profiles.iter().find(|profile| profile.name == name)
    }

    pub fn update_profile(&mut self, profile: TerminalProfile) -> Result<(), ProfileMutationError> {
        let name = profile.name.trim().to_owned();
        let index = self
            .profiles
            .iter()
            .position(|candidate| candidate.name == name)
            .ok_or_else(|| ProfileMutationError::Missing(name.clone()))?;
        let mut profile = profile;
        profile.name = name;
        self.profiles[index] = profile.normalized();
        Ok(())
    }

    pub fn select(&mut self, name: &str) -> bool {
        if self.profile(name).is_some() {
            self.selected = name.to_owned();
            true
        } else {
            false
        }
    }

    pub fn set_default(&mut self, name: &str) -> Result<(), ProfileMutationError> {
        if self.profile(name).is_none() {
            return Err(ProfileMutationError::Missing(name.to_owned()));
        }
        self.selected = name.to_owned();
        Ok(())
    }

    pub fn set_default_profile(&mut self, name: &str) -> Result<(), ProfileMutationError> {
        self.set_default(name)
    }

    pub fn add_profile(
        &mut self,
        mut profile: TerminalProfile,
    ) -> Result<(), ProfileMutationError> {
        profile.name = profile.name.trim().to_owned();
        if profile.name.is_empty() {
            return Err(ProfileMutationError::EmptyName);
        }
        if self.profile(&profile.name).is_some() {
            return Err(ProfileMutationError::DuplicateName(profile.name));
        }
        self.profiles.push(profile.normalized());
        Ok(())
    }

    pub fn duplicate_profile(
        &mut self,
        source: &str,
        new_name: impl Into<String>,
    ) -> Result<(), ProfileMutationError> {
        let mut duplicate = self
            .profile(source)
            .cloned()
            .ok_or_else(|| ProfileMutationError::Missing(source.to_owned()))?;
        duplicate.name = new_name.into();
        self.add_profile(duplicate)
    }

    pub fn delete_profile(&mut self, name: &str) -> Result<TerminalProfile, ProfileMutationError> {
        if self.is_builtin(name) {
            return Err(ProfileMutationError::Protected(name.to_owned()));
        }
        if self.profiles.len() == 1 {
            return Err(ProfileMutationError::LastProfile);
        }
        if let Some(group) = self
            .window_groups
            .iter()
            .find(|group| group.entries.iter().any(|entry| entry.profile == name))
        {
            return Err(ProfileMutationError::ProfileUsedByWindowGroup {
                profile: name.to_owned(),
                group: group.name.clone(),
            });
        }
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
            .ok_or_else(|| ProfileMutationError::Missing(name.to_owned()))?;
        let removed = self.profiles.remove(index);
        if self.selected == name {
            self.selected = self
                .profiles
                .iter()
                .find(|profile| profile.name == DEFAULT_PROFILE_NAME)
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| self.profiles[0].name.clone());
        }
        Ok(removed)
    }

    pub fn reset_overrides(&mut self, name: &str) -> Result<(), ProfileMutationError> {
        if self.is_builtin(name) {
            let replacement = self
                .builtin_defaults
                .iter()
                .find(|profile| profile.name == name)
                .cloned()
                .ok_or_else(|| ProfileMutationError::Missing(name.to_owned()))?;
            let index = self
                .profiles
                .iter()
                .position(|profile| profile.name == name)
                .ok_or_else(|| ProfileMutationError::Missing(name.to_owned()))?;
            self.profiles[index] = replacement;
            return Ok(());
        }
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
            .ok_or_else(|| ProfileMutationError::Missing(name.to_owned()))?;
        // Custom profiles have no mutable built-in backing object. Resetting
        // their overrides therefore restores the portable Homebrew baseline
        // while retaining the user's profile name.
        let profile_name = self.profiles[index].name.clone();
        let mut replacement = TerminalProfile::homebrew();
        replacement.name = profile_name;
        self.profiles[index] = replacement;
        Ok(())
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ProfileError> {
        let path = path.as_ref();
        let document = ProfileDocument {
            default_profile: Some(self.selected.clone()),
            profiles: self.profiles.clone(),
            window_groups: self.window_groups.clone(),
        };
        let content = serde_json::to_string_pretty(&document)? + "\n";
        atomic_write_private(path, content.as_bytes())?;
        Ok(())
    }

    pub fn reset_profile_overrides(&mut self, name: &str) -> Result<(), ProfileMutationError> {
        self.reset_overrides(name)
    }

    pub fn save_user(&self) -> Result<(), ProfileError> {
        Self::config_path()
            .ok_or_else(|| ProfileError::Io(std::io::Error::other("config path unavailable")))
            .and_then(|path| self.save_to_path(path))
    }

    /// Restore the built-in profiles and select Homebrew again.
    pub fn restore_defaults(&mut self) {
        *self = Self::load_project_defaults();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfilePlistError {
    #[error("profile plist is larger than the safe import limit")]
    TooLarge,
    #[error("unsafe plist construct is not accepted")]
    Unsafe,
    #[error("invalid profile plist: {0}")]
    Plist(#[from] plist::Error),
    #[error("could not read/write profile plist: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile plist has an unsupported root or value: {0}")]
    InvalidValue(String),
    #[error("profile plist is missing a name")]
    MissingName,
}

/// A successfully imported profile plus fields whose macOS archive encoding
/// could not be represented safely. Terminal.app stores colors and fonts as
/// `NSData` keyed archives; those values are intentionally not deserialized or
/// executed. Callers can surface these fallbacks to the user.
#[derive(Clone, Debug, PartialEq)]
pub struct PlistImport {
    pub profile: TerminalProfile,
    pub fallbacks: Vec<String>,
}

/// Import a bounded XML `.terminal` plist. Scalar values and the bounded
/// NSKeyedArchiver color/font records used by Terminal are decoded as data.
/// No imported field is ever executed.
pub fn import_terminal_plist(xml: &str) -> Result<TerminalProfile, ProfilePlistError> {
    Ok(import_terminal_plist_with_report(xml)?.profile)
}

pub fn import_terminal_plist_from_path(
    path: impl AsRef<Path>,
) -> Result<PlistImport, ProfilePlistError> {
    const MAX_PLIST_BYTES: u64 = 4 * 1024 * 1024;
    let path = path.as_ref();
    if fs::metadata(path)?.len() > MAX_PLIST_BYTES {
        return Err(ProfilePlistError::TooLarge);
    }
    let bytes = fs::read(path)?;
    import_terminal_plist_bytes_with_report(&bytes)
}

pub fn import_terminal_plist_with_report(xml: &str) -> Result<PlistImport, ProfilePlistError> {
    import_terminal_plist_bytes_with_report(xml.as_bytes())
}

fn import_terminal_plist_bytes_with_report(bytes: &[u8]) -> Result<PlistImport, ProfilePlistError> {
    const MAX_PLIST_BYTES: usize = 4 * 1024 * 1024;
    if bytes.len() > MAX_PLIST_BYTES {
        return Err(ProfilePlistError::TooLarge);
    }
    // plist's XML reader does not resolve external entities, but reject these
    // constructs explicitly so this boundary remains safe if its parser ever
    // changes and to keep the import contract clear.
    if let Ok(xml) = std::str::from_utf8(bytes) {
        let lower = xml.to_ascii_lowercase();
        if lower.contains("<!entity")
            || (lower.contains("<!doctype") && !lower.contains("apple//dtd plist 1.0"))
        {
            return Err(ProfilePlistError::Unsafe);
        }
    }
    let value = plist::Value::from_reader(Cursor::new(bytes))?;
    import_terminal_plist_value(value)
}

fn import_terminal_plist_value(value: plist::Value) -> Result<PlistImport, ProfilePlistError> {
    let dictionary = value
        .as_dictionary()
        .ok_or_else(|| ProfilePlistError::InvalidValue("root is not a dictionary".into()))?;
    let mut profile = TerminalProfile::homebrew();
    let mut fallbacks = Vec::new();
    for (key, value) in dictionary {
        match key.as_str() {
            "name" => set_string(value, &mut profile.name, key, &mut fallbacks),
            "foreground" => set_color(value, &mut profile.foreground, key, &mut fallbacks),
            "background" => set_color(value, &mut profile.background, key, &mut fallbacks),
            "TextColor" => set_color(value, &mut profile.foreground, key, &mut fallbacks),
            "BackgroundColor" => set_color(value, &mut profile.background, key, &mut fallbacks),
            "TextBoldColor" => set_color(value, &mut profile.bold_color, key, &mut fallbacks),
            "SelectionColor" => set_color(value, &mut profile.selection, key, &mut fallbacks),
            "CursorColor" => set_color(value, &mut profile.cursor, key, &mut fallbacks),
            "bold_color" | "bold" => set_color(value, &mut profile.bold_color, key, &mut fallbacks),
            "cursor" => set_color(value, &mut profile.cursor, key, &mut fallbacks),
            "selection" => set_color(value, &mut profile.selection, key, &mut fallbacks),
            "font" | "font_family" => set_font(value, &mut profile, key, &mut fallbacks),
            "Font" => set_font(value, &mut profile, key, &mut fallbacks),
            "font_size" => {
                if let Some(size) = value
                    .as_real()
                    .or_else(|| value.as_signed_integer().map(|v| v as f64))
                {
                    profile.font_size = size;
                } else {
                    fallbacks.push(key.clone());
                }
            }
            "background_alpha" => {
                if let Some(alpha) = value
                    .as_real()
                    .or_else(|| value.as_signed_integer().map(|value| value as f64))
                {
                    profile.background_alpha = alpha;
                } else {
                    fallbacks.push(key.clone());
                }
            }
            "columns" => {
                if let Some(columns) = value.as_unsigned_integer().or_else(|| {
                    value
                        .as_signed_integer()
                        .and_then(|value| u64::try_from(value).ok())
                }) {
                    profile.columns = columns as u32;
                } else {
                    fallbacks.push(key.clone());
                }
            }
            "rows" => {
                if let Some(rows) = value.as_unsigned_integer().or_else(|| {
                    value
                        .as_signed_integer()
                        .and_then(|value| u64::try_from(value).ok())
                }) {
                    profile.rows = rows as u32;
                } else {
                    fallbacks.push(key.clone());
                }
            }
            "cursor_shape" => {
                set_cursor_shape(value, &mut profile.cursor_shape, key, &mut fallbacks)
            }
            "CursorType" => set_cursor_shape(value, &mut profile.cursor_shape, key, &mut fallbacks),
            "cursor_blink" => set_bool(value, &mut profile.cursor_blink, key, &mut fallbacks),
            "CursorBlink" => set_bool(value, &mut profile.cursor_blink, key, &mut fallbacks),
            "terminal_type" => set_string(value, &mut profile.terminal_type, key, &mut fallbacks),
            // `type` identifies the Terminal.app document, not a profile value.
            "type" => {}
            _ => {}
        }
    }
    if profile.name.trim().is_empty() {
        return Err(ProfilePlistError::MissingName);
    }
    Ok(PlistImport {
        profile: profile.normalized(),
        fallbacks,
    })
}

fn set_string(
    value: &plist::Value,
    destination: &mut String,
    key: &str,
    fallbacks: &mut Vec<String>,
) {
    if let Some(string) = value.as_string() {
        *destination = string.to_owned();
    } else {
        fallbacks.push(key.to_owned());
    }
}

fn set_color(
    value: &plist::Value,
    destination: &mut String,
    key: &str,
    fallbacks: &mut Vec<String>,
) {
    if let Some(string) = value.as_string() {
        *destination = string.to_owned();
    } else if let Some(data) = value.as_data() {
        if let Some(color) = archived_color(data) {
            *destination = color;
        } else {
            fallbacks.push(key.to_owned());
        }
    } else {
        fallbacks.push(key.to_owned());
    }
}

fn set_font(
    value: &plist::Value,
    profile: &mut TerminalProfile,
    key: &str,
    fallbacks: &mut Vec<String>,
) {
    if let Some(string) = value.as_string() {
        profile.font = string.to_owned();
    } else if let Some(data) = value.as_data() {
        if let Some((font, size)) = archived_font(data) {
            profile.font = font;
            if let Some(size) = size {
                profile.font_size = size;
            }
        } else {
            fallbacks.push(key.to_owned());
        }
    } else {
        fallbacks.push(key.to_owned());
    }
}

fn archived_objects(data: &[u8]) -> Option<Vec<plist::Value>> {
    if data.len() > 4 * 1024 * 1024 {
        return None;
    }
    let archive = plist::Value::from_reader(Cursor::new(data)).ok()?;
    archive
        .as_dictionary()?
        .get("$objects")
        .and_then(plist::Value::as_array)
        .cloned()
}

fn archived_color(data: &[u8]) -> Option<String> {
    let objects = archived_objects(data)?;
    for object in &objects {
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        let Some(encoded) = dictionary
            .get("NSRGB")
            .or_else(|| dictionary.get("NSWhite"))
            .and_then(|value| archived_string_value(value, &objects))
        else {
            continue;
        };
        let values = encoded
            .split_whitespace()
            .filter_map(|value| value.parse::<f64>().ok())
            .collect::<Vec<_>>();
        if values.len() == 3 || values.len() == 4 {
            return Some(format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                color_byte(values[0]),
                color_byte(values[1]),
                color_byte(values[2]),
                color_byte(values.get(3).copied().unwrap_or(1.0))
            ));
        }
        if values.len() == 1 || values.len() == 2 {
            let white = color_byte(values[0]);
            return Some(format!(
                "#{white:02x}{white:02x}{white:02x}{:02x}",
                color_byte(values.get(1).copied().unwrap_or(1.0))
            ));
        }
    }
    None
}

fn archived_font(data: &[u8]) -> Option<(String, Option<f64>)> {
    let objects = archived_objects(data)?;
    for object in &objects {
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        let Some(name) = dictionary
            .get("NSName")
            .and_then(|value| archived_string_value(value, &objects))
        else {
            continue;
        };
        let size = dictionary
            .get("NSSize")
            .and_then(plist::Value::as_real)
            .or_else(|| {
                dictionary
                    .get("NSSize")
                    .and_then(plist::Value::as_signed_integer)
                    .map(|value| value as f64)
            });
        return Some((name, size));
    }
    None
}

fn archived_string_value(value: &plist::Value, objects: &[plist::Value]) -> Option<String> {
    let mut current = value;
    // UIDs in NSKeyedArchiver are normally one hop. Bound traversal so a
    // hostile archive containing a UID cycle cannot recurse indefinitely.
    for _ in 0..objects.len().min(128) {
        match current {
            plist::Value::String(value) => return Some(value.clone()),
            plist::Value::Data(value) => {
                return std::str::from_utf8(value)
                    .ok()
                    .map(|text| text.trim_end_matches('\0').to_owned())
            }
            plist::Value::Uid(uid) => current = objects.get(uid.get() as usize)?,
            _ => return None,
        }
    }
    None
}

fn color_byte(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn set_bool(value: &plist::Value, destination: &mut bool, key: &str, fallbacks: &mut Vec<String>) {
    if let Some(boolean) = value.as_boolean() {
        *destination = boolean;
    } else {
        fallbacks.push(key.to_owned());
    }
}

fn set_cursor_shape(
    value: &plist::Value,
    destination: &mut CursorShape,
    key: &str,
    fallbacks: &mut Vec<String>,
) {
    if let Some(shape) = value.as_string() {
        *destination = match shape.to_ascii_lowercase().as_str() {
            "ibeam" | "i-beam" | "1" => CursorShape::IBeam,
            "underline" | "2" => CursorShape::Underline,
            "block" | "0" => CursorShape::Block,
            _ => {
                fallbacks.push(key.to_owned());
                return;
            }
        };
    } else if let Some(shape) = value.as_signed_integer() {
        *destination = match shape {
            1 => CursorShape::IBeam,
            2 => CursorShape::Underline,
            0 => CursorShape::Block,
            _ => {
                fallbacks.push(key.to_owned());
                return;
            }
        };
    } else {
        fallbacks.push(key.to_owned());
    }
}

/// Export a safe, scalar XML plist. Apple keyed archives are not emitted;
/// this format is intentionally portable and can be re-imported losslessly.
pub fn export_terminal_plist(profile: &TerminalProfile) -> Result<String, ProfilePlistError> {
    let p = profile.clone().normalized();
    let mut dictionary = plist::Dictionary::new();
    dictionary.insert("name".into(), p.name.into());
    dictionary.insert("foreground".into(), p.foreground.into());
    dictionary.insert("background".into(), p.background.into());
    dictionary.insert("bold_color".into(), p.bold_color.into());
    dictionary.insert("cursor".into(), p.cursor.into());
    dictionary.insert("selection".into(), p.selection.into());
    dictionary.insert("font".into(), p.font.into());
    dictionary.insert("font_size".into(), p.font_size.into());
    dictionary.insert("background_alpha".into(), p.background_alpha.into());
    dictionary.insert("columns".into(), p.columns.into());
    dictionary.insert("rows".into(), p.rows.into());
    dictionary.insert(
        "cursor_shape".into(),
        format!("{:?}", p.cursor_shape).into(),
    );
    dictionary.insert("cursor_blink".into(), p.cursor_blink.into());
    dictionary.insert("terminal_type".into(), p.terminal_type.into());
    let mut output = Vec::new();
    plist::Value::Dictionary(dictionary).to_writer_xml(&mut output)?;
    String::from_utf8(output).map_err(|error| ProfilePlistError::InvalidValue(error.to_string()))
}

pub fn export_terminal_plist_to_path(
    profile: &TerminalProfile,
    path: impl AsRef<Path>,
) -> Result<(), ProfilePlistError> {
    let path = path.as_ref();
    atomic_write_private(path, export_terminal_plist(profile)?.as_bytes())?;
    Ok(())
}

fn atomic_write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("write.{}.{nonce}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        drop(file);
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_project_profiles_and_selects_homebrew() {
        let json = r##"{"profiles":[
          {"name":"Basic","foreground":"#111111","background":"#eeeeee","cursor":"#111111","selection":"#cccccc","font":"Monospace","font_size":11.0},
          {"name":"Homebrew","foreground":"#29ff14","background":"#000000","cursor":"#38ff27","selection":"#8c0017","font":"Monospace","font_size":12.0}
        ]}"##;
        let store = ProfileStore::load_from_str(json).unwrap();
        assert_eq!(store.names().count(), 2);
        assert_eq!(store.selected_name(), DEFAULT_PROFILE_NAME);
        assert_eq!(store.selected().font_size, 12.0);
    }

    #[test]
    fn selection_and_default_restoration_work() {
        let mut store = ProfileStore::defaults();
        assert!(store.select("Ocean"));
        assert_eq!(store.selected_name(), "Ocean");
        assert!(!store.select("not a profile"));
        store.restore_defaults();
        assert_eq!(store.selected_name(), DEFAULT_PROFILE_NAME);
        assert_eq!(store.names().count(), 10);
    }

    #[test]
    fn empty_documents_are_rejected() {
        let error = ProfileStore::load_from_str(r#"{"profiles":[]}"#).unwrap_err();
        assert!(matches!(error, ProfileError::Empty));
    }

    #[test]
    fn loads_the_project_owned_default_profile_document() {
        let store =
            ProfileStore::load_from_str(include_str!("../data/default-profiles.json")).unwrap();
        assert_eq!(store.names().count(), 10);
        assert_eq!(store.selected_name(), DEFAULT_PROFILE_NAME);
        assert_eq!(store.profile("Homebrew").unwrap().font, "Monospace");
    }

    #[test]
    fn project_document_contains_the_ten_reference_names() {
        let store =
            ProfileStore::load_from_str(include_str!("../data/default-profiles.json")).unwrap();
        assert_eq!(store.names().collect::<Vec<_>>(), BUILTIN_PROFILE_NAMES);
        let homebrew = store.profile(DEFAULT_PROFILE_NAME).unwrap();
        assert_eq!(homebrew.foreground, "#00ff00ff");
        assert_eq!(homebrew.cursor, "#00ff00ff");
        assert_eq!(homebrew.background_alpha, 0.96);
        assert!(homebrew.cursor_blink);
    }

    #[test]
    fn custom_profiles_can_be_duplicated_deleted_and_defaulted() {
        let mut store = ProfileStore::defaults();
        store
            .duplicate_profile(DEFAULT_PROFILE_NAME, "My Profile")
            .unwrap();
        assert!(!store.is_builtin("My Profile"));
        store.set_default("My Profile").unwrap();
        assert_eq!(store.selected_name(), "My Profile");
        assert_eq!(
            store.delete_profile("My Profile").unwrap().name,
            "My Profile"
        );
        assert_eq!(store.selected_name(), DEFAULT_PROFILE_NAME);
        assert_eq!(
            store.delete_profile(DEFAULT_PROFILE_NAME),
            Err(ProfileMutationError::Protected(DEFAULT_PROFILE_NAME.into()))
        );
    }

    #[test]
    fn resetting_custom_profile_restores_baseline_but_keeps_name() {
        let mut store = ProfileStore::defaults();
        store
            .duplicate_profile(DEFAULT_PROFILE_NAME, "Custom")
            .unwrap();
        let mut custom = store.profile("Custom").unwrap().clone();
        custom.font_size = 42.0;
        store.update_profile(custom).unwrap();
        store.reset_overrides("Custom").unwrap();
        assert_eq!(store.profile("Custom").unwrap().font_size, 12.0);
        assert_eq!(store.profile("Custom").unwrap().name, "Custom");
    }

    #[test]
    fn built_in_edits_merge_and_reset_without_allowing_deletion() {
        let mut user = ProfileStore::defaults();
        let mut edited = user.profile(DEFAULT_PROFILE_NAME).unwrap().clone();
        edited.font_size = 18.0;
        edited.foreground = "#123456ff".into();
        user.update_profile(edited).unwrap();

        let mut merged = ProfileStore::defaults();
        merged.merge_user(user);
        let homebrew = merged.profile(DEFAULT_PROFILE_NAME).unwrap();
        assert_eq!(homebrew.font_size, 18.0);
        assert_eq!(homebrew.foreground, "#123456ff");

        merged.reset_overrides(DEFAULT_PROFILE_NAME).unwrap();
        let reset = merged.profile(DEFAULT_PROFILE_NAME).unwrap();
        assert_eq!(reset.font_size, 12.0);
        assert_eq!(reset.foreground, "#00ff00ff");
        assert_eq!(
            merged.delete_profile(DEFAULT_PROFILE_NAME),
            Err(ProfileMutationError::Protected(DEFAULT_PROFILE_NAME.into()))
        );
    }

    #[test]
    fn profile_mutations_reject_duplicate_and_missing_names() {
        let mut store = ProfileStore::defaults();
        assert_eq!(
            store.duplicate_profile("missing", "copy"),
            Err(ProfileMutationError::Missing("missing".into()))
        );
        assert_eq!(
            store.duplicate_profile(DEFAULT_PROFILE_NAME, "Basic"),
            Err(ProfileMutationError::DuplicateName("Basic".into()))
        );
    }

    #[test]
    fn profile_defaults_cover_terminal_behavior_surface() {
        let profile = TerminalProfile::homebrew();
        assert_eq!(profile.ansi_palette.len(), 16);
        assert_eq!(profile.columns, 80);
        assert_eq!(profile.rows, 24);
        assert_eq!(profile.terminal_type, "xterm-256color");
        assert!(profile.utf8);
        assert_eq!(profile.ambiguous_width, 1);
        assert_eq!(profile.close_on_exit, CloseOnExit::Never);
        assert_eq!(profile.ask_before_close_policy, AskBeforeClosePolicy::Never);
        assert!(profile.run_inside_shell);
        assert!(profile.smooth_resize);
        assert_eq!(profile.key_mappings.len(), 20);
        assert_eq!(profile.key_mappings[0].action, r"\eOP");
    }

    #[test]
    fn custom_profiles_round_trip_through_project_document() {
        let mut store = ProfileStore::defaults();
        store
            .duplicate_profile(DEFAULT_PROFILE_NAME, "Custom")
            .unwrap();
        store.set_default("Custom").unwrap();
        let path = std::env::temp_dir().join(format!(
            "core-terminal-profiles-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        store.save_to_path(&path).unwrap();
        let restored = ProfileStore::load_from_path(&path).unwrap();
        assert_eq!(restored.selected_name(), "Custom");
        assert_eq!(restored.profile("Custom").unwrap().ansi_palette.len(), 16);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn scalar_terminal_plist_round_trips_and_clamps_untrusted_values() {
        let mut profile = TerminalProfile::homebrew();
        profile.name = "Imported & Safe".into();
        profile.font = "Monospace <12>".into();
        profile.background_alpha = 2.0;
        profile.columns = 10_000;
        profile.terminal_type = " xterm-direct ".into();
        let xml = export_terminal_plist(&profile).unwrap();
        let imported = import_terminal_plist(&xml).unwrap();
        assert_eq!(imported.name, profile.name);
        assert_eq!(imported.font, profile.font);
        assert_eq!(imported.background_alpha, 1.0);
        assert_eq!(imported.columns, 1_000);
        assert_eq!(imported.terminal_type, "xterm-direct");
    }

    #[test]
    fn archived_terminal_values_are_reported_as_field_fallbacks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <plist version="1.0"><dict>
              <key>name</key><string>Homebrew imported</string>
              <key>TextColor</key><data>AQID</data>
              <key>Font</key><data>AQID</data>
            </dict></plist>"#;
        let imported = import_terminal_plist_with_report(xml).unwrap();
        assert_eq!(imported.profile.name, "Homebrew imported");
        assert_eq!(
            imported.profile.foreground,
            TerminalProfile::homebrew().foreground
        );
        assert!(imported.fallbacks.iter().any(|key| key == "TextColor"));
        assert!(imported.fallbacks.iter().any(|key| key == "Font"));
    }

    #[test]
    fn nested_keyed_archive_color_is_decoded_without_execution() {
        let mut color_object = plist::Dictionary::new();
        color_object.insert(
            "NSRGB".into(),
            plist::Value::Data(b"0.15686275 0.99607849 0.078431375\0".to_vec()),
        );
        let mut archive = plist::Dictionary::new();
        archive.insert(
            "$objects".into(),
            plist::Value::Array(vec![
                plist::Value::String("$null".into()),
                plist::Value::Dictionary(color_object),
            ]),
        );
        let mut archive_bytes = Vec::new();
        plist::Value::Dictionary(archive)
            .to_writer_binary(&mut archive_bytes)
            .unwrap();
        let mut outer = plist::Dictionary::new();
        outer.insert("name".into(), "Nested Color".into());
        outer.insert("TextColor".into(), plist::Value::Data(archive_bytes));
        let mut xml = Vec::new();
        plist::Value::Dictionary(outer)
            .to_writer_xml(&mut xml)
            .unwrap();
        let imported = import_terminal_plist(std::str::from_utf8(&xml).unwrap()).unwrap();
        assert_eq!(imported.name, "Nested Color");
        assert_eq!(imported.foreground, "#28fe14ff");
    }

    #[test]
    fn nested_keyed_archive_font_is_decoded_with_size() {
        let mut font_object = plist::Dictionary::new();
        font_object.insert("NSName".into(), plist::Value::Uid(plist::Uid::new(2)));
        font_object.insert("NSSize".into(), 12.0.into());
        let mut archive = plist::Dictionary::new();
        archive.insert(
            "$objects".into(),
            plist::Value::Array(vec![
                plist::Value::String("$null".into()),
                plist::Value::Dictionary(font_object),
                plist::Value::String("AndaleMono".into()),
            ]),
        );
        let mut archive_bytes = Vec::new();
        plist::Value::Dictionary(archive)
            .to_writer_binary(&mut archive_bytes)
            .unwrap();
        let mut outer = plist::Dictionary::new();
        outer.insert("name".into(), "Nested Font".into());
        outer.insert("Font".into(), plist::Value::Data(archive_bytes));
        let mut xml = Vec::new();
        plist::Value::Dictionary(outer)
            .to_writer_xml(&mut xml)
            .unwrap();
        let imported = import_terminal_plist(std::str::from_utf8(&xml).unwrap()).unwrap();
        assert_eq!(imported.font, "AndaleMono");
        assert_eq!(imported.font_size, 12.0);
    }

    #[test]
    fn binary_outer_terminal_plist_is_accepted() {
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert("name".into(), "Binary Profile".into());
        dictionary.insert("CursorBlink".into(), false.into());
        dictionary.insert("CursorType".into(), 2_i64.into());
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dictionary)
            .to_writer_binary(&mut bytes)
            .unwrap();

        let imported = import_terminal_plist_bytes_with_report(&bytes).unwrap();
        assert_eq!(imported.profile.name, "Binary Profile");
        assert!(!imported.profile.cursor_blink);
        assert_eq!(imported.profile.cursor_shape, CursorShape::Underline);
    }

    #[test]
    #[ignore = "requires the owner-only private profile fixture directory"]
    fn private_reference_profiles_decode_when_explicitly_requested() {
        let fixture_dir = std::env::var_os("CORE_TERMINAL_PRIVATE_FIXTURE_DIR")
            .expect("set CORE_TERMINAL_PRIVATE_FIXTURE_DIR for this local-only test");
        let entries = fs::read_dir(fixture_dir).expect("private fixture directory is readable");
        let mut decoded_names = Vec::new();
        for entry in entries {
            let entry = entry.expect("private fixture directory entry is readable");
            if entry.path().extension().and_then(|value| value.to_str()) != Some("terminal") {
                continue;
            }
            let bytes = fs::read(entry.path()).expect("private fixture file is readable");
            let imported = import_terminal_plist_bytes_with_report(&bytes)
                .expect("private fixture decodes as a supported plist");
            assert!(
                imported.fallbacks.is_empty(),
                "profile import left unsupported fields: {:?}",
                imported.fallbacks
            );
            decoded_names.push(imported.profile.name);
        }
        decoded_names.sort();
        let mut expected = BUILTIN_PROFILE_NAMES.map(str::to_owned).to_vec();
        expected.sort();
        assert_eq!(decoded_names, expected);
    }

    #[test]
    fn plist_import_rejects_unsafe_or_non_dictionary_roots() {
        assert!(matches!(
            import_terminal_plist("<!DOCTYPE plist><plist/>").unwrap_err(),
            ProfilePlistError::Unsafe
        ));
        assert!(matches!(
            import_terminal_plist("<plist version=\"1.0\"><string>nope</string></plist>")
                .unwrap_err(),
            ProfilePlistError::InvalidValue(_)
        ));
        let oversized = format!("<plist>{}</plist>", "x".repeat(4 * 1024 * 1024));
        assert!(matches!(
            import_terminal_plist(&oversized).unwrap_err(),
            ProfilePlistError::TooLarge
        ));
    }

    #[test]
    fn window_groups_round_trip_and_have_crud_validation() {
        let mut store = ProfileStore::defaults();
        let group = WindowGroup {
            name: "  Development  ".into(),
            entries: vec![WindowGroupEntry {
                profile: DEFAULT_PROFILE_NAME.into(),
                working_directory: Some(" /tmp/project ".into()),
                columns: 120,
                rows: 40,
            }],
        };
        store.add_window_group(group).unwrap();
        assert_eq!(
            store.window_group("Development").unwrap().entries[0].columns,
            120
        );
        assert_eq!(
            store.window_group("Development").unwrap().entries[0]
                .working_directory
                .as_deref(),
            Some("/tmp/project")
        );
        assert_eq!(
            store.add_window_group(WindowGroup {
                name: "Development".into(),
                entries: vec![WindowGroupEntry {
                    profile: DEFAULT_PROFILE_NAME.into(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
            }),
            Err(ProfileMutationError::DuplicateWindowGroup(
                "Development".into()
            ))
        );
        assert_eq!(
            store.add_window_group(WindowGroup {
                name: "bad".into(),
                entries: vec![WindowGroupEntry {
                    profile: "missing".into(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
            }),
            Err(ProfileMutationError::MissingWindowGroupProfile(
                "missing".into()
            ))
        );
        assert_eq!(
            store.add_window_group(WindowGroup {
                name: "bad-size".into(),
                entries: vec![WindowGroupEntry {
                    profile: DEFAULT_PROFILE_NAME.into(),
                    working_directory: None,
                    columns: 0,
                    rows: 24,
                }],
            }),
            Err(ProfileMutationError::InvalidWindowGroupDimensions)
        );
        assert_eq!(
            store.add_window_group(WindowGroup {
                name: "bad-directory".into(),
                entries: vec![WindowGroupEntry {
                    profile: DEFAULT_PROFILE_NAME.into(),
                    working_directory: Some("relative/path".into()),
                    columns: 80,
                    rows: 24,
                }],
            }),
            Err(ProfileMutationError::InvalidWindowGroupDirectory)
        );
        for directory in [
            "relative/path".to_owned(),
            "/tmp/with\ncontrol".to_owned(),
            format!("/{}", "x".repeat(4096)),
        ] {
            assert_eq!(
                store.add_window_group(WindowGroup {
                    name: "invalid-directory".into(),
                    entries: vec![WindowGroupEntry {
                        profile: DEFAULT_PROFILE_NAME.into(),
                        working_directory: Some(directory),
                        columns: 80,
                        rows: 24,
                    }],
                }),
                Err(ProfileMutationError::InvalidWindowGroupDirectory)
            );
        }
        store
            .add_window_group(WindowGroup {
                name: "Whitespace directory".into(),
                entries: vec![WindowGroupEntry {
                    profile: DEFAULT_PROFILE_NAME.into(),
                    working_directory: Some("   ".into()),
                    columns: 80,
                    rows: 24,
                }],
            })
            .unwrap();
        assert_eq!(
            store.window_group("Whitespace directory").unwrap().entries[0].working_directory,
            None
        );
        store
            .rename_window_group(
                "Whitespace directory",
                WindowGroup {
                    name: "Renamed group".into(),
                    entries: vec![WindowGroupEntry {
                        profile: DEFAULT_PROFILE_NAME.into(),
                        working_directory: Some(format!("/{}", "x".repeat(4095))),
                        columns: 81,
                        rows: 25,
                    }],
                },
            )
            .unwrap();
        assert!(store.window_group("Whitespace directory").is_none());
        assert_eq!(
            store.window_group("Renamed group").unwrap().entries[0].columns,
            81
        );
        assert_eq!(
            store.rename_window_group(
                "Renamed group",
                WindowGroup {
                    name: "Development".into(),
                    entries: vec![WindowGroupEntry {
                        profile: DEFAULT_PROFILE_NAME.into(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    }],
                }
            ),
            Err(ProfileMutationError::DuplicateWindowGroup(
                "Development".into()
            ))
        );
        assert!(store.window_group("Renamed group").is_some());
        let path = std::env::temp_dir().join(format!(
            "core-terminal-window-groups-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        store.save_to_path(&path).unwrap();
        let restored = ProfileStore::load_from_path(&path).unwrap();
        assert_eq!(
            restored.window_group("Development").unwrap().name,
            "Development"
        );
        let _ = fs::remove_file(path);
        assert_eq!(
            store.delete_window_group("Development").unwrap().name,
            "Development"
        );
        assert_eq!(
            store.delete_window_group("Renamed group").unwrap().name,
            "Renamed group"
        );
        assert!(store.window_groups().is_empty());
    }

    #[test]
    fn profiles_referenced_by_window_groups_cannot_be_deleted() {
        let mut store = ProfileStore::defaults();
        let mut profile = TerminalProfile::homebrew();
        profile.name = "Project".into();
        store.add_profile(profile).unwrap();
        store
            .add_window_group(WindowGroup {
                name: "Development".into(),
                entries: vec![WindowGroupEntry {
                    profile: "Project".into(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
            })
            .unwrap();

        assert_eq!(
            store.delete_profile("Project"),
            Err(ProfileMutationError::ProfileUsedByWindowGroup {
                profile: "Project".into(),
                group: "Development".into(),
            })
        );
        assert!(store.profile("Project").is_some());
    }
}
