//! Saved SSH and SFTP launch records.
//!
//! Connection records deliberately contain only public connection metadata.
//! Authentication, host-key verification, and any interactive prompts remain
//! the responsibility of the installed OpenSSH client in the spawned PTY.

use crate::settings::APP_CONFIG_DIR;
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const CONNECTIONS_FILENAME: &str = "connections.json";
const MAX_CONNECTIONS_BYTES: usize = 1024 * 1024;
const MAX_FIELD_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Sftp,
}
impl Protocol {
    pub fn executable_name(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Sftp => "sftp",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SavedConnection {
    pub label: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub profile: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectionStore {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ConnectionError {
    #[error("{field} is required")]
    Required { field: &'static str },
    #[error("{field} contains an unsafe character")]
    Unsafe { field: &'static str },
    #[error("{field} is too long")]
    TooLong { field: &'static str },
    #[error("the port must be between 1 and 65535")]
    Port,
    #[error("a connection with this label already exists")]
    DuplicateLabel,
    #[error("an installed {0} client was not found on PATH")]
    MissingClient(&'static str),
    #[error("connection file is too large")]
    TooLarge,
    #[error("could not read connection file: {0}")]
    Io(String),
    #[error("invalid connection file: {0}")]
    Json(String),
}

impl SavedConnection {
    pub fn validate(&self) -> Result<(), ConnectionError> {
        validate_text(&self.label, "label", false)?;
        validate_text(&self.host, "host", true)?;
        validate_text(&self.user, "user", true)?;
        if self.port == 0 {
            return Err(ConnectionError::Port);
        }
        if !self.profile.trim().is_empty() {
            validate_text(&self.profile, "profile", false)?;
        }
        Ok(())
    }

    /// Construct direct argv, never shell source. `--` prevents a target that
    /// starts with a dash being reinterpreted as an OpenSSH option.
    pub fn argv(&self, executable: &Path) -> Result<Vec<String>, ConnectionError> {
        self.validate()?;
        let target = format!("{}@{}", self.user.trim(), self.host.trim());
        Ok(vec![
            executable.to_string_lossy().into_owned(),
            "-p".into(),
            self.port.to_string(),
            "--".into(),
            target,
        ])
    }
}

fn validate_text(
    value: &str,
    field: &'static str,
    reject_leading_dash: bool,
) -> Result<(), ConnectionError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ConnectionError::Required { field });
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(ConnectionError::TooLong { field });
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace())
        || (reject_leading_dash && value.starts_with('-'))
    {
        return Err(ConnectionError::Unsafe { field });
    }
    Ok(())
}

impl ConnectionStore {
    pub fn normalize(mut self) -> Self {
        self.connections
            .retain(|connection| connection.validate().is_ok());
        self.connections
            .sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        self.connections
            .dedup_by(|a, b| a.label.eq_ignore_ascii_case(&b.label));
        self
    }
    pub fn upsert(
        &mut self,
        connection: SavedConnection,
        replacing: Option<&str>,
    ) -> Result<(), ConnectionError> {
        connection.validate()?;
        if self.connections.iter().any(|existing| {
            existing.label.eq_ignore_ascii_case(&connection.label)
                && Some(existing.label.as_str()) != replacing
        }) {
            return Err(ConnectionError::DuplicateLabel);
        }
        if let Some(label) = replacing {
            self.connections.retain(|existing| existing.label != label);
        }
        self.connections.push(connection);
        *self = self.clone().normalize();
        Ok(())
    }
    pub fn delete(&mut self, label: &str) {
        self.connections
            .retain(|connection| connection.label != label);
    }
    pub fn config_path() -> Option<PathBuf> {
        let base = env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.trim().is_empty() && Path::new(v).is_absolute())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join(APP_CONFIG_DIR).join(CONNECTIONS_FILENAME))
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConnectionError> {
        let bytes = fs::read(path).map_err(|e| ConnectionError::Io(e.to_string()))?;
        if bytes.len() > MAX_CONNECTIONS_BYTES {
            return Err(ConnectionError::TooLarge);
        }
        serde_json::from_slice::<Self>(&bytes)
            .map(|store| store.normalize())
            .map_err(|e| ConnectionError::Json(e.to_string()))
    }
    pub fn load_user() -> Self {
        Self::config_path()
            .and_then(|path| Self::load(path).ok())
            .unwrap_or_default()
    }
    pub fn save_user(&self) -> Result<(), ConnectionError> {
        let path = Self::config_path()
            .ok_or_else(|| ConnectionError::Io("no user config directory is available".into()))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ConnectionError::Io(e.to_string()))?;
        }
        let contents = serde_json::to_vec_pretty(&self.clone().normalize())
            .map_err(|e| ConnectionError::Json(e.to_string()))?;
        fs::write(path, contents).map_err(|e| ConnectionError::Io(e.to_string()))
    }
}

pub fn resolve_client(protocol: Protocol) -> Result<PathBuf, ConnectionError> {
    resolve_client_in_path(protocol, env::var_os("PATH").as_deref())
}

pub fn resolve_client_in_path(
    protocol: Protocol,
    path: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, ConnectionError> {
    let name = protocol.executable_name();
    env::split_paths(path.unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or(ConnectionError::MissingClient(name))
}

/// Read plain `Host` aliases without writing or expanding `Include` files.
/// Wildcards and negated patterns are skipped because they are not single,
/// unambiguous saved connections.
pub fn read_ssh_host_aliases(path: impl AsRef<Path>) -> io::Result<Vec<String>> {
    let text = fs::read_to_string(path)?;
    let mut aliases = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, values)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if !key.eq_ignore_ascii_case("Host") {
            continue;
        }
        for alias in values.split_whitespace() {
            if !alias.starts_with('-')
                && !alias.starts_with('!')
                && !alias.contains('*')
                && !alias.contains('?')
                && validate_text(alias, "host", true).is_ok()
                && !aliases
                    .iter()
                    .any(|old: &String| old.eq_ignore_ascii_case(alias))
            {
                aliases.push(alias.to_owned());
            }
        }
    }
    Ok(aliases)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argv_is_direct_and_option_safe() {
        let connection = SavedConnection {
            label: "Build".into(),
            host: "nuc.lan".into(),
            user: "kasey".into(),
            port: 2222,
            protocol: Protocol::Ssh,
            profile: "Ocean".into(),
        };
        assert_eq!(
            connection.argv(Path::new("/usr/bin/ssh")).unwrap(),
            ["/usr/bin/ssh", "-p", "2222", "--", "kasey@nuc.lan"]
        );
        assert!(SavedConnection {
            host: "-oProxyCommand=x".into(),
            ..connection
        }
        .validate()
        .is_err());
    }
    #[test]
    fn controls_and_duplicate_labels_are_rejected() {
        let mut store = ConnectionStore::default();
        let one = SavedConnection {
            label: "NUC".into(),
            host: "host".into(),
            user: "user".into(),
            port: 22,
            protocol: Protocol::Sftp,
            profile: String::new(),
        };
        store.upsert(one.clone(), None).unwrap();
        assert_eq!(
            store.upsert(
                SavedConnection {
                    label: "nuc".into(),
                    ..one
                },
                None
            ),
            Err(ConnectionError::DuplicateLabel)
        );
        assert!(SavedConnection {
            label: "bad\nname".into(),
            ..store.connections[0].clone()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn persistence_round_trip_and_profile_fallback_field() {
        let path =
            std::env::temp_dir().join(format!("core-terminal-connections-{}", std::process::id()));
        let store = ConnectionStore {
            connections: vec![SavedConnection {
                label: "NUC".into(),
                host: "nuc".into(),
                user: "me".into(),
                port: 22,
                protocol: Protocol::Ssh,
                profile: String::new(),
            }],
        };
        fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
        assert_eq!(ConnectionStore::load(&path).unwrap(), store);
        let _ = fs::remove_file(path);
    }
    #[test]
    fn aliases_are_read_only_and_skip_patterns() {
        let path =
            std::env::temp_dir().join(format!("core-terminal-ssh-config-{}", std::process::id()));
        fs::write(&path, "Host nuc *.corp !skip\nHost build # note\n").unwrap();
        assert_eq!(read_ssh_host_aliases(&path).unwrap(), ["nuc", "build"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn client_resolution_uses_a_stub_path_and_reports_missing_client() {
        let directory =
            std::env::temp_dir().join(format!("core-terminal-client-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let client = directory.join("ssh");
        fs::write(&client, b"stub").unwrap();
        assert_eq!(
            resolve_client_in_path(Protocol::Ssh, Some(directory.as_os_str())).unwrap(),
            client
        );
        assert_eq!(
            resolve_client_in_path(Protocol::Sftp, Some(directory.as_os_str())),
            Err(ConnectionError::MissingClient("sftp"))
        );
        let _ = fs::remove_file(client);
        let _ = fs::remove_dir(directory);
    }
}
