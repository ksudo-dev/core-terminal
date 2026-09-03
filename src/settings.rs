//! Small, human-readable settings persistence for Terminal.
//!
//! Settings intentionally live outside the profile document. Built-in profile
//! baselines live in project data, while this file stores the user's selected
//! profile and window preferences under the normal XDG config path.

use crate::profiles::{CursorShape, TerminalProfile, DEFAULT_PROFILE_NAME};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const APP_CONFIG_DIR: &str = "core-terminal";
pub const SETTINGS_FILENAME: &str = "settings.json";
pub const CURRENT_SCHEMA_VERSION: u32 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Version of the on-disk settings schema. Missing values are legacy v1.
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default = "default_selected_profile")]
    pub selected_profile: String,
    #[serde(default = "default_selected_profile")]
    pub startup_profile: String,
    /// Empty starts one profile window; otherwise names a saved window group.
    #[serde(default)]
    pub startup_window_group: String,
    /// Profile selection policy for newly-created windows and tabs.
    #[serde(default = "default_new_window_profile")]
    pub new_window_profile: String,
    #[serde(default = "default_new_tab_profile")]
    pub new_tab_profile: String,
    #[serde(default)]
    pub new_window_same_directory: bool,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    #[serde(default)]
    pub cursor_shape: CursorShape,
    #[serde(default = "default_true")]
    pub cursor_blink: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: u32,
    #[serde(default = "default_window_width")]
    pub window_width: i32,
    #[serde(default = "default_window_height")]
    pub window_height: i32,
    /// Login shell executable override. Empty uses `$SHELL` with `/bin/sh`
    /// fallback; this field is never interpreted as a command line.
    #[serde(default)]
    pub shell: String,
    #[serde(default)]
    pub use_custom_command: bool,
    #[serde(default)]
    pub custom_command: String,
    #[serde(default = "default_true")]
    pub run_command_inside_shell: bool,
    #[serde(default = "default_true")]
    pub new_tab_same_directory: bool,
    #[serde(default = "default_true")]
    pub ctrl_number_tabs: bool,
    #[serde(default = "default_true")]
    pub scroll_on_output: bool,
    #[serde(default = "default_true")]
    pub scroll_on_input: bool,
    #[serde(default)]
    pub audible_bell: bool,
    #[serde(default = "default_true")]
    pub bold_is_bright: bool,
    #[serde(default)]
    pub mouse_autohide: bool,
    #[serde(default)]
    pub background_notifications: bool,
    #[serde(default = "default_terminal_type")]
    pub terminal_type: String,
    #[serde(default)]
    pub locale: String,
    #[serde(default)]
    pub set_locale_environment: bool,
}

fn default_selected_profile() -> String {
    DEFAULT_PROFILE_NAME.into()
}

fn default_font() -> String {
    "Monospace".into()
}

fn default_font_size() -> f64 {
    12.0
}

fn default_new_window_profile() -> String {
    "default".into()
}

fn default_new_tab_profile() -> String {
    "same".into()
}

fn default_true() -> bool {
    true
}

fn default_scrollback() -> u32 {
    10_000
}

fn default_window_width() -> i32 {
    1_120
}

fn default_window_height() -> i32 {
    720
}

fn default_terminal_type() -> String {
    "xterm-256color".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            selected_profile: default_selected_profile(),
            startup_profile: default_selected_profile(),
            startup_window_group: String::new(),
            new_window_profile: default_new_window_profile(),
            new_tab_profile: default_new_tab_profile(),
            new_window_same_directory: false,
            font: default_font(),
            font_size: default_font_size(),
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            scrollback_lines: default_scrollback(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            shell: String::new(),
            use_custom_command: false,
            custom_command: String::new(),
            run_command_inside_shell: true,
            new_tab_same_directory: true,
            ctrl_number_tabs: true,
            scroll_on_output: true,
            scroll_on_input: true,
            audible_bell: false,
            bold_is_bright: true,
            mouse_autohide: false,
            background_notifications: false,
            terminal_type: default_terminal_type(),
            locale: String::new(),
            set_locale_environment: false,
        }
    }
}

impl Settings {
    pub fn from_profile(profile: &TerminalProfile) -> Self {
        Self {
            selected_profile: profile.name.clone(),
            startup_profile: profile.name.clone(),
            font: profile.font.clone(),
            font_size: profile.font_size,
            cursor_shape: profile.cursor_shape,
            cursor_blink: profile.cursor_blink,
            scrollback_lines: profile.scrollback_lines,
            ..Self::default()
        }
    }

    pub fn normalize(mut self) -> Self {
        if self.schema_version < CURRENT_SCHEMA_VERSION {
            if self.startup_profile == default_selected_profile()
                && self.selected_profile != default_selected_profile()
            {
                // v1-v4 had one selected profile field; preserve it as the
                // startup profile when introducing the explicit split.
                self.startup_profile = self.selected_profile.clone();
            }
            // Pre-v4 settings were written before the global/profile split. Never
            // preserve their implicit pointer-hiding behavior on upgrade.
            self.schema_version = CURRENT_SCHEMA_VERSION;
        }
        // Mouse hiding is intentionally not an active feature. Keep the field
        // only as a migration-compatible serialization placeholder.
        self.mouse_autohide = false;
        if self.selected_profile.trim().is_empty() {
            self.selected_profile = DEFAULT_PROFILE_NAME.into();
        }
        if self.startup_profile.trim().is_empty() {
            self.startup_profile = self.selected_profile.clone();
        }
        self.startup_window_group = self.startup_window_group.trim().to_owned();
        if self.startup_window_group.len() > 256
            || self.startup_window_group.chars().any(char::is_control)
        {
            self.startup_window_group.clear();
        }
        if self.font.trim().is_empty() {
            self.font = default_font();
        }
        if !self.font_size.is_finite() || self.font_size <= 0.0 {
            self.font_size = 12.0;
        }
        self.font_size = self.font_size.clamp(6.0, 96.0);
        self.scrollback_lines = self.scrollback_lines.clamp(100, 1_000_000);
        self.window_width = self.window_width.clamp(320, 8_000);
        self.window_height = self.window_height.clamp(240, 8_000);
        self.custom_command = self.custom_command.trim().to_owned();
        if self.custom_command.is_empty() {
            self.use_custom_command = false;
        }
        self.shell = self.shell.trim().to_owned();
        if !self.shell.is_empty()
            && (!self.shell.starts_with('/')
                || self
                    .shell
                    .chars()
                    .any(|character| character.is_whitespace()))
        {
            self.shell.clear();
        }
        self.terminal_type = self.terminal_type.trim().to_owned();
        if self.terminal_type.is_empty()
            || !self
                .terminal_type
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_+.-".contains(character))
        {
            self.terminal_type = default_terminal_type();
        }
        self.locale = self.locale.trim().to_owned();
        if !self.locale.is_empty()
            && !self
                .locale
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_.@+-".contains(character))
        {
            self.locale.clear();
            self.set_locale_environment = false;
        }
        self.new_window_profile = normalize_session_profile(&self.new_window_profile, "default");
        self.new_tab_profile = normalize_session_profile(&self.new_tab_profile, "same");
        self
    }

    pub fn config_path() -> Option<PathBuf> {
        if let Ok(path) = env::var("XDG_CONFIG_HOME") {
            if !path.trim().is_empty() && Path::new(&path).is_absolute() {
                return Some(
                    PathBuf::from(path)
                        .join(APP_CONFIG_DIR)
                        .join(SETTINGS_FILENAME),
                );
            }
        }
        env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join(APP_CONFIG_DIR)
                .join(SETTINGS_FILENAME)
        })
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref();
        if fs::metadata(path)?.len() > 4 * 1024 * 1024 {
            return Err(SettingsError::TooLarge);
        }
        let content = fs::read_to_string(path)?;
        #[cfg(unix)]
        {
            if fs::symlink_metadata(path)
                .map(|metadata| !metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            }
        }
        Ok(serde_json::from_str::<Self>(&content)?.normalize())
    }

    pub fn load_or_default(path: impl AsRef<Path>) -> Self {
        Self::load(path).unwrap_or_default()
    }

    pub fn load_user() -> Self {
        Self::config_path()
            .map(Self::load_or_default)
            .unwrap_or_default()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self.clone().normalize())? + "\n";
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let write_result = (|| -> Result<(), SettingsError> {
            let mut file = options.open(&temporary)?;
            file.write_all(content.as_bytes())?;
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
        write_result?;
        Ok(())
    }

    pub fn save_user(&self) -> Result<(), SettingsError> {
        Self::config_path()
            .ok_or(SettingsError::ConfigPathUnavailable)
            .and_then(|path| self.save(path))
    }
}

fn normalize_session_profile(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.into()
    } else {
        value.to_owned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read settings: {0}")]
    Io(#[from] io::Error),
    #[error("invalid settings JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("a user config directory is not available")]
    ConfigPathUnavailable,
    #[error("settings file is larger than the safe limit")]
    TooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_start_with_homebrew() {
        assert_eq!(Settings::default().selected_profile, DEFAULT_PROFILE_NAME);
        assert_eq!(Settings::default().schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!Settings::default().mouse_autohide);
    }

    #[test]
    fn settings_round_trip_and_normalize() {
        let path = std::env::temp_dir().join(format!(
            "core-terminal-settings-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let settings = Settings {
            selected_profile: String::new(),
            font: String::new(),
            font_size: -1.0,
            scrollback_lines: 0,
            window_width: 1,
            window_height: 1,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.selected_profile, DEFAULT_PROFILE_NAME);
        assert_eq!(loaded.font, "Monospace");
        assert_eq!(loaded.font_size, 12.0);
        assert_eq!(loaded.scrollback_lines, 100);
        assert_eq!(loaded.window_width, 320);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn profile_values_can_seed_settings() {
        let profile = TerminalProfile::homebrew();
        let settings = Settings::from_profile(&profile);
        assert_eq!(settings.selected_profile, DEFAULT_PROFILE_NAME);
        assert_eq!(settings.font_size, 12.0);
        assert!(settings.cursor_blink);
    }

    #[test]
    fn selected_profile_persists_across_restart() {
        let path = std::env::temp_dir().join(format!(
            "core-terminal-profile-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let settings = Settings {
            selected_profile: "Ocean".into(),
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().selected_profile, "Ocean");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn old_settings_json_deserializes_with_new_defaults() {
        let old = r#"{
            "selected_profile":"Ocean",
            "font":"Monospace",
            "font_size":11.0,
            "cursor_shape":"block",
            "cursor_blink":true,
            "scrollback_lines":10000,
            "window_width":900,
            "window_height":600
        }"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.terminal_type, "xterm-256color");
        assert!(!settings.use_custom_command);
        assert!(settings.new_tab_same_directory);
        assert!(settings.ctrl_number_tabs);
        assert_eq!(settings.schema_version, 0);
        let migrated = settings.normalize();
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!migrated.mouse_autohide);
        assert_eq!(migrated.startup_profile, "Ocean");
    }

    #[test]
    fn old_settings_with_implicit_mouse_hiding_are_migrated_safely() {
        let old = r#"{
            "selected_profile":"Homebrew",
            "font":"Monospace",
            "font_size":12.0,
            "cursor_shape":"block",
            "cursor_blink":true,
            "scrollback_lines":10000,
            "window_width":1120,
            "window_height":720,
            "mouse_autohide":true
        }"#;
        let migrated = serde_json::from_str::<Settings>(old).unwrap().normalize();
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!migrated.mouse_autohide);
    }

    #[test]
    fn schema_v2_is_upgraded_and_mouse_hiding_stays_disabled() {
        let settings = Settings {
            schema_version: CURRENT_SCHEMA_VERSION,
            mouse_autohide: true,
            ..Settings::default()
        }
        .normalize();
        assert!(!settings.mouse_autohide);
        let serialized = serde_json::to_string(&settings).unwrap();
        let restored = serde_json::from_str::<Settings>(&serialized)
            .unwrap()
            .normalize();
        assert!(!restored.mouse_autohide);
        assert_eq!(restored.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn command_and_terminal_values_are_trimmed_and_safely_normalized() {
        let settings = Settings {
            use_custom_command: true,
            custom_command: "  tmux new-session  ".into(),
            terminal_type: "  xterm-direct  ".into(),
            ..Settings::default()
        }
        .normalize();
        assert_eq!(settings.custom_command, "tmux new-session");
        assert_eq!(settings.terminal_type, "xterm-direct");

        let invalid = Settings {
            use_custom_command: true,
            custom_command: "  ".into(),
            terminal_type: "xterm;rm -rf /".into(),
            ..Settings::default()
        }
        .normalize();
        assert!(!invalid.use_custom_command);
        assert_eq!(invalid.terminal_type, "xterm-256color");
    }

    #[test]
    fn schema_v1_through_v4_settings_upgrade_to_v5_without_mouse_hiding() {
        for version in [0, 1, 2, 3, 4] {
            let settings = Settings {
                schema_version: version,
                mouse_autohide: true,
                ..Settings::default()
            }
            .normalize();
            assert_eq!(settings.schema_version, CURRENT_SCHEMA_VERSION);
            assert!(!settings.mouse_autohide);
        }
        let old = r#"{"selected_profile":"Homebrew"}"#;
        let settings = serde_json::from_str::<Settings>(old).unwrap();
        assert!(!settings.mouse_autohide);
        assert_eq!(settings.schema_version, 0);
    }

    #[test]
    fn shell_executable_is_a_path_only_and_custom_command_stays_text() {
        let settings = Settings {
            shell: " /bin/bash ".into(),
            custom_command: "  printf 'hello; not parsed'  ".into(),
            use_custom_command: true,
            ..Settings::default()
        }
        .normalize();
        assert_eq!(settings.shell, "/bin/bash");
        assert_eq!(settings.custom_command, "printf 'hello; not parsed'");
        let untrusted = Settings {
            shell: "/bin/bash -c evil".into(),
            ..Settings::default()
        }
        .normalize();
        assert!(untrusted.shell.is_empty());
    }
}
