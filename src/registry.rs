//! The registry of managed repositories (`registry.toml`).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::paths::RepoId;

const VERSION: u32 = 1;
const DEFAULT_INTERVAL: Duration = Duration::from_secs(600);
const DEFAULT_QUIET_PERIOD: Duration = Duration::from_secs(30);
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_INTERVALS: u32 = 3;

/// Errors from reading, writing and editing the registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("cannot serialize the registry: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("unsupported registry version {0} (expected {VERSION})")]
    UnsupportedVersion(u32),
    #[error("repository {id}: invalid {key} {value:?}: {source}")]
    BadDuration {
        id: RepoId,
        key: &'static str,
        value: String,
        source: humantime::DurationError,
    },
    #[error(
        "repository {id}: the default stall threshold ({STALL_INTERVALS} x interval {interval:?}) \
         overflows; set stall_threshold explicitly"
    )]
    StallThresholdOverflow { id: RepoId, interval: Duration },
    #[error("cannot resolve {}: {source}", path.display())]
    UnresolvablePath { path: PathBuf, source: io::Error },
    #[error("repository {0} is already registered")]
    DuplicateId(RepoId),
    #[error("{} is already registered as {id}", path.display())]
    DuplicatePath { id: RepoId, path: PathBuf },
    #[error("no repository {0} is registered")]
    UnknownId(RepoId),
}

/// The whole registry file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(rename = "repo", default)]
    pub repos: Vec<RepoEntry>,
}

/// One registered repository. Durations are humantime strings, times
/// RFC 3339, `allowed_hooks` the digest recorded by the last allow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub id: RepoId,
    pub path: PathBuf,
    pub branch: String,
    pub remote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_period: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_threshold: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_hooks: Option<String>,
}

/// A repository's effective settings, with defaults applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub interval: Duration,
    pub quiet_period: Duration,
    pub git_timeout: Duration,
    pub lock_timeout: Duration,
    pub lock_name: Option<String>,
    pub stall_threshold: Duration,
}

impl RepoEntry {
    /// Parses the overrides and applies the defaults; the stall threshold
    /// defaults to three times this repository's own interval.
    pub fn settings(&self) -> Result<Settings, RegistryError> {
        let interval = self.duration("interval", &self.interval, DEFAULT_INTERVAL)?;
        Ok(Settings {
            interval,
            quiet_period: self.duration(
                "quiet_period",
                &self.quiet_period,
                DEFAULT_QUIET_PERIOD,
            )?,
            git_timeout: self.duration("git_timeout", &self.git_timeout, DEFAULT_GIT_TIMEOUT)?,
            lock_timeout: self.duration(
                "lock_timeout",
                &self.lock_timeout,
                DEFAULT_LOCK_TIMEOUT,
            )?,
            lock_name: self.lock_name.clone(),
            stall_threshold: self.stall_threshold(interval)?,
        })
    }

    /// The explicit override, else `STALL_INTERVALS` x `interval`; the
    /// default is computed only when needed and never overflows silently.
    fn stall_threshold(&self, interval: Duration) -> Result<Duration, RegistryError> {
        if let Some(value) = &self.stall_threshold {
            return self.parse_duration("stall_threshold", value);
        }
        interval
            .checked_mul(STALL_INTERVALS)
            .ok_or_else(|| RegistryError::StallThresholdOverflow {
                id: self.id.clone(),
                interval,
            })
    }

    fn duration(
        &self,
        key: &'static str,
        value: &Option<String>,
        default: Duration,
    ) -> Result<Duration, RegistryError> {
        match value {
            Some(value) => self.parse_duration(key, value),
            None => Ok(default),
        }
    }

    fn parse_duration(&self, key: &'static str, value: &str) -> Result<Duration, RegistryError> {
        humantime::parse_duration(value).map_err(|source| RegistryError::BadDuration {
            id: self.id.clone(),
            key,
            value: value.to_owned(),
            source,
        })
    }
}

impl Default for Registry {
    fn default() -> Registry {
        Registry {
            version: VERSION,
            repos: Vec::new(),
        }
    }
}

impl Registry {
    /// Reads the registry; a missing file is an empty registry. Fails on an
    /// unknown version or on any entry whose settings do not parse.
    pub fn load(path: &Path) -> Result<Registry, RegistryError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(source) => return Err(io_error(path, source)),
        };
        let registry: Registry = toml::from_str(&text).map_err(|source| RegistryError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if registry.version != VERSION {
            return Err(RegistryError::UnsupportedVersion(registry.version));
        }
        for entry in &registry.repos {
            entry.settings()?;
        }
        Ok(registry)
    }

    /// Writes the registry atomically.
    pub fn save(&self, path: &Path) -> Result<(), RegistryError> {
        let text = toml::to_string(self)?;
        write_atomic(path, text.as_bytes()).map_err(|source| io_error(path, source))
    }

    /// Locked read-modify-write: holds an exclusive lock on the sibling
    /// `<path>.lock` (never replaced, unlike the registry itself) across
    /// load, `f` and save. The lock is released when the file is dropped,
    /// on every path.
    pub fn update(
        path: &Path,
        f: impl FnOnce(&Registry) -> Result<Registry, RegistryError>,
    ) -> Result<Registry, RegistryError> {
        let lock_path = lock_path(path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let lock = File::create(&lock_path).map_err(|source| io_error(&lock_path, source))?;
        lock.lock().map_err(|source| io_error(&lock_path, source))?;
        let updated = f(&Registry::load(path)?)?;
        updated.save(path)?;
        drop(lock);
        Ok(updated)
    }

    pub fn get(&self, id: &RepoId) -> Option<&RepoEntry> {
        self.repos.iter().find(|e| &e.id == id)
    }

    /// A new registry with `entry` appended. The path must resolve (`add`
    /// registers existing checkouts only) and is stored canonical: a path's
    /// identity is its canonical path when it is registered. A duplicate id
    /// or an already registered path is refused.
    pub fn with_added(&self, entry: RepoEntry) -> Result<Registry, RegistryError> {
        let entry = RepoEntry {
            path: resolve(&entry.path)?,
            ..entry
        };
        check_unique(&self.repos, &entry)?;
        let repos = self.repos.iter().cloned().chain([entry]).collect();
        Ok(Registry {
            version: self.version,
            repos,
        })
    }

    /// A new registry without `id`; unknown ids are an error.
    pub fn without(&self, id: &RepoId) -> Result<Registry, RegistryError> {
        self.position(id)?;
        let repos = self.repos.iter().filter(|e| &e.id != id).cloned().collect();
        Ok(Registry {
            version: self.version,
            repos,
        })
    }

    /// A new registry with `id`'s entry replaced by `f(entry)`; the result
    /// must not collide with another entry's id or path. A changed path must
    /// resolve and is stored canonical; an unchanged one is kept as stored,
    /// so a moved checkout can still be paused or edited.
    pub fn with_entry(
        &self,
        id: &RepoId,
        f: impl FnOnce(&RepoEntry) -> RepoEntry,
    ) -> Result<Registry, RegistryError> {
        let index = self.position(id)?;
        let original = &self.repos[index];
        let edited = f(original);
        let edited = if edited.path == original.path {
            edited
        } else {
            RepoEntry {
                path: resolve(&edited.path)?,
                ..edited
            }
        };
        let others: Vec<RepoEntry> = self.without(id)?.repos;
        check_unique(&others, &edited)?;
        let repos = self
            .repos
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if i == index {
                    edited.clone()
                } else {
                    e.clone()
                }
            })
            .collect();
        Ok(Registry {
            version: self.version,
            repos,
        })
    }

    fn position(&self, id: &RepoId) -> Result<usize, RegistryError> {
        self.repos
            .iter()
            .position(|e| &e.id == id)
            .ok_or_else(|| RegistryError::UnknownId(id.clone()))
    }
}

/// Fails unless `entry` differs from every `existing` entry by id and by
/// stored (canonical) path.
fn check_unique(existing: &[RepoEntry], entry: &RepoEntry) -> Result<(), RegistryError> {
    for other in existing {
        if other.id == entry.id {
            return Err(RegistryError::DuplicateId(entry.id.clone()));
        }
        if other.path == entry.path {
            return Err(RegistryError::DuplicatePath {
                id: other.id.clone(),
                path: entry.path.clone(),
            });
        }
    }
    Ok(())
}

/// The canonical path of a path that must exist.
fn resolve(path: &Path) -> Result<PathBuf, RegistryError> {
    fs::canonicalize(path).map_err(|source| RegistryError::UnresolvablePath {
        path: path.to_path_buf(),
        source,
    })
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".lock");
    PathBuf::from(name)
}

fn io_error(path: &Path, source: io::Error) -> RegistryError {
    RegistryError::Io {
        path: path.to_path_buf(),
        source,
    }
}
