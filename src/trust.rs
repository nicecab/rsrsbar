//! The `.autosync/` hook digest that explicit trust ("allow") records.

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const AUTOSYNC_DIR: &str = ".autosync";
const DOMAIN: &[u8] = b"rsrsbar .autosync digest v1";
const EXEC_BITS: u32 = 0o111;

/// A git-rsrs hook phase that a repository may provide under `.autosync/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    PreCommit,
    PostIntegrate,
    OnResult,
}

impl Phase {
    pub const ALL: [Phase; 3] = [Phase::PreCommit, Phase::PostIntegrate, Phase::OnResult];

    /// The hook's file name under `.autosync/`.
    pub fn file_name(self) -> &'static str {
        match self {
            Phase::PreCommit => "pre-commit",
            Phase::PostIntegrate => "post-integrate",
            Phase::OnResult => "on-result",
        }
    }

    /// The phase's table name in the git-rsrs config.
    pub fn config_key(self) -> &'static str {
        match self {
            Phase::PreCommit => "pre_commit",
            Phase::PostIntegrate => "post_integrate",
            Phase::OnResult => "on_result",
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    File = 1,
    Symlink = 2,
    Other = 3,
}

struct Entry {
    path: Vec<u8>,
    kind: Kind,
    executable: bool,
    content: Vec<u8>,
}

/// `"sha256:<hex>"` over every file and symlink under `<repo>/.autosync/`,
/// recursively, sorted by relative path bytes. Each entry contributes its
/// relative path, its type, the executable bit (any of `0o111`, files only)
/// and its content — a symlink's target string, never followed. Every field
/// is length-prefixed. Directories are traversed but are not entries, so an
/// empty subdirectory does not change the digest. A missing or empty
/// `.autosync/` has one fixed digest; a `.autosync` that is itself a symlink
/// or a file is hashed as a single entry named `.`.
///
/// Symlinks are never followed only for a tree that is not modified while it
/// is scanned: each entry's type is checked and then read by path, so a
/// concurrent process swapping an entry for a symlink in between can make the
/// scan follow it. The trust model covers content arriving through pulled
/// commits, not a local process racing the scan.
pub fn hooks_digest(repo: &Path) -> io::Result<String> {
    let root = repo.join(AUTOSYNC_DIR);
    let entries = match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
        Ok(meta) if meta.is_dir() => collect(&root, Path::new(""))?,
        Ok(meta) => vec![entry(&root, Path::new("."), &meta)?],
    };
    Ok(hash(sorted(entries)))
}

/// Phases whose hook exists under a real `.autosync/` directory and is
/// executable — what `run-hook` would execute. A hook that is a symlink
/// counts when its target is an executable file; `.autosync` itself being a
/// symlink yields no hooks.
pub fn present_hooks(repo: &Path) -> Vec<Phase> {
    let root = repo.join(AUTOSYNC_DIR);
    let is_real_dir = fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir());
    if !is_real_dir {
        return Vec::new();
    }
    Phase::ALL
        .into_iter()
        .filter(|phase| {
            fs::metadata(root.join(phase.file_name()))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & EXEC_BITS != 0)
        })
        .collect()
}

fn collect(dir: &Path, prefix: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        let item = item?;
        let path = item.path();
        let relative = prefix.join(item.file_name());
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            entries.extend(collect(&path, &relative)?);
        } else {
            entries.push(entry(&path, &relative, &meta)?);
        }
    }
    Ok(entries)
}

fn entry(path: &Path, relative: &Path, meta: &Metadata) -> io::Result<Entry> {
    let file_type = meta.file_type();
    let (kind, content) = if file_type.is_symlink() {
        (Kind::Symlink, link_target(path)?)
    } else if file_type.is_file() {
        (Kind::File, fs::read(path)?)
    } else {
        (Kind::Other, Vec::new())
    };
    Ok(Entry {
        path: relative.as_os_str().as_bytes().to_vec(),
        kind,
        executable: file_type.is_file() && meta.permissions().mode() & EXEC_BITS != 0,
        content,
    })
}

fn link_target(path: &Path) -> io::Result<Vec<u8>> {
    let target: PathBuf = fs::read_link(path)?;
    Ok(target.as_os_str().as_bytes().to_vec())
}

fn sorted(entries: Vec<Entry>) -> Vec<Entry> {
    let mut entries = entries;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn hash(entries: Vec<Entry>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    for e in &entries {
        for field in [
            e.path.as_slice(),
            &[e.kind as u8],
            &[u8::from(e.executable)],
            &e.content,
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field);
        }
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}
