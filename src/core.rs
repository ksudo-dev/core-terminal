//! Terminal process and tab/session lifecycle primitives.

#![allow(dead_code)]

use crate::{
    profiles::{AskBeforeClosePolicy, ShellExitAction, TerminalProfile},
    settings::Settings,
};
use gtk::{gio, glib};
use std::env;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for SessionId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalTab {
    pub id: SessionId,
    pub title: String,
    pub profile_name: String,
    pub working_directory: Option<String>,
    pub child_pid: Option<glib::Pid>,
}

impl TerminalTab {
    fn new(id: SessionId, profile_name: &str, working_directory: Option<&str>) -> Self {
        Self {
            id,
            title: "Terminal".into(),
            profile_name: profile_name.into(),
            working_directory: working_directory.map(str::to_owned),
            child_pid: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionManager {
    tabs: Vec<TerminalTab>,
    active: usize,
    next_id: u64,
}

impl SessionManager {
    pub fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
        }
    }

    pub fn tabs(&self) -> &[TerminalTab] {
        &self.tabs
    }

    pub fn tab(&self, id: SessionId) -> Option<&TerminalTab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn active(&self) -> Option<&TerminalTab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn open_tab(&mut self, profile_name: &str, working_directory: Option<&str>) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.tabs
            .push(TerminalTab::new(id, profile_name, working_directory));
        self.active = self.tabs.len() - 1;
        id
    }

    pub fn close_tab(&mut self, id: SessionId) -> Option<TerminalTab> {
        self.tabs
            .iter()
            .position(|tab| tab.id == id)
            .and_then(|index| self.close_at(index))
    }

    fn close_at(&mut self, index: usize) -> Option<TerminalTab> {
        if index >= self.tabs.len() {
            return None;
        }
        let removed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active > index {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        Some(removed)
    }

    pub fn next_tab(&mut self) -> Option<&TerminalTab> {
        if self.tabs.len() > 1 {
            self.active = (self.active + 1) % self.tabs.len();
        }
        self.active()
    }

    pub fn previous_tab(&mut self) -> Option<&TerminalTab> {
        if self.tabs.len() > 1 {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
        self.active()
    }

    pub fn select_tab(&mut self, index: usize) -> Option<&TerminalTab> {
        if index < self.tabs.len() {
            self.active = index;
        }
        self.active()
    }

    pub fn set_title(&mut self, id: SessionId, title: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            let title = title.into();
            if !title.trim().is_empty() {
                tab.title = title;
            }
        }
    }

    pub fn set_child_pid(&mut self, id: SessionId, child_pid: Option<glib::Pid>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.child_pid = child_pid;
        }
    }

    pub fn clear_child_pid(&mut self, id: SessionId) {
        self.set_child_pid(id, None);
    }

    pub fn set_working_directory(&mut self, id: SessionId, working_directory: Option<&str>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.working_directory = working_directory.map(str::to_owned);
        }
    }
}

/// Return the user's login shell, with a safe fallback for stripped-down
/// environments and tests.
pub fn login_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty() && shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Build argv for a login shell.  Keeping this separate makes shell spawning
/// deterministic and easy to test without creating a PTY.
#[allow(dead_code)]
pub fn login_shell_command(shell: impl Into<String>) -> Vec<String> {
    let shell = shell.into();
    login_shell_argv(&shell, None)
}

/// Build argv for the user's login shell, optionally asking it to execute a
/// configured command. `-lc` keeps the custom command in the same login-shell
/// environment as a normal terminal session.
pub fn login_shell_argv(shell: &str, custom_command: Option<&str>) -> Vec<String> {
    match custom_command
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        Some(command) => vec![shell.to_owned(), "-lc".into(), command.to_owned()],
        None => vec![shell.to_owned(), "-l".into()],
    }
}

pub fn startup_argv(
    shell: &str,
    custom_command: Option<&str>,
    run_inside_shell: bool,
) -> Vec<String> {
    let Some(command) = custom_command
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return login_shell_argv(shell, None);
    };
    if run_inside_shell {
        return login_shell_argv(shell, Some(command));
    }
    // Direct startup is parsed as argv, not passed through a shell. GLib's
    // parser preserves quoted arguments and backslash escapes while not
    // performing expansion, command substitution, or redirection. Invalid
    // or unreasonably large imported commands fail closed to a normal login
    // shell rather than being partially interpreted.
    parse_direct_command(command).unwrap_or_else(|| login_shell_argv(shell, None))
}

const MAX_DIRECT_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_DIRECT_ARGUMENTS: usize = 4096;

fn parse_direct_command(command: &str) -> Option<Vec<String>> {
    if command.len() > MAX_DIRECT_COMMAND_BYTES || command.contains('\0') {
        return None;
    }
    let arguments = glib::shell_parse_argv(command)
        .ok()?
        .into_iter()
        .map(|argument| argument.into_string().ok())
        .collect::<Option<Vec<_>>>()?;
    (!arguments.is_empty() && arguments.len() <= MAX_DIRECT_ARGUMENTS).then_some(arguments)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnOptions {
    pub working_directory: Option<String>,
    pub custom_command: Option<String>,
    pub terminal_type: String,
    pub shell: Option<String>,
    pub run_command_inside_shell: bool,
    pub locale: Option<String>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            working_directory: None,
            custom_command: None,
            terminal_type: "xterm-256color".into(),
            shell: None,
            run_command_inside_shell: true,
            locale: None,
        }
    }
}

impl SpawnOptions {
    pub fn new(
        working_directory: Option<&str>,
        custom_command: Option<&str>,
        terminal_type: &str,
    ) -> Self {
        Self {
            working_directory: working_directory.map(str::to_owned),
            custom_command: custom_command
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .map(str::to_owned),
            terminal_type: sanitize_terminal_type(terminal_type),
            shell: None,
            run_command_inside_shell: true,
            locale: None,
        }
    }

    #[allow(dead_code)]
    pub fn from_settings(settings: &Settings, working_directory: Option<&str>) -> Self {
        Self::from_settings_with_terminal_type(settings, working_directory, &settings.terminal_type)
    }

    pub fn from_settings_with_terminal_type(
        settings: &Settings,
        working_directory: Option<&str>,
        terminal_type: &str,
    ) -> Self {
        Self::new(
            working_directory,
            settings
                .use_custom_command
                .then_some(settings.custom_command.as_str()),
            terminal_type,
        )
        .with_shell(&settings.shell, settings.run_command_inside_shell)
        .with_locale(
            settings
                .set_locale_environment
                .then_some(settings.locale.as_str()),
        )
    }

    pub fn from_profile(profile: &TerminalProfile, working_directory: Option<&str>) -> Self {
        Self::new(
            working_directory,
            (!profile.shell_command.trim().is_empty()).then_some(profile.shell_command.as_str()),
            &profile.terminal_type,
        )
        .with_shell(&profile.shell, profile.run_inside_shell)
        .with_locale(
            profile
                .set_locale_environment
                .then_some(profile.locale.as_str()),
        )
    }

    pub fn with_shell(mut self, shell: &str, run_command_inside_shell: bool) -> Self {
        self.shell = sanitize_shell_path(shell);
        self.run_command_inside_shell = run_command_inside_shell;
        self
    }

    pub fn with_locale(mut self, locale: Option<&str>) -> Self {
        self.locale = locale
            .map(str::trim)
            .filter(|locale| {
                !locale.is_empty()
                    && locale.chars().all(|character| {
                        character.is_ascii_alphanumeric() || "_.@+-".contains(character)
                    })
            })
            .map(str::to_owned);
        self
    }
}

/// Spawn a login shell in a VTE terminal. VTE owns the PTY and child watch;
/// callers should retain the returned PID from the callback and remove the
/// corresponding tab when the child-exited signal arrives.
/// Spawn a VTE child using explicit command, TERM, and working-directory
/// options. VTE owns the PTY and child watch.
pub fn spawn_with_settings<F>(
    terminal: &vte4::Terminal,
    settings: &Settings,
    working_directory: Option<&str>,
    callback: F,
) where
    F: FnOnce(Result<glib::Pid, glib::Error>) + 'static,
{
    let options = SpawnOptions::from_settings(settings, working_directory);
    spawn_terminal(terminal, &options, callback);
}

pub fn spawn_with_profile<F>(
    terminal: &vte4::Terminal,
    profile: &TerminalProfile,
    working_directory: Option<&str>,
    callback: F,
) where
    F: FnOnce(Result<glib::Pid, glib::Error>) + 'static,
{
    let options = SpawnOptions::from_profile(profile, working_directory);
    spawn_terminal(terminal, &options, callback);
}

/// Spawn while retaining ownership of the session lookup needed for the
/// close-before-spawn race. If a tab disappeared while VTE was setting up the
/// PTY, the returned child is terminated immediately and the callback is
/// intentionally not invoked.
pub fn spawn_terminal_for_tab<F>(
    terminal: &vte4::Terminal,
    options: &SpawnOptions,
    sessions: std::rc::Rc<std::cell::RefCell<SessionManager>>,
    id: SessionId,
    callback: F,
) where
    F: FnOnce(Result<glib::Pid, glib::Error>) + 'static,
{
    let sessions_callback = sessions.clone();
    spawn_terminal(terminal, options, move |result| {
        if let Ok(pid) = result {
            let Ok(mut sessions) = sessions_callback.try_borrow_mut() else {
                terminate_child(pid);
                return;
            };
            if sessions.tab(id).is_none() {
                terminate_child(pid);
                return;
            }
            sessions.set_child_pid(id, Some(pid));
            callback(Ok(pid));
        } else {
            callback(result);
        }
    });
}

pub fn spawn_terminal<F>(terminal: &vte4::Terminal, options: &SpawnOptions, callback: F)
where
    F: FnOnce(Result<glib::Pid, glib::Error>) + 'static,
{
    let shell = options.shell.clone().unwrap_or_else(login_shell);
    let command = startup_argv(
        &shell,
        options.custom_command.as_deref(),
        options.run_command_inside_shell,
    );
    let envv_owned = child_environment(&options.terminal_type, options.locale.as_deref());
    let (command, working_directory) = if running_in_flatpak() {
        (
            flatpak_host_argv(&command, options.working_directory.as_deref(), &envv_owned),
            None,
        )
    } else {
        (command, options.working_directory.clone())
    };
    let argv: Vec<&str> = command.iter().map(String::as_str).collect();
    let envv: Vec<&str> = envv_owned.iter().map(String::as_str).collect();
    use vte4::prelude::TerminalExtManual;
    terminal.spawn_async(
        vte4::PtyFlags::DEFAULT,
        working_directory.as_deref(),
        &argv,
        &envv,
        glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        None::<&gio::Cancellable>,
        callback,
    );
}

fn running_in_flatpak() -> bool {
    env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").is_file()
}

fn flatpak_host_argv(
    command: &[String],
    working_directory: Option<&str>,
    environment: &[String],
) -> Vec<String> {
    let mut argv = vec![
        "flatpak-spawn".into(),
        "--host".into(),
        "--clear-env".into(),
        "--watch-bus".into(),
    ];
    let host_home = environment
        .iter()
        .find_map(|entry| entry.strip_prefix("HOME="))
        .filter(|value| value.starts_with('/') && !value.contains('\0'));
    let directory = working_directory
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .or(host_home)
        .unwrap_or("/");
    argv.push(format!("--directory={directory}"));

    let host_path = host_home
        .map(|home| {
            format!(
                "PATH={home}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            )
        })
        .unwrap_or_else(|| {
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
        });
    argv.push(format!("--env={host_path}"));

    const HOST_ENVIRONMENT: [&str; 15] = [
        "COLORTERM",
        "DBUS_SESSION_BUS_ADDRESS",
        "DISPLAY",
        "HOME",
        "LANG",
        "LC_ALL",
        "LOGNAME",
        "SHELL",
        "SSH_AUTH_SOCK",
        "TERM",
        "TZ",
        "USER",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
    ];
    for entry in environment {
        let Some((name, _)) = entry.split_once('=') else {
            continue;
        };
        if HOST_ENVIRONMENT.contains(&name) || name.starts_with("LC_") {
            argv.push(format!("--env={entry}"));
        }
    }
    argv.push("--".into());
    argv.extend(command.iter().cloned());
    argv
}

/// Apply the profile's requested terminal grid size. VTE measures this in
/// character cells; the window manager may still constrain the resulting
/// widget, so this is intentionally separate from pixel window sizing.
#[allow(dead_code)]
pub fn apply_terminal_size(terminal: &vte4::Terminal, profile: &TerminalProfile) {
    use vte4::prelude::TerminalExt;
    terminal.set_size(profile.columns as i64, profile.rows as i64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildExitDecision {
    CloseWindow,
    CloseTab,
    Keep,
    Ask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// Decode the wait status supplied by VTE's `child-exited` signal. VTE uses
/// the platform wait status rather than just the shell's 0..255 exit code.
pub fn decode_child_exit_status(status: i32) -> ChildExitStatus {
    #[cfg(unix)]
    {
        if libc::WIFEXITED(status) {
            return ChildExitStatus {
                code: Some(libc::WEXITSTATUS(status)),
                signal: None,
            };
        }
        if libc::WIFSIGNALED(status) {
            return ChildExitStatus {
                code: None,
                signal: Some(libc::WTERMSIG(status)),
            };
        }
    }
    ChildExitStatus {
        code: Some(status),
        signal: None,
    }
}

/// Resolve a VTE child exit into a UI action. Explicit clean/error policies
/// take precedence over the general shell action, allowing a profile to keep
/// failed commands visible while closing a clean shell (or vice versa).
pub fn child_exit_decision(profile: &TerminalProfile, status: i32) -> ChildExitDecision {
    let clean = decode_child_exit_status(status).code == Some(0);
    match profile.close_on_exit {
        crate::profiles::CloseOnExit::Clean if clean => {
            return ChildExitDecision::CloseTab;
        }
        crate::profiles::CloseOnExit::Always => return ChildExitDecision::CloseTab,
        _ => {}
    }
    if clean && profile.close_on_clean_exit {
        return ChildExitDecision::CloseTab;
    }
    if !clean && profile.close_on_error {
        return ChildExitDecision::CloseTab;
    }
    match profile.shell_exit_action {
        ShellExitAction::CloseWindow => ChildExitDecision::CloseWindow,
        ShellExitAction::CloseTab => ChildExitDecision::CloseTab,
        ShellExitAction::Keep => ChildExitDecision::Keep,
        ShellExitAction::Ask => ChildExitDecision::Ask,
    }
}

pub fn should_prompt_before_close(profile: &TerminalProfile, process: Option<&str>) -> bool {
    // Keep the legacy boolean meaningful for profiles written before the
    // explicit policy enum was introduced.
    if profile.ask_before_close && profile.ask_before_close_policy == AskBeforeClosePolicy::Never {
        return true;
    }
    match profile.ask_before_close_policy {
        AskBeforeClosePolicy::Always => true,
        AskBeforeClosePolicy::Never => false,
        AskBeforeClosePolicy::NonExempt => {
            let Some(process) = process.map(str::trim).filter(|process| !process.is_empty()) else {
                return true;
            };
            !profile
                .ask_before_close_exceptions
                .iter()
                .any(|exception| exception == process)
        }
    }
}

fn child_environment(terminal_type: &str, locale: Option<&str>) -> Vec<String> {
    let mut environment: Vec<String> = env::vars()
        .filter(|(name, _)| {
            name != "TERM" && locale.is_none_or(|_| name != "LANG" && name != "LC_ALL")
        })
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    environment.push(format!("TERM={}", sanitize_terminal_type(terminal_type)));
    if let Some(locale) = locale {
        environment.push(format!("LANG={locale}"));
    }
    environment
}

pub fn sanitize_terminal_type(value: &str) -> String {
    let value = value.trim();
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_+.-".contains(character))
    {
        value.to_owned()
    } else {
        "xterm-256color".into()
    }
}

fn sanitize_shell_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('/') && !value.chars().any(char::is_whitespace) && !value.contains('\0') {
        Some(value.to_owned())
    } else {
        None
    }
}

/// Write one control byte to the child process through VTE's PTY.
pub fn send_control_character(terminal: &vte4::Terminal, character: u8) {
    use vte4::prelude::TerminalExt;
    terminal.feed_child(&[character]);
}

pub fn send_control_c(terminal: &vte4::Terminal) {
    send_control_character(terminal, crate::shortcuts::CONTROL_C);
}

pub fn send_control_v(terminal: &vte4::Terminal) {
    send_control_character(terminal, crate::shortcuts::CONTROL_V);
}

/// Ask a session's process group to exit when its tab is closed. VTE's PTY
/// child is the process-group leader, so signalling the group also cleans up a
/// foreground command instead of leaving it orphaned after a tab closes.
pub fn terminate_child(child_pid: glib::Pid) {
    // SAFETY: kill only receives a process id supplied by VTE for this app's
    // own child. A failed signal (for an already exited child) is harmless.
    unsafe {
        let pid = child_pid.0;
        if pid <= 1 {
            return;
        }
        let _ = libc::kill(-pid, libc::SIGHUP);
        let _ = libc::kill(pid, libc::SIGTERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{CursorShape, DEFAULT_PROFILE_NAME};

    #[test]
    fn login_command_uses_login_flag() {
        assert_eq!(login_shell_command("/bin/bash"), ["/bin/bash", "-l"]);
    }

    #[test]
    fn custom_command_uses_login_shell_c_mode() {
        assert_eq!(
            login_shell_argv("/bin/bash", Some("  tmux new-session  ")),
            ["/bin/bash", "-lc", "tmux new-session"]
        );
        assert_eq!(
            startup_argv("/bin/bash", Some("htop --tree"), false),
            ["htop", "--tree"]
        );
        assert_eq!(
            login_shell_argv("/bin/bash", Some("  ")),
            ["/bin/bash", "-l"]
        );
    }

    #[test]
    fn direct_startup_preserves_quoted_arguments_without_shell_expansion() {
        assert_eq!(
            startup_argv(
                "/bin/bash",
                Some("printf 'hello world' \"$HOME\" escaped\\ value"),
                false,
            ),
            ["printf", "hello world", "$HOME", "escaped value"]
        );
    }

    #[test]
    fn malformed_or_oversized_direct_startup_falls_back_to_login_shell() {
        assert_eq!(
            startup_argv("/bin/bash", Some("printf 'unterminated"), false),
            ["/bin/bash", "-l"]
        );
        let oversized = "x".repeat(MAX_DIRECT_COMMAND_BYTES + 1);
        assert_eq!(
            startup_argv("/bin/bash", Some(&oversized), false),
            ["/bin/bash", "-l"]
        );
    }

    #[test]
    fn spawn_options_copy_settings_without_enabling_empty_commands() {
        let settings = Settings {
            use_custom_command: true,
            custom_command: "  zellij  ".into(),
            terminal_type: " xterm-direct ".into(),
            ..Settings::default()
        };
        let options = SpawnOptions::from_settings(&settings, Some("/tmp"));
        assert_eq!(options.custom_command.as_deref(), Some("zellij"));
        assert_eq!(options.working_directory.as_deref(), Some("/tmp"));
        assert_eq!(options.terminal_type, "xterm-direct");

        let settings = Settings {
            use_custom_command: true,
            custom_command: String::new(),
            ..Settings::default()
        };
        assert_eq!(
            SpawnOptions::from_settings(&settings, None).custom_command,
            None
        );
    }

    #[test]
    fn spawn_options_copy_profile_shell_command_and_locale_policy() {
        let mut profile = TerminalProfile::homebrew();
        profile.shell = "/bin/bash".into();
        profile.shell_command = "printf 'hello world'".into();
        profile.run_inside_shell = false;
        profile.set_locale_environment = true;
        profile.locale = "C.UTF-8".into();
        profile.terminal_type = "xterm-direct".into();

        let options = SpawnOptions::from_profile(&profile, Some("/tmp"));
        assert_eq!(options.shell.as_deref(), Some("/bin/bash"));
        assert_eq!(
            options.custom_command.as_deref(),
            Some("printf 'hello world'")
        );
        assert!(!options.run_command_inside_shell);
        assert_eq!(options.locale.as_deref(), Some("C.UTF-8"));
        assert_eq!(options.terminal_type, "xterm-direct");
    }

    #[test]
    fn child_environment_preserves_process_environment_and_replaces_term() {
        let environment = child_environment(" xterm-direct ", None);
        assert_eq!(
            environment
                .iter()
                .filter(|entry| entry.starts_with("TERM="))
                .count(),
            1
        );
        assert!(environment.iter().any(|entry| entry == "TERM=xterm-direct"));
        assert!(environment.iter().any(|entry| entry.contains('=')));
        assert_eq!(
            sanitize_terminal_type("xterm;echo unsafe"),
            "xterm-256color"
        );
    }

    #[test]
    fn flatpak_host_command_preserves_arguments_directory_and_environment() {
        let command = vec![
            "/bin/bash".into(),
            "-lc".into(),
            "printf '%s' \"$HOME\"".into(),
        ];
        let environment = vec![
            "HOME=/home/user".into(),
            "TERM=xterm-256color".into(),
            "LANG=C.UTF-8".into(),
            "XDG_CONFIG_HOME=/home/user/.var/app/example/config".into(),
            "PATH=/app/bin:/usr/bin".into(),
            "EXAMPLE_SECRET=not-forwarded-on-the-command-line".into(),
        ];
        assert_eq!(
            flatpak_host_argv(&command, Some("/home/user/Project Files"), &environment),
            [
                "flatpak-spawn",
                "--host",
                "--clear-env",
                "--watch-bus",
                "--directory=/home/user/Project Files",
                "--env=PATH=/home/user/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "--env=HOME=/home/user",
                "--env=TERM=xterm-256color",
                "--env=LANG=C.UTF-8",
                "--",
                "/bin/bash",
                "-lc",
                "printf '%s' \"$HOME\"",
            ]
        );
    }

    #[test]
    fn flatpak_host_command_defaults_to_home_and_drops_sandbox_environment() {
        let command = vec!["/bin/bash".into(), "-l".into()];
        let environment = vec![
            "HOME=/home/user".into(),
            "FLATPAK_ID=io.github.example.App".into(),
            "XDG_DATA_HOME=/home/user/.var/app/example/data".into(),
            "PATH=/app/bin:/usr/bin".into(),
        ];
        let argv = flatpak_host_argv(&command, None, &environment);
        assert!(argv.iter().any(|entry| entry == "--directory=/home/user"));
        assert!(argv.iter().any(|entry| entry == "--clear-env"));
        assert!(!argv.iter().any(|entry| entry.contains("FLATPAK_ID=")));
        assert!(!argv.iter().any(|entry| entry.contains("XDG_DATA_HOME=")));
        assert!(!argv
            .iter()
            .any(|entry| entry == "--env=PATH=/app/bin:/usr/bin"));
    }

    #[test]
    fn tabs_open_switch_and_close_without_losing_active_neighbor() {
        let mut sessions = SessionManager::empty();
        let first = sessions.open_tab(DEFAULT_PROFILE_NAME, None);
        let second = sessions.open_tab("Ocean", None);
        assert_eq!(sessions.tabs().len(), 2);
        assert_eq!(sessions.active().unwrap().id, second);
        sessions.previous_tab();
        assert_eq!(sessions.active().unwrap().id, first);
        sessions.close_tab(first);
        assert_eq!(sessions.tabs().len(), 1);
        assert_eq!(sessions.active().unwrap().id, second);
    }

    #[test]
    fn closing_last_tab_leaves_an_empty_model_for_window_to_close() {
        let mut sessions = SessionManager::empty();
        let id = sessions.open_tab(DEFAULT_PROFILE_NAME, None);
        sessions.close_tab(id);
        assert!(sessions.is_empty());
        assert!(sessions.active().is_none());
    }

    #[test]
    fn ids_remain_unique_after_close() {
        let mut sessions = SessionManager::empty();
        let first = sessions.open_tab(DEFAULT_PROFILE_NAME, None);
        sessions.close_tab(first);
        let second = sessions.open_tab("Pro", None);
        assert_ne!(first, second);
    }

    #[test]
    fn working_directory_tracks_the_live_terminal_directory() {
        let mut sessions = SessionManager::empty();
        let id = sessions.open_tab(DEFAULT_PROFILE_NAME, None);
        sessions.set_working_directory(id, Some("/tmp/project"));
        assert_eq!(
            sessions.active().unwrap().working_directory.as_deref(),
            Some("/tmp/project")
        );
    }

    #[test]
    fn child_exit_policy_distinguishes_clean_and_failed_children() {
        let mut profile = TerminalProfile::homebrew();
        profile.close_on_clean_exit = true;
        profile.close_on_error = false;
        profile.shell_exit_action = ShellExitAction::Keep;
        assert_eq!(
            child_exit_decision(&profile, 0),
            ChildExitDecision::CloseTab
        );
        assert_eq!(
            child_exit_decision(&profile, 1 << 8),
            ChildExitDecision::Keep
        );

        profile.close_on_error = true;
        assert_eq!(
            child_exit_decision(&profile, 1 << 8),
            ChildExitDecision::CloseTab
        );
        profile.close_on_clean_exit = false;
        profile.shell_exit_action = ShellExitAction::CloseWindow;
        assert_eq!(
            child_exit_decision(&profile, 0),
            ChildExitDecision::CloseWindow
        );
    }

    #[test]
    fn close_prompt_policy_honors_non_exempt_processes() {
        let mut profile = TerminalProfile::homebrew();
        profile.ask_before_close_policy = AskBeforeClosePolicy::NonExempt;
        profile.ask_before_close_exceptions = vec!["bash".into()];
        assert!(!should_prompt_before_close(&profile, Some("bash")));
        assert!(should_prompt_before_close(&profile, Some("vim")));
        assert!(should_prompt_before_close(&profile, None));
        profile.ask_before_close_policy = AskBeforeClosePolicy::Never;
        profile.ask_before_close = true;
        assert!(should_prompt_before_close(&profile, Some("bash")));
    }

    #[test]
    fn child_exit_status_decodes_waitpid_values() {
        assert_eq!(
            decode_child_exit_status(0),
            ChildExitStatus {
                code: Some(0),
                signal: None
            }
        );
        assert_eq!(
            decode_child_exit_status(7 << 8),
            ChildExitStatus {
                code: Some(7),
                signal: None
            }
        );
        assert_eq!(
            decode_child_exit_status(libc::SIGTERM),
            ChildExitStatus {
                code: None,
                signal: Some(libc::SIGTERM)
            }
        );
    }

    #[test]
    fn profile_grid_dimensions_are_exposed_for_vte_sizing() {
        let profile = TerminalProfile {
            columns: 132,
            rows: 43,
            cursor_shape: CursorShape::Underline,
            ..TerminalProfile::homebrew()
        };
        assert_eq!((profile.columns, profile.rows), (132, 43));
    }
}
