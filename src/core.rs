//! Terminal process and tab/session lifecycle primitives.

#![allow(dead_code)]

use crate::{
    profiles::{AskBeforeClosePolicy, ShellExitAction, TerminalProfile},
    settings::Settings,
};
use gtk::{gio, glib};
use std::{env, fs::File, os::fd::AsRawFd, time::Duration};

#[cfg(target_os = "linux")]
use std::{
    io::Read,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

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

    pub fn set_profile(&mut self, id: SessionId, profile_name: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.profile_name = profile_name.into();
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

pub fn spawn_terminal<F>(terminal: &vte4::Terminal, options: &SpawnOptions, callback: F)
where
    F: FnOnce(Result<glib::Pid, glib::Error>) + 'static,
{
    let envv_owned = child_environment(&options.terminal_type, options.locale.as_deref());
    if running_in_flatpak() {
        let supervisor = match File::open(FLATPAK_HOST_SUPERVISOR_PATH) {
            Ok(supervisor) => supervisor,
            Err(error) => {
                callback(Err(glib::Error::new(
                    gio::IOErrorEnum::Failed,
                    &format!("cannot open Flatpak host supervisor: {error}"),
                )));
                return;
            }
        };
        let command = flatpak_host_argv(options, env::var("HOME").ok().as_deref());
        let argv: Vec<&str> = command.iter().map(String::as_str).collect();
        let envv: Vec<&str> = envv_owned.iter().map(String::as_str).collect();
        // SAFETY: the one owned helper descriptor is deliberately mapped to
        // fd 3, which matches flatpak-spawn's fixed --forward-fd option.
        unsafe {
            terminal.spawn_with_fds_async(
                vte4::PtyFlags::DEFAULT,
                None,
                &argv,
                &envv,
                vec![supervisor.into()],
                &[FLATPAK_HOST_SUPERVISOR_FD],
                glib::SpawnFlags::DEFAULT,
                || {},
                -1,
                None::<&gio::Cancellable>,
                callback,
            );
        }
        return;
    }
    let shell = options.shell.clone().unwrap_or_else(login_shell);
    let command = startup_argv(
        &shell,
        options.custom_command.as_deref(),
        options.run_command_inside_shell,
    );
    let working_directory = options.working_directory.clone();
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

pub(crate) fn running_in_flatpak() -> bool {
    env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").is_file()
}

const FLATPAK_HOST_SUPERVISOR_PATH: &str = "/app/libexec/core-terminal-host-supervisor";
const FLATPAK_HOST_SUPERVISOR_FD: i32 = 3;
const FLATPAK_HOST_SUPERVISOR_EXEC: &str = "/proc/self/fd/3";

/// Build the command that crosses the Flatpak boundary.
///
/// `flatpak-spawn --host` obtains the desktop session's environment from the
/// Flatpak broker. It must not be cleared and rebuilt from `env::vars()`: the
/// latter is the sandbox environment and contains a sandbox shell, D-Bus
/// proxy, PATH and XDG paths that are invalid for an unsandboxed host child.
/// Only terminal-specific values selected by Core Terminal are overridden.
fn flatpak_host_argv(options: &SpawnOptions, sandbox_home: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "flatpak-spawn".into(),
        "--host".into(),
        "--watch-bus".into(),
        format!("--forward-fd={FLATPAK_HOST_SUPERVISOR_FD}"),
    ];
    let directory = options
        .working_directory
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .or_else(|| {
            sandbox_home
                .map(str::trim)
                .filter(|value| value.starts_with('/') && !value.contains('\0'))
        })
        .unwrap_or("/");
    argv.push(format!("--directory={directory}"));
    argv.push(format!(
        "--env=TERM={}",
        sanitize_terminal_type(&options.terminal_type)
    ));
    argv.push("--env=COLORTERM=truecolor".into());
    argv.push(format!("--env=VTE_VERSION={}", vte_version_number()));
    if let Some(locale) = options.locale.as_deref() {
        argv.push(format!("--env=LANG={locale}"));
        argv.push(format!("--env=LC_ALL={locale}"));
    }
    argv.push("--".into());
    argv.push(FLATPAK_HOST_SUPERVISOR_EXEC.into());
    argv.extend(flatpak_host_command(options));
    argv
}

fn flatpak_host_command(options: &SpawnOptions) -> Vec<String> {
    if let Some(shell) = &options.shell {
        return startup_argv(
            shell,
            options.custom_command.as_deref(),
            options.run_command_inside_shell,
        );
    }

    let Some(command) = options.custom_command.as_deref() else {
        return host_login_shell_argv(None);
    };
    if options.run_command_inside_shell {
        host_login_shell_argv(Some(command))
    } else {
        parse_direct_command(command).unwrap_or_else(|| host_login_shell_argv(None))
    }
}

/// Resolve `$SHELL` after crossing onto the host. Flatpak deliberately exposes
/// `/bin/sh` as the sandbox's `SHELL`, which is unrelated to the user's login
/// shell. The fixed wrapper script treats the broker-provided host value only
/// as an executable path and passes custom command text as a positional
/// argument, so it does not interpolate settings into shell source.
fn host_login_shell_argv(custom_command: Option<&str>) -> Vec<String> {
    match custom_command {
        Some(command) => vec![
            "/bin/sh".into(),
            "-c".into(),
            "host_shell=${SHELL:-/bin/sh}; case $host_shell in /*) ;; *) host_shell=/bin/sh;; esac; exec \"$host_shell\" -lc \"$1\"".into(),
            "core-terminal".into(),
            command.into(),
        ],
        None => vec![
            "/bin/sh".into(),
            "-c".into(),
            "host_shell=${SHELL:-/bin/sh}; case $host_shell in /*) ;; *) host_shell=/bin/sh;; esac; exec \"$host_shell\" -l".into(),
        ],
    }
}

fn vte_version_number() -> u32 {
    // SAFETY: these VTE accessors are pure version queries for the linked
    // library and require no initialized object or borrowed pointer.
    unsafe {
        vte4::ffi::vte_get_major_version() * 10_000
            + vte4::ffi::vte_get_minor_version() * 100
            + vte4::ffi::vte_get_micro_version()
    }
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
        crate::profiles::CloseOnExit::Error if !clean => {
            return ChildExitDecision::CloseTab;
        }
        crate::profiles::CloseOnExit::Always => return ChildExitDecision::CloseTab,
        _ => {}
    }
    match profile.shell_exit_action {
        ShellExitAction::CloseWindow => ChildExitDecision::CloseWindow,
        ShellExitAction::CloseTab => ChildExitDecision::CloseTab,
        ShellExitAction::Keep => ChildExitDecision::Keep,
        ShellExitAction::Ask => ChildExitDecision::Ask,
    }
}

pub fn should_prompt_before_close(profile: &TerminalProfile, process: Option<&str>) -> bool {
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

/// Stable identity for the live or pending process attached to a terminal tab.
///
/// The VTE child PID is retained when available even if the foreground process
/// cannot be inspected. Close planning treats pending and unknown processes as
/// non-exempt rather than assuming that the login shell is still active.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

/// Spawn-time identity for the VTE child process.
///
/// Linux stores the process start time, session ID, and process group from
/// `/proc`. Those values must still match before Core Terminal enumerates or
/// signals a process session, preventing a recycled PID from targeting an
/// unrelated process. Flatpak records the same identity for its local proxy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildProcessIdentity {
    pid: i32,
    start_time: Option<u64>,
    session: Option<i32>,
    process_group: Option<i32>,
    brokered: bool,
}

impl ChildProcessIdentity {
    pub const fn pid(&self) -> glib::Pid {
        glib::Pid(self.pid)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningProcessIdentity {
    pub child_pid: Option<i32>,
    pub foreground_pgid: Option<i32>,
    pub name: Option<String>,
    pub executable: Option<ExecutableIdentity>,
    /// Sorted Linux process IDs that still belong to the VTE child's process
    /// session. `None` means the session could not be inspected reliably.
    pub session_processes: Option<Vec<i32>>,
}

impl RunningProcessIdentity {
    /// Represent a spawn request that has not returned its child PID yet.
    ///
    /// Pending spawns participate in close planning as unknown processes. Once
    /// a PID arrives, the identity changes and any earlier confirmation must be
    /// revalidated.
    pub const fn pending() -> Self {
        Self {
            child_pid: None,
            foreground_pgid: None,
            name: None,
            executable: None,
            session_processes: None,
        }
    }

    /// Represent a PID whose spawn-time identity is unavailable or no longer
    /// matches. It remains a close blocker but is never signalled as native
    /// process-session authority.
    pub const fn unverified(child_pid: glib::Pid) -> Self {
        Self {
            child_pid: Some(child_pid.0),
            foreground_pgid: None,
            name: None,
            executable: None,
            session_processes: None,
        }
    }
}

/// One tab considered by a tab- or window-close request.
#[derive(Clone, Debug)]
pub struct CloseCandidate<'a> {
    pub session_id: SessionId,
    pub process: Option<RunningProcessIdentity>,
    pub profile: Option<&'a TerminalProfile>,
    /// Kernel-backed executable identity expected for a login-shell launch. A
    /// custom command or an unresolvable shell has no automatic exemption.
    pub expected_login_shell: Option<&'a ExecutableIdentity>,
}

/// A live or pending process whose profile requires confirmation before closing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseBlocker {
    pub session_id: SessionId,
    pub process: RunningProcessIdentity,
}

/// Immutable result of evaluating every tab in one close request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosePlan {
    pub targets: Vec<SessionId>,
    pub blockers: Vec<CloseBlocker>,
}

/// Evaluate tab close policies without changing session or process state.
///
/// A live tab whose profile cannot be resolved is a blocker. Failing safe here
/// prevents malformed or concurrently edited profile data from silently
/// terminating a process.
pub fn plan_close<'a>(candidates: impl IntoIterator<Item = CloseCandidate<'a>>) -> ClosePlan {
    let mut targets = Vec::new();
    let mut blockers = Vec::new();
    for candidate in candidates {
        targets.push(candidate.session_id);
        let Some(process) = candidate.process else {
            continue;
        };
        let is_native_idle_login_shell = candidate
            .expected_login_shell
            .zip(process.executable.as_ref())
            .is_some_and(|(expected, actual)| expected == actual)
            && process.child_pid.is_some()
            && process.child_pid == process.foreground_pgid
            && process.session_processes.as_deref()
                == process.child_pid.as_ref().map(std::slice::from_ref);
        let should_prompt = match candidate.profile {
            None => true,
            Some(profile)
                if profile.ask_before_close_policy == AskBeforeClosePolicy::NonExempt
                    && is_native_idle_login_shell =>
            {
                false
            }
            Some(profile) => should_prompt_before_close(profile, process.name.as_deref()),
        };
        if should_prompt {
            blockers.push(CloseBlocker {
                session_id: candidate.session_id,
                process,
            });
        }
    }
    ClosePlan { targets, blockers }
}

/// Return whether a previous confirmation still covers the current close plan.
///
/// Tabs and processes may exit while a non-modal confirmation is open, so the
/// current target and blocker sets may be subsets of the confirmed sets. A new
/// target or a changed PID, foreground process group, or process name
/// invalidates authorization.
pub fn close_authorization_covers(plan: &ClosePlan, confirmed: &ClosePlan) -> bool {
    plan.targets
        .iter()
        .all(|target| confirmed.targets.contains(target))
        && plan
            .blockers
            .iter()
            .all(|blocker| confirmed.blockers.contains(blocker))
}

/// Inspect the kernel-owned foreground process for a VTE PTY when possible.
///
/// A Flatpak VTE child is `flatpak-spawn`, a sandbox-side proxy for a host
/// process. Its name must not be used to exempt an otherwise unobservable host
/// command, so Flatpak deliberately returns an unknown foreground process.
pub fn running_process_identity(
    terminal: Option<&vte4::Terminal>,
    child: &ChildProcessIdentity,
) -> RunningProcessIdentity {
    let child_pid = child.pid();
    if !child_process_identity_is_current(child) {
        return RunningProcessIdentity::unverified(child_pid);
    }
    let foreground_pgid = if running_in_flatpak() {
        None
    } else {
        terminal.and_then(foreground_process_group)
    };
    RunningProcessIdentity {
        child_pid: Some(child_pid.0),
        foreground_pgid,
        name: foreground_pgid.and_then(process_name_for_pid),
        executable: foreground_pgid.and_then(process_executable_identity),
        session_processes: if running_in_flatpak() {
            None
        } else {
            session_processes_for_pid(child_pid.0)
        },
    }
}

/// Capture the kernel identity of a just-spawned VTE child.
///
/// Failure on Linux means the child already exited or `/proc` could not
/// be inspected. Callers must retain the PID for display/lifecycle bookkeeping
/// but must not later signal it without this token.
pub fn child_process_identity(pid: glib::Pid) -> Option<ChildProcessIdentity> {
    if pid.0 <= 1 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let process = proc_process(pid.0)?;
        Some(ChildProcessIdentity {
            pid: pid.0,
            start_time: Some(process.start_time),
            session: Some(process.session),
            process_group: Some(process.process_group),
            brokered: running_in_flatpak(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn child_process_identity_is_current(identity: &ChildProcessIdentity) -> bool {
    if identity.brokered != running_in_flatpak() {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        identity
            .expected_process()
            .zip(proc_process(identity.pid))
            .is_some_and(|(expected, current)| same_process(&expected, &current))
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Resolve the kernel-backed identity a native login shell is expected to
/// expose in `/proc/<pid>/exe`. Device and inode identity prevents a different
/// executable with the same basename from being mistaken for an idle shell.
pub fn expected_executable_identity(path: &str) -> Option<ExecutableIdentity> {
    #[cfg(target_os = "linux")]
    {
        let canonical = std::fs::canonicalize(path).ok()?;
        executable_identity_from_path(&canonical)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

fn foreground_process_group(terminal: &vte4::Terminal) -> Option<i32> {
    use vte4::prelude::TerminalExt;

    let pty = terminal.pty()?;
    // SAFETY: VTE owns the borrowed PTY file descriptor for the duration of
    // this call. tcgetpgrp does not retain or modify the descriptor.
    let process_group = unsafe { libc::tcgetpgrp(pty.fd().as_raw_fd()) };
    (process_group > 0).then_some(process_group)
}

#[cfg(target_os = "linux")]
fn process_name_for_pid(pid: i32) -> Option<String> {
    if pid <= 1 {
        return None;
    }
    let proc_root = format!("/proc/{pid}");
    executable_name_from_link(Path::new(&proc_root).join("exe")).or_else(|| {
        // `comm` is writable by the process itself, so its value is advisory.
        // Marking it prevents built-in exception names such as `bash` from
        // treating an unverified fallback as a trusted executable identity.
        read_bounded_proc_file(Path::new(&proc_root).join("comm"), MAX_PROC_COMM_BYTES)
            .and_then(|value| advisory_process_name_from_comm(&value))
    })
}

#[cfg(target_os = "linux")]
fn process_executable_identity(pid: i32) -> Option<ExecutableIdentity> {
    (pid > 1).then_some(())?;
    executable_identity_from_path(format!("/proc/{pid}/exe"))
}

#[cfg(not(target_os = "linux"))]
fn process_executable_identity(_pid: i32) -> Option<ExecutableIdentity> {
    None
}

#[cfg(not(target_os = "linux"))]
fn process_name_for_pid(_pid: i32) -> Option<String> {
    None
}

const MAX_PROCESS_NAME_BYTES: usize = 255;

#[cfg(target_os = "linux")]
const MAX_PROC_COMM_BYTES: usize = 64;

#[cfg(target_os = "linux")]
const MAX_PROC_STAT_BYTES: usize = 4096;

#[cfg(target_os = "linux")]
fn session_processes_for_pid(child_pid: i32) -> Option<Vec<i32>> {
    session_members_for_pid(child_pid)
        .map(|members| members.into_iter().map(|member| member.pid).collect())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcProcess {
    pid: i32,
    process_group: i32,
    session: i32,
    start_time: u64,
}

#[cfg(target_os = "linux")]
impl ChildProcessIdentity {
    fn expected_process(&self) -> Option<ProcProcess> {
        Some(ProcProcess {
            pid: self.pid,
            process_group: self.process_group?,
            session: self.session?,
            start_time: self.start_time?,
        })
    }
}

#[cfg(target_os = "linux")]
fn session_members_for_pid(child_pid: i32) -> Option<Vec<ProcProcess>> {
    if child_pid <= 1 {
        return None;
    }
    let child_session = proc_process(child_pid)?.session;
    let members = session_members_for_session(child_session)?;
    members
        .iter()
        .any(|member| member.pid == child_pid)
        .then_some(members)
}

#[cfg(target_os = "linux")]
fn session_members_for_session(session: i32) -> Option<Vec<ProcProcess>> {
    if session <= 1 {
        return None;
    }
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some(process) = proc_process(pid) {
            if process.session == session {
                members.push(process);
            }
        }
    }
    members.sort_unstable_by_key(|member| member.pid);
    members.dedup_by_key(|member| member.pid);
    (!members.is_empty()).then_some(members)
}

#[cfg(not(target_os = "linux"))]
fn session_processes_for_pid(_child_pid: i32) -> Option<Vec<i32>> {
    None
}

#[cfg(target_os = "linux")]
fn proc_process(pid: i32) -> Option<ProcProcess> {
    let stat = read_bounded_proc_file(format!("/proc/{pid}/stat"), MAX_PROC_STAT_BYTES)?;
    proc_process_from_stat(&stat)
}

#[cfg(target_os = "linux")]
fn proc_process_from_stat(stat: &[u8]) -> Option<ProcProcess> {
    let stat = std::str::from_utf8(stat).ok()?;
    // The command field is parenthesized and may contain spaces or `)`, so
    // split after its final closing delimiter. The remaining fields begin
    // with state, parent PID, process group, then session ID.
    let fields = stat.rsplit_once(") ")?.1;
    let mut fields = fields.split_whitespace();
    let _state = fields.next()?;
    let _parent = fields.next()?;
    let process_group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    for _ in 0..15 {
        fields.next()?;
    }
    let start_time = fields.next()?.parse().ok()?;
    let pid = stat.split_whitespace().next()?.parse().ok()?;
    Some(ProcProcess {
        pid,
        process_group,
        session,
        start_time,
    })
}

#[cfg(target_os = "linux")]
fn executable_name_from_link(path: impl AsRef<Path>) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    executable_name_from_target(&target)
}

#[cfg(target_os = "linux")]
fn executable_identity_from_path(path: impl AsRef<Path>) -> Option<ExecutableIdentity> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(ExecutableIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "linux")]
fn read_bounded_proc_file(path: impl AsRef<Path>, max_bytes: usize) -> Option<Vec<u8>> {
    let mut value = Vec::with_capacity(max_bytes.min(256) + 1);
    std::fs::File::open(path)
        .ok()?
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut value)
        .ok()?;
    (value.len() <= max_bytes).then_some(value)
}

#[cfg(target_os = "linux")]
fn executable_name_from_target(target: &Path) -> Option<String> {
    let name = target.file_name()?.to_str()?;
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    normalize_process_name(name)
}

fn normalize_process_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_PROCESS_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return None;
    }
    let name = value.rsplit('/').next()?.trim();
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

#[cfg(target_os = "linux")]
fn advisory_process_name_from_comm(value: &[u8]) -> Option<String> {
    std::str::from_utf8(value)
        .ok()
        .and_then(normalize_process_name)
        .map(|name| format!("{name} (unverified)"))
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

#[cfg(target_os = "linux")]
fn same_process(expected: &ProcProcess, current: &ProcProcess) -> bool {
    current.pid == expected.pid
        && current.session == expected.session
        && current.process_group == expected.process_group
        && current.start_time == expected.start_time
}

/// Open a kernel process handle before validating the numeric PID. If the PID
/// was recycled before `pidfd_open`, the subsequent `/proc` comparison rejects
/// the replacement. If it exits after validation, the pidfd remains bound to
/// the old process and cannot retarget a later reuse.
#[cfg(target_os = "linux")]
fn open_validated_pidfd(expected: &ProcProcess) -> Option<OwnedFd> {
    // SAFETY: `pidfd_open` takes a numeric PID and flags value and returns a new
    // owned file descriptor on success. The descriptor is immediately wrapped
    // in `OwnedFd` so every later return path closes it.
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0u32) };
    if raw_fd < 0 {
        return None;
    }
    // SAFETY: a successful `pidfd_open` returns a fresh descriptor owned by the
    // caller, transferred exactly once into `OwnedFd`.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as i32) };
    proc_process(expected.pid)
        .filter(|current| same_process(expected, current))
        .map(|_| pidfd)
}

#[cfg(target_os = "linux")]
fn send_pidfd_signal(pidfd: &OwnedFd, signal: i32) -> bool {
    // SAFETY: `pidfd` is a live owned process descriptor; a null siginfo and
    // zero flags have the same semantics as `kill(2)` for the supplied signal.
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        ) == 0
    }
}

#[cfg(target_os = "linux")]
fn signal_validated_process(expected: &ProcProcess, signal: i32) -> bool {
    open_validated_pidfd(expected)
        .as_ref()
        .is_some_and(|pidfd| send_pidfd_signal(pidfd, signal))
}

/// Emergency cleanup for the native acceptance probe. Keep the operation
/// crate-private and fixed to SIGKILL: production shutdown uses
/// `terminate_child`, while the harness may need to remove an exact leftover
/// job after a failed assertion.
#[cfg(target_os = "linux")]
pub(crate) fn kill_process_if_exact(
    pid: i32,
    start_time: u64,
    session: i32,
    process_group: i32,
) -> bool {
    if pid <= 1 || session == unsafe { libc::getsid(0) } {
        return false;
    }
    signal_validated_process(
        &ProcProcess {
            pid,
            process_group,
            session,
            start_time,
        },
        libc::SIGKILL,
    )
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn kill_process_if_exact(
    _pid: i32,
    _start_time: u64,
    _session: i32,
    _process_group: i32,
) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn revalidated_session_members(session: i32, anchors: &[ProcProcess]) -> Option<Vec<ProcProcess>> {
    for expected in anchors
        .iter()
        .filter(|expected| expected.session == session)
    {
        let Some(pidfd) = open_validated_pidfd(expected) else {
            continue;
        };
        let members = session_members_for_session(session)?;
        // Signal zero verifies that the pidfd-bound anchor remained alive for
        // the complete enumeration. Therefore the numeric session ID could not
        // have disappeared and been reused for an unrelated session midway.
        if send_pidfd_signal(&pidfd, 0) {
            return Some(members);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn signal_session_members(members: &mut [ProcProcess], leader_pid: i32, signal: i32) {
    // Keep the session leader alive until its jobs receive each signal. This
    // preserves an authorization anchor while newly forked members are
    // discovered between escalation stages.
    members.sort_unstable_by_key(|member| member.pid == leader_pid);
    for member in members {
        let _ = signal_validated_process(member, signal);
    }
}

/// Terminate processes still in a terminal's native process session.
/// Interactive foreground and background jobs can have process groups that do
/// not match VTE's child PID, so session membership is re-enumerated between
/// escalation stages and every member is revalidated immediately before a
/// signal. The caller runs this escalation away from GTK's main thread so the
/// grace intervals do not stall input or window redraws.
pub fn terminate_child(child: ChildProcessIdentity) {
    let pid = child.pid;
    if pid <= 1 {
        return;
    }
    if !child_process_identity_is_current(&child) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let Some(expected_child) = child.expected_process() else {
            return;
        };
        if child.brokered {
            // The sandbox-side `flatpak-spawn` proxy is the only process Core
            // Terminal can identify directly. Its host-side supervisor expands
            // a proxy signal to the verified private host session. Give the
            // synchronous supervisor time to consume HUP and begin cleanup
            // before signalling the proxy again.
            if signal_validated_process(&expected_child, libc::SIGHUP) {
                std::thread::sleep(Duration::from_millis(200));
                let _ = signal_validated_process(&expected_child, libc::SIGTERM);
            }
            return;
        }
        // VTE creates a separate process session. Never enumerate or signal the
        // application's own login/desktop session if that invariant cannot be
        // established.
        let own_session = unsafe { libc::getsid(0) };
        let session = expected_child.session;
        if session == own_session {
            return;
        }
        let Some(mut members) = revalidated_session_members(session, &[expected_child]) else {
            return;
        };
        let Some(mut current) = revalidated_session_members(session, &members) else {
            return;
        };
        signal_session_members(&mut current, pid, libc::SIGHUP);
        members = current;

        // Give shells and jobs a chance to react to their terminal going away
        // before escalating. TERM then receives a full second for handlers to
        // flush state and exit cleanly before KILL is considered.
        std::thread::sleep(Duration::from_millis(200));
        let Some(mut current) = revalidated_session_members(session, &members) else {
            return;
        };
        signal_session_members(&mut current, pid, libc::SIGTERM);
        members = current;

        std::thread::sleep(Duration::from_secs(1));
        if let Some(mut current) = revalidated_session_members(session, &members) {
            signal_session_members(&mut current, pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{CloseOnExit, CursorShape, DEFAULT_PROFILE_NAME};

    fn running_process(
        child_pid: i32,
        foreground_pgid: Option<i32>,
        name: Option<&str>,
    ) -> RunningProcessIdentity {
        RunningProcessIdentity {
            child_pid: Some(child_pid),
            foreground_pgid,
            name: name.map(str::to_owned),
            executable: name.map(test_executable),
            session_processes: foreground_pgid.map(|_| vec![child_pid]),
        }
    }

    fn test_executable(name: &str) -> ExecutableIdentity {
        ExecutableIdentity {
            device: 1,
            inode: name.bytes().map(u64::from).sum(),
        }
    }

    fn close_blocker(
        session_id: u64,
        child_pid: i32,
        foreground_pgid: Option<i32>,
        name: Option<&str>,
    ) -> CloseBlocker {
        CloseBlocker {
            session_id: SessionId::from(session_id),
            process: running_process(child_pid, foreground_pgid, name),
        }
    }

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
    fn flatpak_host_command_preserves_explicit_shell_arguments_and_directory() {
        let options = SpawnOptions {
            working_directory: Some("/home/user/Project Files".into()),
            custom_command: Some("printf '%s' \"$HOME\"".into()),
            terminal_type: "xterm-256color".into(),
            shell: Some("/bin/bash".into()),
            run_command_inside_shell: true,
            locale: Some("C.UTF-8".into()),
        };
        let argv = flatpak_host_argv(&options, Some("/home/user"));
        assert_eq!(&argv[..3], ["flatpak-spawn", "--host", "--watch-bus"]);
        assert!(argv
            .iter()
            .any(|entry| entry == "--directory=/home/user/Project Files"));
        assert!(argv
            .iter()
            .any(|entry| entry == "--env=TERM=xterm-256color"));
        assert!(argv
            .iter()
            .any(|entry| entry == "--env=COLORTERM=truecolor"));
        assert!(argv.iter().any(|entry| entry == "--env=LANG=C.UTF-8"));
        assert!(argv.iter().any(|entry| entry == "--env=LC_ALL=C.UTF-8"));
        assert!(argv.iter().any(|entry| entry == "--forward-fd=3"));
        let separator = argv.iter().position(|entry| entry == "--").unwrap();
        assert_eq!(argv[separator + 1], FLATPAK_HOST_SUPERVISOR_EXEC);
        assert_eq!(&argv[separator + 2..separator + 4], ["/bin/bash", "-lc"]);
        assert_eq!(
            &argv[argv.len() - 3..],
            ["/bin/bash", "-lc", "printf '%s' \"$HOME\""]
        );
    }

    #[test]
    fn flatpak_host_command_uses_broker_environment_and_resolves_default_shell_on_host() {
        let options = SpawnOptions::new(None, None, "xterm-direct");
        let argv = flatpak_host_argv(&options, Some("/home/user"));
        assert!(argv.iter().any(|entry| entry == "--directory=/home/user"));
        assert!(!argv.iter().any(|entry| entry == "--clear-env"));
        assert!(!argv.iter().any(|entry| entry.starts_with("--env=PATH=")));
        assert!(!argv.iter().any(|entry| entry.starts_with("--env=HOME=")));
        assert!(!argv
            .iter()
            .any(|entry| entry.starts_with("--env=DBUS_SESSION_BUS_ADDRESS=")));
        assert!(!argv.iter().any(|entry| entry.starts_with("--env=XDG_")));
        assert_eq!(argv[argv.len() - 3], "/bin/sh");
        assert_eq!(argv[argv.len() - 2], "-c");
        assert!(argv[argv.len() - 1].contains("${SHELL:-/bin/sh}"));
    }

    #[test]
    fn flatpak_default_shell_custom_command_is_a_positional_argument() {
        let options = SpawnOptions::new(
            None,
            Some("printf '%s' \"$HOME\"; touch /tmp/example"),
            "xterm-256color",
        );
        let argv = flatpak_host_argv(&options, Some("/home/user"));
        assert_eq!(argv[argv.len() - 5], "/bin/sh");
        assert_eq!(argv[argv.len() - 4], "-c");
        assert!(argv[argv.len() - 3].contains("exec \"$host_shell\" -lc \"$1\""));
        assert_eq!(argv[argv.len() - 2], "core-terminal");
        assert_eq!(
            argv[argv.len() - 1],
            "printf '%s' \"$HOME\"; touch /tmp/example"
        );
    }

    #[test]
    fn flatpak_direct_command_does_not_use_a_shell() {
        let mut options = SpawnOptions::new(
            None,
            Some("printf 'hello world' \"$HOME\""),
            "xterm-256color",
        );
        options.run_command_inside_shell = false;
        assert_eq!(
            flatpak_host_command(&options),
            ["printf", "hello world", "$HOME"]
        );
    }

    #[test]
    fn flatpak_host_command_executes_only_the_forwarded_supervisor() {
        let options = SpawnOptions::new(None, Some("printf '%s' safe"), "xterm-256color")
            .with_shell("/bin/bash", true);
        let argv = flatpak_host_argv(&options, Some("/home/user"));
        let separator = argv.iter().position(|entry| entry == "--").unwrap();
        assert_eq!(argv[separator + 1], "/proc/self/fd/3");
        assert_eq!(argv[separator + 2], "/bin/bash");
        assert_eq!(argv[separator + 3], "-lc");
        assert_eq!(argv[separator + 4], "printf '%s' safe");
        assert!(!argv.iter().any(|entry| entry == "/bin/sh"));
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
    fn changing_a_tabs_profile_preserves_its_session_identity_and_directory() {
        let mut sessions = SessionManager::empty();
        let id = sessions.open_tab("Homebrew", Some("/tmp/project"));
        sessions.set_profile(id, "Ocean");
        let tab = sessions.tab(id).unwrap();
        assert_eq!(tab.id, id);
        assert_eq!(tab.profile_name, "Ocean");
        assert_eq!(tab.working_directory.as_deref(), Some("/tmp/project"));
    }

    #[test]
    fn legacy_child_exit_flags_do_not_override_explicit_policy() {
        let mut profile = TerminalProfile::homebrew();
        profile.close_on_exit = CloseOnExit::Never;
        profile.close_on_clean_exit = true;
        profile.close_on_error = false;
        profile.shell_exit_action = ShellExitAction::Keep;
        assert_eq!(child_exit_decision(&profile, 0), ChildExitDecision::Keep);
        assert_eq!(
            child_exit_decision(&profile, 1 << 8),
            ChildExitDecision::Keep
        );

        profile.close_on_error = true;
        assert_eq!(
            child_exit_decision(&profile, 1 << 8),
            ChildExitDecision::Keep
        );
        profile.close_on_clean_exit = false;
        profile.shell_exit_action = ShellExitAction::CloseWindow;
        assert_eq!(
            child_exit_decision(&profile, 0),
            ChildExitDecision::CloseWindow
        );
    }

    #[test]
    fn close_on_exit_policy_distinguishes_clean_and_error_statuses() {
        let mut profile = TerminalProfile::homebrew();
        profile.close_on_clean_exit = false;
        profile.close_on_error = false;
        profile.shell_exit_action = ShellExitAction::Keep;

        profile.close_on_exit = CloseOnExit::Clean;
        assert_eq!(
            child_exit_decision(&profile, 0),
            ChildExitDecision::CloseTab
        );
        assert_eq!(
            child_exit_decision(&profile, 7 << 8),
            ChildExitDecision::Keep
        );

        profile.close_on_exit = CloseOnExit::Error;
        assert_eq!(child_exit_decision(&profile, 0), ChildExitDecision::Keep);
        assert_eq!(
            child_exit_decision(&profile, 7 << 8),
            ChildExitDecision::CloseTab
        );
        assert_eq!(
            child_exit_decision(&profile, libc::SIGTERM),
            ChildExitDecision::CloseTab
        );
    }

    #[test]
    fn every_close_on_exit_rule_precedes_every_fallback_action() {
        let fallback_decision = |action| match action {
            ShellExitAction::Ask => ChildExitDecision::Ask,
            ShellExitAction::Keep => ChildExitDecision::Keep,
            ShellExitAction::CloseTab => ChildExitDecision::CloseTab,
            ShellExitAction::CloseWindow => ChildExitDecision::CloseWindow,
        };
        for close_on_exit in [
            CloseOnExit::Never,
            CloseOnExit::Clean,
            CloseOnExit::Error,
            CloseOnExit::Always,
        ] {
            for shell_exit_action in [
                ShellExitAction::Ask,
                ShellExitAction::Keep,
                ShellExitAction::CloseTab,
                ShellExitAction::CloseWindow,
            ] {
                for (status, clean) in [(0, true), (7 << 8, false), (libc::SIGTERM, false)] {
                    let mut profile = TerminalProfile::homebrew();
                    profile.close_on_exit = close_on_exit;
                    profile.shell_exit_action = shell_exit_action;
                    let automatic_close = match close_on_exit {
                        CloseOnExit::Never => false,
                        CloseOnExit::Clean => clean,
                        CloseOnExit::Error => !clean,
                        CloseOnExit::Always => true,
                    };
                    let expected = if automatic_close {
                        ChildExitDecision::CloseTab
                    } else {
                        fallback_decision(shell_exit_action)
                    };
                    assert_eq!(child_exit_decision(&profile, status), expected);
                }
            }
        }
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
        assert!(!should_prompt_before_close(&profile, Some("bash")));
    }

    #[test]
    fn close_plan_ignores_exited_sessions() {
        let profile = TerminalProfile::homebrew();
        let id = SessionId::from(11);
        let plan = plan_close([CloseCandidate {
            session_id: id,
            process: None,
            profile: Some(&profile),
            expected_login_shell: None,
        }]);
        assert_eq!(plan.targets, [id]);
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn close_plan_honors_always_and_never() {
        let mut always = TerminalProfile::homebrew();
        always.ask_before_close_policy = AskBeforeClosePolicy::Always;
        let mut never = TerminalProfile::homebrew();
        never.ask_before_close_policy = AskBeforeClosePolicy::Never;
        let always_id = SessionId::from(12);
        let never_id = SessionId::from(13);
        let always_process = running_process(1200, Some(1201), Some("bash"));
        let plan = plan_close([
            CloseCandidate {
                session_id: always_id,
                process: Some(always_process.clone()),
                profile: Some(&always),
                expected_login_shell: None,
            },
            CloseCandidate {
                session_id: never_id,
                process: Some(running_process(1300, Some(1301), Some("vim"))),
                profile: Some(&never),
                expected_login_shell: None,
            },
        ]);
        assert_eq!(plan.targets, [always_id, never_id]);
        assert_eq!(
            plan.blockers,
            [CloseBlocker {
                session_id: always_id,
                process: always_process,
            }]
        );
    }

    #[test]
    fn close_plan_honors_non_exempt_foreground_process() {
        let mut profile = TerminalProfile::homebrew();
        profile.ask_before_close_policy = AskBeforeClosePolicy::NonExempt;
        profile.ask_before_close_exceptions = vec!["bash".into(), "tmux".into()];
        let shell_id = SessionId::from(14);
        let editor_id = SessionId::from(15);
        let unknown_id = SessionId::from(16);
        let editor_process = running_process(1500, Some(1501), Some("vim"));
        let unknown_process = running_process(1600, None, None);
        let plan = plan_close([
            CloseCandidate {
                session_id: shell_id,
                process: Some(running_process(1400, Some(1401), Some("bash"))),
                profile: Some(&profile),
                expected_login_shell: None,
            },
            CloseCandidate {
                session_id: editor_id,
                process: Some(editor_process.clone()),
                profile: Some(&profile),
                expected_login_shell: None,
            },
            CloseCandidate {
                session_id: unknown_id,
                process: Some(unknown_process.clone()),
                profile: Some(&profile),
                expected_login_shell: None,
            },
        ]);
        assert_eq!(plan.targets, [shell_id, editor_id, unknown_id]);
        assert_eq!(
            plan.blockers,
            [
                CloseBlocker {
                    session_id: editor_id,
                    process: editor_process,
                },
                CloseBlocker {
                    session_id: unknown_id,
                    process: unknown_process,
                },
            ]
        );
    }

    #[test]
    fn close_plan_fails_safe_when_profile_is_missing() {
        let id = SessionId::from(17);
        let process = running_process(1700, Some(1701), Some("bash"));
        let plan = plan_close([CloseCandidate {
            session_id: id,
            process: Some(process.clone()),
            profile: None,
            expected_login_shell: None,
        }]);
        assert_eq!(
            plan.blockers,
            [CloseBlocker {
                session_id: id,
                process,
            }]
        );
    }

    #[test]
    fn pending_spawn_blocks_close_and_invalidates_its_confirmation_on_start() {
        let mut profile = TerminalProfile::homebrew();
        profile.ask_before_close_policy = AskBeforeClosePolicy::NonExempt;
        profile.ask_before_close_exceptions = vec!["bash".into()];
        let id = SessionId::from(24);
        let expected_shell = test_executable("bash");
        let pending_plan = plan_close([CloseCandidate {
            session_id: id,
            process: Some(RunningProcessIdentity::pending()),
            profile: Some(&profile),
            expected_login_shell: Some(&expected_shell),
        }]);
        assert_eq!(pending_plan.blockers.len(), 1);
        assert_eq!(pending_plan.blockers[0].process.child_pid, None);

        let running_plan = plan_close([CloseCandidate {
            session_id: id,
            process: Some(running_process(2400, Some(2401), Some("vim"))),
            profile: Some(&profile),
            expected_login_shell: Some(&expected_shell),
        }]);
        assert!(!close_authorization_covers(&running_plan, &pending_plan));
    }

    #[test]
    fn only_a_native_idle_login_shell_is_automatically_exempt() {
        let mut profile = TerminalProfile::homebrew();
        profile.ask_before_close_policy = AskBeforeClosePolicy::NonExempt;
        profile.ask_before_close_exceptions.clear();
        let expected_shell = test_executable("bash");

        let idle_id = SessionId::from(25);
        let job_id = SessionId::from(26);
        let pending_id = SessionId::from(27);
        let unobservable_id = SessionId::from(28);
        let job = running_process(2600, Some(2601), Some("vim"));
        let pending = RunningProcessIdentity::pending();
        let unobservable = running_process(2800, None, None);
        let mut background_job = running_process(2900, Some(2900), Some("bash"));
        background_job.session_processes = Some(vec![2900, 2901]);
        let replaced_shell = running_process(3000, Some(3000), Some("sleep"));
        let mut same_name_replacement = running_process(3100, Some(3100), Some("bash"));
        same_name_replacement.executable = Some(ExecutableIdentity {
            device: 99,
            inode: 99,
        });
        let plan = plan_close([
            CloseCandidate {
                session_id: idle_id,
                process: Some(running_process(2500, Some(2500), Some("bash"))),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: job_id,
                process: Some(job.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: pending_id,
                process: Some(pending.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: unobservable_id,
                process: Some(unobservable.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: SessionId::from(29),
                process: Some(background_job.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: SessionId::from(30),
                process: Some(replaced_shell.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
            CloseCandidate {
                session_id: SessionId::from(31),
                process: Some(same_name_replacement.clone()),
                profile: Some(&profile),
                expected_login_shell: Some(&expected_shell),
            },
        ]);

        assert_eq!(
            plan.blockers,
            [
                CloseBlocker {
                    session_id: job_id,
                    process: job,
                },
                CloseBlocker {
                    session_id: pending_id,
                    process: pending,
                },
                CloseBlocker {
                    session_id: unobservable_id,
                    process: unobservable,
                },
                CloseBlocker {
                    session_id: SessionId::from(29),
                    process: background_job,
                },
                CloseBlocker {
                    session_id: SessionId::from(30),
                    process: replaced_shell,
                },
                CloseBlocker {
                    session_id: SessionId::from(31),
                    process: same_name_replacement,
                },
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parser_handles_spaces_and_closing_parentheses_in_command() {
        assert_eq!(
            proc_process_from_stat(
                b"123 (odd ) shell) S 1 123 456 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 98765\n"
            ),
            Some(ProcProcess {
                pid: 123,
                process_group: 123,
                session: 456,
                start_time: 98765,
            })
        );
        assert_eq!(proc_process_from_stat(b"malformed"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_time_identity_rejects_a_reused_or_changed_process() {
        if running_in_flatpak() {
            return;
        }
        let pid = glib::Pid(unsafe { libc::getpid() });
        let identity = child_process_identity(pid).expect("test process identity");
        assert!(child_process_identity_is_current(&identity));

        let mut changed_start = identity.clone();
        changed_start.start_time = changed_start
            .start_time
            .map(|value| value.saturating_add(1));
        assert!(!child_process_identity_is_current(&changed_start));

        let mut changed_session = identity.clone();
        changed_session.session = changed_session.session.map(|value| value.saturating_add(1));
        assert!(!child_process_identity_is_current(&changed_session));

        let mut changed_group = identity;
        changed_group.process_group = changed_group
            .process_group
            .map(|value| value.saturating_add(1));
        assert!(!child_process_identity_is_current(&changed_group));
        assert_eq!(child_process_identity(glib::Pid(1)), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pidfd_binding_validates_the_complete_process_identity() {
        let pid = unsafe { libc::getpid() };
        let expected = proc_process(pid).expect("test process identity");
        let pidfd = open_validated_pidfd(&expected).expect("validated pidfd");
        assert!(send_pidfd_signal(&pidfd, 0));

        let mut changed_start = expected;
        changed_start.start_time = changed_start.start_time.saturating_add(1);
        assert!(open_validated_pidfd(&changed_start).is_none());

        let mut changed_session = expected;
        changed_session.session = changed_session.session.saturating_add(1);
        assert!(open_validated_pidfd(&changed_session).is_none());

        let mut changed_group = expected;
        changed_group.process_group = changed_group.process_group.saturating_add(1);
        assert!(open_validated_pidfd(&changed_group).is_none());
    }

    #[test]
    fn close_authorization_rejects_new_or_changed_blockers() {
        let confirmed = close_blocker(18, 1800, Some(1801), Some("vim"));
        let confirmed_plan = ClosePlan {
            targets: vec![confirmed.session_id],
            blockers: vec![confirmed.clone()],
        };
        let new_blocker = close_blocker(19, 1900, Some(1901), Some("top"));
        let plan_with_new_process = ClosePlan {
            targets: vec![confirmed.session_id, new_blocker.session_id],
            blockers: vec![confirmed.clone(), new_blocker],
        };
        assert!(!close_authorization_covers(
            &plan_with_new_process,
            &confirmed_plan
        ));

        let changed_process = close_blocker(18, 1800, Some(1802), Some("less"));
        let plan_with_changed_process = ClosePlan {
            targets: vec![changed_process.session_id],
            blockers: vec![changed_process],
        };
        assert!(!close_authorization_covers(
            &plan_with_changed_process,
            &confirmed_plan
        ));
    }

    #[test]
    fn close_authorization_rejects_a_new_unblocked_target() {
        let confirmed = close_blocker(22, 2200, Some(2201), Some("vim"));
        let confirmed_plan = ClosePlan {
            targets: vec![confirmed.session_id],
            blockers: vec![confirmed.clone()],
        };
        let plan_with_new_target = ClosePlan {
            targets: vec![confirmed.session_id, SessionId::from(23)],
            blockers: vec![confirmed],
        };
        assert!(!close_authorization_covers(
            &plan_with_new_target,
            &confirmed_plan
        ));
    }

    #[test]
    fn close_authorization_accepts_same_or_exited_blockers() {
        let first = close_blocker(20, 2000, Some(2001), Some("vim"));
        let second = close_blocker(21, 2100, Some(2101), Some("top"));
        let confirmed_plan = ClosePlan {
            targets: vec![first.session_id, second.session_id],
            blockers: vec![first.clone(), second.clone()],
        };
        assert!(close_authorization_covers(&confirmed_plan, &confirmed_plan));

        let one_exited = ClosePlan {
            targets: confirmed_plan.targets.clone(),
            blockers: vec![second.clone()],
        };
        assert!(close_authorization_covers(&one_exited, &confirmed_plan));
        assert!(close_authorization_covers(
            &ClosePlan {
                targets: vec![second.session_id],
                blockers: Vec::new(),
            },
            &confirmed_plan
        ));
    }

    #[test]
    fn process_name_normalization_is_safe_and_deterministic() {
        assert_eq!(normalize_process_name("  bash\n").as_deref(), Some("bash"));
        assert_eq!(normalize_process_name("tmux").as_deref(), Some("tmux"));
        assert_eq!(normalize_process_name("\n\t"), None);
        assert_eq!(normalize_process_name("vi\nm"), None);
        assert_eq!(normalize_process_name("bash\0"), None);
    }

    #[test]
    fn executable_identity_recovers_long_and_deleted_basenames() {
        let name = "core-terminal-command-name-over-fifteen-bytes";
        assert_eq!(
            executable_name_from_target(Path::new(&format!("/opt/core-terminal/bin/{name}")))
                .as_deref(),
            Some(name)
        );
        assert_eq!(
            executable_name_from_target(Path::new("/usr/bin/bash (deleted)")).as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn advisory_comm_identity_cannot_match_a_plain_exception() {
        let name = advisory_process_name_from_comm(b"bash\n").unwrap();
        assert_eq!(name, "bash (unverified)");

        let mut profile = TerminalProfile::homebrew();
        profile.ask_before_close_policy = AskBeforeClosePolicy::NonExempt;
        profile.ask_before_close_exceptions = vec!["bash".into()];
        assert!(should_prompt_before_close(&profile, Some(&name)));
    }

    #[test]
    fn process_name_candidates_reject_unsafe_or_unbounded_values() {
        assert_eq!(normalize_process_name(""), None);
        assert_eq!(normalize_process_name("/usr/bin/.."), None);
        assert_eq!(normalize_process_name("/usr/bin/vi\nm"), None);
        assert_eq!(advisory_process_name_from_comm(b"/usr/bin/\xff\n"), None);
        assert_eq!(
            advisory_process_name_from_comm(&[b'a'; MAX_PROCESS_NAME_BYTES + 1]),
            None
        );
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
