//! The `.autosync/` hook digest that explicit trust ("allow") records.

use std::ffi::{CString, OsString};
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const AUTOSYNC_DIR: &str = ".autosync";
const DOMAIN: &[u8] = b"rsrsbar .autosync digest v1";
/// The mode bits that make a file executable, for the digest and for hooks.
pub(crate) const EXEC_BITS: u32 = 0o111;
const MODE_BITS: u32 = 0o7777;
const STAGING_PREFIX: &str = ".allowed-hooks-";
/// Link hops followed while resolving one symlink (macOS `MAXSYMLINKS`).
const MAX_LINK_HOPS: usize = 32;

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
        .filter(|phase| is_executable_file(&root.join(phase.file_name())))
        .collect()
}

/// Whether `path`, following symlinks, is a file with an executable bit:
/// what git, and `run-hook`, treat as a hook.
pub(crate) fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & EXEC_BITS != 0)
}

/// Replaces `dest` with a copy of `<repo>/.autosync/` — files with their
/// modes, symlinks as symlinks (never followed), directories recursively —
/// and returns [`hooks_digest`] of the copy laid out as `.autosync/`, which
/// equals the source's for a tree not modified while it is copied. The copy
/// is built in a temporary sibling of `dest` and swapped in atomically
/// (`renamex_np` with `RENAME_SWAP`; a plain rename the first time), so
/// `dest` is never partial or missing once it exists. A missing
/// `.autosync/` gives an empty copy.
///
/// Refused (`InvalidInput`), because the digest covers a symlink's target
/// string, not what it points to, so the copy would run content that was
/// never allowed: a `.autosync` that is not a real directory, an entry that
/// is neither file, symlink nor directory, and a symlink whose target is
/// absolute or resolves — lexically, following links inside the tree —
/// outside `.autosync/`. Relative links within the tree are kept.
pub fn snapshot_hooks(repo: &Path, dest: &Path) -> io::Result<String> {
    let (staging, digest) = stage(repo, dest)?;
    replace_dir(staging, dest)?;
    Ok(digest)
}

/// [`snapshot_hooks`], but the staged copy replaces `dest` only if its
/// digest is `expected`; otherwise `dest` is left untouched and the result
/// is `Ok(false)`. Binds an allow to the content a preview showed.
pub fn snapshot_hooks_matching(repo: &Path, dest: &Path, expected: &str) -> io::Result<bool> {
    let (staging, digest) = stage(repo, dest)?;
    if digest != expected {
        return Ok(false);
    }
    replace_dir(staging, dest)?;
    Ok(true)
}

/// Copies `.autosync/` into a temporary sibling of `dest` and digests it.
fn stage(repo: &Path, dest: &Path) -> io::Result<(TempDir, String)> {
    let root = repo.join(AUTOSYNC_DIR);
    let parent = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent)?;
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(meta) if meta.is_dir() => copy_tree(&root, &root, staging.path())?,
        Ok(_) => return Err(not_copyable(&root, "is not a directory")),
    }
    let digest = hash(sorted(collect(staging.path(), Path::new(""))?));
    Ok((staging, digest))
}

/// [`hooks_digest`] of a snapshot made by [`snapshot_hooks`], laid out as
/// `.autosync/`: what `run-hook` compares with the allowed digest before it
/// executes anything from the snapshot. A missing snapshot has the empty
/// digest (the same as a missing `.autosync/`); anything but a real
/// directory at `dest` is an error.
pub fn snapshot_digest(dest: &Path) -> io::Result<String> {
    match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(hash(Vec::new())),
        Err(e) => Err(e),
        Ok(meta) if meta.is_dir() => Ok(hash(sorted(collect(dest, Path::new(""))?))),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a directory", dest.display()),
        )),
    }
}

fn not_copyable(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} {why}", path.display()),
    )
}

fn copy_tree(root: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    for item in fs::read_dir(src)? {
        let path = item?.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = dst.join(name);
        let meta = fs::symlink_metadata(&path)?;
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            check_internal_link(root, &path)?;
            symlink(fs::read_link(&path)?, &target)?;
            continue;
        }
        if file_type.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(root, &path, &target)?;
        } else if file_type.is_file() {
            fs::copy(&path, &target)?;
        } else {
            return Err(not_copyable(&path, "is not a file, symlink or directory"));
        }
        // After the recursion, so a read-only directory can still be filled.
        let mode = meta.permissions().mode() & MODE_BITS;
        fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// One step of a path being resolved below the tree root.
enum Step {
    Up,
    Down(OsString),
}

/// The steps of a symlink target; `None` for an absolute target.
fn steps(target: &Path) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    for component in target.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => return None,
            Component::CurDir => {}
            Component::ParentDir => steps.push(Step::Up),
            Component::Normal(name) => steps.push(Step::Down(name.to_os_string())),
        }
    }
    Some(steps)
}

/// Fails unless the symlink `link` (inside `root`) resolves to a path
/// inside `root`: `..` past the root, an absolute target anywhere in the
/// chain, or more than [`MAX_LINK_HOPS`] links is a refusal.
fn check_internal_link(root: &Path, link: &Path) -> io::Result<()> {
    let leaves = |target: &Path| {
        not_copyable(
            link,
            &format!("links outside .autosync/ (via {})", target.display()),
        )
    };
    let relative = link.strip_prefix(root).map_err(|_| leaves(link))?;
    let mut at: Vec<OsString> = relative
        .parent()
        .map(|dir| dir.iter().map(OsString::from).collect())
        .unwrap_or_default();
    let target = fs::read_link(link)?;
    let mut todo = steps(&target).ok_or_else(|| leaves(&target))?;
    todo.reverse();
    let mut hops = 0;
    while let Some(step) = todo.pop() {
        let name = match step {
            Step::Up => {
                at.pop().ok_or_else(|| leaves(&target))?;
                continue;
            }
            Step::Down(name) => name,
        };
        let here: PathBuf = root.join(at.iter().collect::<PathBuf>()).join(&name);
        if !fs::symlink_metadata(&here).is_ok_and(|m| m.file_type().is_symlink()) {
            at.push(name);
            continue;
        }
        hops += 1;
        let next = fs::read_link(&here)?;
        let next_steps = steps(&next).filter(|_| hops <= MAX_LINK_HOPS);
        todo.extend(next_steps.ok_or_else(|| leaves(&next))?.into_iter().rev());
    }
    Ok(())
}

/// Puts `staging` in place of `dest`. An existing `dest` is exchanged with
/// it atomically, so a reader or a crash sees either the old or the new
/// copy, never none; the old copy then sits at the staging path and is
/// removed with it.
fn replace_dir(staging: TempDir, dest: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::rename(staging.path(), dest)?;
            // The staging path is now `dest`; nothing is left to remove.
            let _ = staging.keep();
            Ok(())
        }
        Err(e) => Err(e),
        Ok(_) => swap(staging.path(), dest),
    }
}

fn swap(a: &Path, b: &Path) -> io::Result<()> {
    let c_path = |p: &Path| {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| not_copyable(p, "contains a NUL byte"))
    };
    let (a, b) = (c_path(a)?, c_path(b)?);
    // SAFETY: both pointers come from live, NUL-terminated CStrings.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
    Ok(fs::read_link(path)?.into_os_string().into_vec())
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
