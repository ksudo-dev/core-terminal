//! Opt-in layout and plain text restore. Commands and processes are never saved.
use crate::{profiles::read_bounded_text_file, settings::Settings};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    io::AsRawFd,
};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};
pub const VERSION: u32 = 1;
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_WINDOWS: usize = 16;
pub const MAX_TABS: usize = 64;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_LINES: usize = 10_000;
const MAX_TOTAL_TEXT_BYTES: usize = 3 * 1024 * 1024;
const MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
const SESSION_FILE_NAME: &str = "session.json";
const LOCK_FILE_NAME: &str = ".session-restore.lock";
const TEMP_PREFIX: &str = ".session-";
const TEMP_SUFFIX: &str = ".tmp";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static STATE_LOCK: Mutex<()> = Mutex::new(());
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSnapshot {
    pub profile: String,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub transcript: String,
}
// Window layout
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSnapshot {
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default = "default_width")]
    pub width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
    pub tabs: Vec<TabSnapshot>,
}
fn default_width() -> i32 {
    1120
}
fn default_height() -> i32 {
    720
}
impl Default for WindowSnapshot {
    fn default() -> Self {
        Self {
            active_tab: 0,
            width: default_width(),
            height: default_height(),
            maximized: false,
            tabs: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub saved_at: u64,
    #[serde(default)]
    pub active_window: usize,
    pub windows: Vec<WindowSnapshot>,
}
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("session state I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid session JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported session version")]
    Version,
    #[error("saved session expired")]
    Expired,
    #[error("session exceeds its safe limit")]
    TooLarge,
    #[error("invalid session state: {0}")]
    Invalid(&'static str),
    #[error("session path unavailable")]
    NoPath,
}
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn state_path() -> Option<PathBuf> {
    let config = Settings::config_path()?;
    let parent = config.parent()?;
    parent
        .is_absolute()
        .then(|| parent.join("session-restore").join("session.json"))
}
impl Snapshot {
    pub fn new(windows: Vec<WindowSnapshot>, active_window: usize) -> Self {
        Self {
            version: VERSION,
            saved_at: now_secs(),
            active_window,
            windows,
        }
    }
    pub fn normalized(mut self) -> Result<Self, RestoreError> {
        if self.version != VERSION {
            return Err(RestoreError::Version);
        }
        if self.windows.len() > MAX_WINDOWS {
            return Err(RestoreError::TooLarge);
        }
        let count = self.windows.iter().map(|w| w.tabs.len()).sum::<usize>();
        if count > MAX_TABS {
            return Err(RestoreError::TooLarge);
        }
        let budget = MAX_TEXT_BYTES.min(MAX_TOTAL_TEXT_BYTES / count.max(1));
        self.active_window = self.active_window.min(self.windows.len().saturating_sub(1));
        for window in &mut self.windows {
            window.width = window.width.clamp(320, 8000);
            window.height = window.height.clamp(240, 8000);
            if window.tabs.is_empty() {
                return Err(RestoreError::Invalid("empty window"));
            }
            window.active_tab = window.active_tab.min(window.tabs.len() - 1);
            for tab in &mut window.tabs {
                if tab.profile.is_empty()
                    || tab.profile.len() > 256
                    || tab.profile.chars().any(char::is_control)
                {
                    return Err(RestoreError::Invalid("profile name"));
                }
                if tab
                    .working_directory
                    .as_deref()
                    .is_some_and(|p| !valid_directory(p))
                {
                    tab.working_directory = None;
                }
                tab.transcript = sanitize_text(&tab.transcript, budget, MAX_TEXT_LINES);
            }
        }
        Ok(self)
    }
}
pub fn load(path: &Path, now: u64) -> Result<Snapshot, RestoreError> {
    let text = read_bounded_text_file(path, MAX_FILE_BYTES + 1)?;
    if text.len() > MAX_FILE_BYTES {
        return Err(RestoreError::TooLarge);
    }
    let snapshot: Snapshot = serde_json::from_str(&text)?;
    if snapshot.version != VERSION {
        return Err(RestoreError::Version);
    }
    if snapshot.saved_at > now.saturating_add(300) {
        return Err(RestoreError::Invalid("future timestamp"));
    }
    if now.saturating_sub(snapshot.saved_at) > MAX_AGE_SECS {
        return Err(RestoreError::Expired);
    }
    snapshot.normalized()
}
pub fn load_user() -> Result<Snapshot, RestoreError> {
    load(&state_path().ok_or(RestoreError::NoPath)?, now_secs())
}
pub fn save(path: &Path, snapshot: &Snapshot) -> Result<(), RestoreError> {
    let mut snapshot = snapshot.clone().normalized()?;
    snapshot.saved_at = now_secs();
    let bytes = serde_json::to_vec(&snapshot)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(RestoreError::TooLarge);
    }
    with_state_lock(path, |parent| atomic_write_private(path, parent, &bytes))?;
    Ok(())
}
pub fn save_user(snapshot: &Snapshot) -> Result<(), RestoreError> {
    save(&state_path().ok_or(RestoreError::NoPath)?, snapshot)
}
pub fn clear(path: &Path) -> Result<(), RestoreError> {
    with_state_lock(path, |parent| {
        remove_session_file(path)?;
        cleanup_interrupted_temporary_files(parent)
    })?;
    Ok(())
}
pub fn clear_user() -> Result<(), RestoreError> {
    clear(&state_path().ok_or(RestoreError::NoPath)?)
}
fn valid_directory(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && Path::new(p).is_absolute()
        && !p.chars().any(char::is_control)
}
pub fn directory_or_home(saved: Option<&str>, home: Option<&str>) -> Option<String> {
    saved
        .into_iter()
        .chain(home)
        .chain(std::iter::once("/"))
        .find(|p| valid_directory(p) && Path::new(p).is_dir())
        .map(str::to_owned)
}
#[derive(Clone, Copy)]
enum TextState {
    Ground,
    Escape,
    Intermediate,
    Csi,
    String(bool),
    StringEscape(bool),
}
pub fn sanitize_text(input: &str, byte_limit: usize, line_limit: usize) -> String {
    let byte_limit = byte_limit.min(MAX_TEXT_BYTES);
    let line_limit = line_limit.min(MAX_TEXT_LINES);
    if byte_limit == 0 || line_limit == 0 {
        return String::new();
    }
    let mut state = TextState::Ground;
    let mut output = VecDeque::<char>::new();
    let mut bytes = 0usize;
    let mut newlines = 0usize;
    for character in input.chars() {
        let printable = match state {
            TextState::Ground => match character {
                '\u{1b}' => {
                    state = TextState::Escape;
                    None
                }
                '\u{9b}' => {
                    state = TextState::Csi;
                    None
                }
                '\u{9d}' => {
                    state = TextState::String(true);
                    None
                }
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                    state = TextState::String(false);
                    None
                }
                '\n' | '\t' => Some(character),
                v if !v.is_control() => Some(v),
                _ => None,
            },
            TextState::Escape => {
                state = match character {
                    '[' => TextState::Csi,
                    ']' => TextState::String(true),
                    'P' | 'X' | '^' | '_' => TextState::String(false),
                    '\u{1b}' => TextState::Escape,
                    '\u{20}'..='\u{2f}' => TextState::Intermediate,
                    _ => TextState::Ground,
                };
                None
            }
            TextState::Intermediate => {
                if character == '\u{1b}' {
                    state = TextState::Escape;
                } else if ('\u{30}'..='\u{7e}').contains(&character) {
                    state = TextState::Ground;
                }
                None
            }
            TextState::Csi => {
                if character == '\u{1b}' {
                    state = TextState::Escape;
                } else if ('@'..='~').contains(&character) {
                    state = TextState::Ground;
                }
                None
            }
            TextState::String(bell) => {
                if character == '\u{9c}' || (bell && character == '\u{7}') {
                    state = TextState::Ground;
                } else if character == '\u{1b}' {
                    state = TextState::StringEscape(bell);
                }
                None
            }
            TextState::StringEscape(bell) => {
                state =
                    if character == '\\' || character == '\u{9c}' || (bell && character == '\u{7}')
                    {
                        TextState::Ground
                    } else if character == '\u{1b}' {
                        TextState::StringEscape(bell)
                    } else {
                        TextState::String(bell)
                    };
                None
            }
        };
        let Some(c) = printable else {
            continue;
        };
        output.push_back(c);
        bytes += c.len_utf8();
        newlines += usize::from(c == '\n');
        while bytes > byte_limit {
            let c = output.pop_front().expect("nonempty bounded output");
            bytes -= c.len_utf8();
            newlines -= usize::from(c == '\n');
        }
        while newlines + usize::from(!output.is_empty() && output.back() != Some(&'\n'))
            > line_limit
        {
            while let Some(c) = output.pop_front() {
                bytes -= c.len_utf8();
                if c == '\n' {
                    newlines -= 1;
                    break;
                }
            }
        }
    }
    output.into_iter().collect()
}
// Display-only bytes for VTE feed, never feed_child.
pub fn display_bytes(input: &str) -> Vec<u8> {
    let plain = sanitize_text(input, MAX_TEXT_BYTES, MAX_TEXT_LINES);
    if plain.is_empty() {
        return Vec::new();
    }
    let mut output = Vec::with_capacity(plain.len() + 2);
    for b in plain.bytes() {
        if b == b'\n' {
            output.push(b'\r');
        }
        output.push(b);
    }
    if !plain.ends_with('\n') {
        output.extend_from_slice(b"\r\n");
    }
    output
}
struct StateLock {
    _process_guard: MutexGuard<'static, ()>,
    #[cfg(unix)]
    file: File,
}

impl Drop for StateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        // The descriptor remains open until after this explicit unlock. Keep the
        // lock file itself: unlinking it would permit an inode-race bypass.
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn state_parent(path: &Path) -> io::Result<&Path> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    if path.file_name().and_then(|name| name.to_str()) != Some(SESSION_FILE_NAME) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected session state filename",
        ));
    }
    Ok(parent)
}

fn prepare_state_directory(path: &Path) -> io::Result<&Path> {
    let parent = state_parent(path)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(parent)?;
    if !fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state directory is not a directory",
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(parent)
}

fn acquire_state_lock(parent: &Path) -> io::Result<StateLock> {
    let process_guard = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = parent.join(LOCK_FILE_NAME);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session lock is not a regular file",
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session lock is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        // SAFETY: `file` stays open in StateLock until the matching unlock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(StateLock {
        _process_guard: process_guard,
        #[cfg(unix)]
        file,
    })
}

fn with_state_lock<T>(
    path: &Path,
    operation: impl FnOnce(&Path) -> io::Result<T>,
) -> io::Result<T> {
    let parent = prepare_state_directory(path)?;
    let _lock = acquire_state_lock(parent)?;
    operation(parent)
}

fn session_path_is_safe(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session state must not be a symlink",
        )),
        Ok(metadata) if !metadata.file_type().is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session state is not a regular file",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_interrupted_temporary_file(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(numbers) = name
        .strip_prefix(TEMP_PREFIX)
        .and_then(|value| value.strip_suffix(TEMP_SUFFIX))
    else {
        return false;
    };
    let mut parts = numbers.split('-');
    parts.by_ref().take(3).count() == 3
        && parts.next().is_none()
        && numbers
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn cleanup_interrupted_temporary_files(parent: &Path) -> io::Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !is_interrupted_temporary_file(&entry.file_name()) {
            continue;
        }
        // DirEntry::file_type does not follow links. Never unlink a link even
        // when its name resembles an interrupted Core Terminal write.
        if entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn remove_session_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session state must not be a symlink",
        )),
        Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session state is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn atomic_write_private(path: &Path, parent: &Path, content: &[u8]) -> io::Result<()> {
    session_path_is_safe(path)?;
    cleanup_interrupted_temporary_files(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".session-{}-{nonce}-{sequence}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    let result = (|| -> io::Result<()> {
        file.write_all(content)?;
        file.sync_all()?;
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        drop(file);
        session_path_is_safe(path)?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct TemporaryState {
        root: PathBuf,
    }

    impl TemporaryState {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "core-terminal-{label}-{}-{}",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self { root }
        }

        fn session(&self) -> PathBuf {
            self.root.join(SESSION_FILE_NAME)
        }
    }

    impl Drop for TemporaryState {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn sample() -> Snapshot {
        Snapshot::new(
            vec![WindowSnapshot {
                tabs: vec![TabSnapshot {
                    profile: "Homebrew".into(),
                    transcript: "old\nnew\n".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            0,
        )
    }
    #[test]
    fn strips_protocol_and_bounds_unicode_tail() {
        let raw="a\x1b[31mRED\x1b[0m\x1b]52;c;secret\x07\x1bPpayload\x1b\\\u{9d}hidden\u{9c}\x1b(B\0\rb\t\n";
        assert_eq!(sanitize_text(raw, 100, 10), "aREDb\t\n");
        assert_eq!(
            sanitize_text("\u{3b1}\u{3b2}\u{3b3}", 5, 10),
            "\u{3b2}\u{3b3}"
        );
        assert_eq!(sanitize_text("a\nb\nc\n", 100, 2), "b\nc\n");
        assert_eq!(
            sanitize_text("visible\x1b]unterminated", 100, 10),
            "visible"
        );
        assert_eq!(display_bytes("a\x1b[6n\nb"), b"a\r\nb\r\n");
    }
    #[test]
    fn bounds_layout_and_rejects_commands() {
        let mut s = sample();
        s.active_window = 99;
        s.windows[0].active_tab = 99;
        s.windows[0].width = -1;
        s.windows[0].height = i32::MAX;
        s.windows[0].tabs[0].working_directory = Some("relative".into());
        let n = s.normalized().unwrap();
        assert_eq!(n.active_window, 0);
        assert_eq!(n.windows[0].active_tab, 0);
        assert_eq!((n.windows[0].width, n.windows[0].height), (320, 8000));
        assert!(n.windows[0].tabs[0].working_directory.is_none());
        let mut json = serde_json::to_value(&n).unwrap();
        json["windows"][0]["tabs"][0]["command"] = "touch marker".into();
        assert!(serde_json::from_value::<Snapshot>(json).is_err());
        let mut s = sample();
        s.windows[0].tabs = vec![TabSnapshot::default(); MAX_TABS + 1];
        assert!(matches!(s.normalized(), Err(RestoreError::TooLarge)));
    }
    #[test]
    fn private_atomic_roundtrip_and_recovery() {
        let root = std::env::temp_dir().join(format!(
            "core-session-test-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("session.json");
        assert!(load(&path, now_secs()).is_err());
        let original = sample();
        save(&path, &original).unwrap();
        let loaded = load(&path, now_secs()).unwrap();
        assert_eq!(loaded.windows, original.windows);
        assert_eq!(
            directory_or_home(Some("relative"), root.to_str()),
            root.to_str().map(str::to_owned)
        );
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let before = fs::read(&path).unwrap();
        let mut invalid = original.clone();
        invalid.windows[0].tabs[0].profile.clear();
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        invalid.windows[0].tabs = vec![TabSnapshot::default(); MAX_TABS + 1];
        assert!(matches!(save(&path, &invalid), Err(RestoreError::TooLarge)));
        assert_eq!(fs::read(&path).unwrap(), before);
        save(&path, &original).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert!(matches!(
            load(&path, loaded.saved_at + MAX_AGE_SECS + 2),
            Err(RestoreError::Expired)
        ));
        let mut wrong = sample();
        wrong.version += 1;
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(matches!(
            load(&path, now_secs()),
            Err(RestoreError::Version)
        ));
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            load(&path, now_secs()),
            Err(RestoreError::Json(_))
        ));
        fs::write(&path, vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        assert!(matches!(
            load(&path, now_secs()),
            Err(RestoreError::TooLarge)
        ));
        let mut tainted = original.clone();
        tainted.windows[0].tabs[0].transcript =
            "safe\x1b[6n\x1b]52;c;secret\x07\u{9b}31m\0\n".into();
        fs::write(&path, serde_json::to_vec(&tainted).unwrap()).unwrap();
        assert_eq!(
            load(&path, now_secs()).unwrap().windows[0].tabs[0].transcript,
            "safe\n"
        );
        #[cfg(unix)]
        {
            let link = root.join("alias");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            let before = fs::read(&path).unwrap();
            assert!(save(&link.join("session.json"), &original).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
            fs::remove_file(link).unwrap();
        }
    }

    #[test]
    fn save_cleans_only_interrupted_application_temporary_files() {
        let state = TemporaryState::new("interrupted-session");
        let path = state.session();
        let interrupted = state.root.join(".session-10-20-30.tmp");
        let malformed = state.root.join(".session-10-20.tmp");
        let unrelated = state.root.join("snapshot.json");
        fs::write(&interrupted, b"partial").unwrap();
        fs::write(&malformed, b"preserve").unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        save(&path, &sample()).unwrap();

        assert!(!interrupted.exists());
        assert_eq!(fs::read(&malformed).unwrap(), b"preserve");
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        assert!(path.is_file());
        assert!(state.root.join(LOCK_FILE_NAME).is_file());
    }

    #[test]
    fn clear_is_opt_out_and_preserves_unrelated_state() {
        let state = TemporaryState::new("clear-session");
        let path = state.session();
        let interrupted = state.root.join(".session-10-20-30.tmp");
        let unrelated = state.root.join("other-snapshot.json");
        save(&path, &sample()).unwrap();
        fs::write(&interrupted, b"partial").unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        clear(&path).unwrap();

        assert!(!path.exists());
        assert!(!interrupted.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        assert!(state.root.join(LOCK_FILE_NAME).is_file());
    }

    #[test]
    fn concurrent_save_and_clear_leave_no_recognized_remnant() {
        let state = TemporaryState::new("concurrent-session");
        let path = Arc::new(state.session());
        let barrier = Arc::new(Barrier::new(3));
        let saver_path = Arc::clone(&path);
        let saver_barrier = Arc::clone(&barrier);
        let saver = std::thread::spawn(move || {
            saver_barrier.wait();
            for _ in 0..32 {
                save(&saver_path, &sample()).unwrap();
            }
        });
        let clearer_path = Arc::clone(&path);
        let clearer_barrier = Arc::clone(&barrier);
        let clearer = std::thread::spawn(move || {
            clearer_barrier.wait();
            for _ in 0..32 {
                clear(&clearer_path).unwrap();
            }
        });
        barrier.wait();
        saver.join().unwrap();
        clearer.join().unwrap();

        if path.exists() {
            assert!(load(&path, now_secs()).is_ok());
        }
        let entries = fs::read_dir(&state.root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(entries
            .iter()
            .all(|entry| !is_interrupted_temporary_file(&entry.file_name())));
        assert!(state.root.join(LOCK_FILE_NAME).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn clear_preserves_symlinked_and_unrelated_files() {
        let state = TemporaryState::new("symlink-preserve");
        let path = state.session();
        let target = state.root.join("unrelated-target");
        let temporary_link = state.root.join(".session-10-20-30.tmp");
        let unrelated = state.root.join("other-snapshot.json");
        fs::write(&target, b"outside").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        save(&path, &sample()).unwrap();
        std::os::unix::fs::symlink(&target, &temporary_link).unwrap();

        clear(&path).unwrap();

        assert!(temporary_link.is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"outside");
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn save_and_clear_reject_a_symlinked_session_file() {
        let state = TemporaryState::new("symlink-session");
        let path = state.session();
        let target = state.root.join("unrelated-target");
        fs::write(&target, b"outside").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(save(&path, &sample()).is_err());
        assert!(clear(&path).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"outside");
        assert!(path.is_symlink());
    }
    #[test]
    fn limits_aggregate_text_and_empty_windows() {
        let mut s = sample();
        s.windows[0].tabs.clear();
        assert!(s.normalized().is_err());
        let mut s = sample();
        s.windows = vec![WindowSnapshot::default(); MAX_WINDOWS + 1];
        assert!(matches!(s.normalized(), Err(RestoreError::TooLarge)));
        let mut s = sample();
        s.windows[0].tabs = vec![
            TabSnapshot {
                profile: "Homebrew".into(),
                transcript: "x".repeat(MAX_TEXT_BYTES + 16),
                ..Default::default()
            };
            MAX_TABS
        ];
        let s = s.normalized().unwrap();
        assert!(s.windows[0].tabs.iter().all(|t| !t.transcript.is_empty()));
        assert!(
            s.windows[0]
                .tabs
                .iter()
                .map(|t| t.transcript.len())
                .sum::<usize>()
                <= MAX_TOTAL_TEXT_BYTES
        );
    }
}
