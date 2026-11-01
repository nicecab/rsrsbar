//! `allow`'s preview: what changed in `.autosync/` since the last allow,
//! bound to the digest of exactly what was shown.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::allow::snapshot_refusal;
use super::lifecycle::registered;
use super::{CommandError, Ctx, io_error};
use crate::trust::{snapshot_digest, snapshot_hooks};

const DIFF: &str = "/usr/bin/diff";
const NO_CHANGES: &str = "no changes since the last allow";
const NOT_ALLOWED_YET: &str = "note: no allow recorded yet; every hook is shown as new";
const UNVERIFIED: &str = "note: the allowed snapshot does not match the recorded allow; \
                          every hook is shown as new";
/// `diff` exit status when the trees differ.
const DIFF_DIFFERENT: i32 = 1;
const EXEC_BITS: u32 = 0o111;
const PERMISSION_BITS: u32 = 0o777;

/// The preview text and the digest of the hooks it shows; `allow` refuses
/// unless what it snapshots still has this digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub text: String,
    pub digest: String,
}

/// Snapshots the checkout's `.autosync/` into a temporary copy and compares
/// it with the last allow: mode, file-type and symlink changes (which the
/// digest covers but a content diff does not show) followed by
/// `diff -ruN --no-dereference`. "No changes since the last allow" only
/// when the copy's digest equals the RECORDED digest. The snapshot on disk
/// stands for the last allow only while its digest equals the recorded one;
/// otherwise every hook is shown as new, with a note saying why.
pub fn allow_preview(ctx: &Ctx, id: &str) -> Result<Preview, CommandError> {
    let entry = registered(ctx, id)?;
    let scratch = tempfile::tempdir().map_err(io_error(Path::new("temporary directory")))?;
    let shown = scratch.path().join("autosync");
    let digest = snapshot_hooks(&entry.path, &shown)
        .map_err(|err| snapshot_refusal(err, &entry.id, &shown))?;
    let recorded = entry.allowed_hooks.as_deref();
    if recorded == Some(digest.as_str()) {
        return Ok(Preview {
            text: NO_CHANGES.to_owned(),
            digest,
        });
    }
    let allowed = ctx.paths.allowed_hooks_dir(&entry.id);
    let on_disk = snapshot_digest(&allowed).ok();
    let verified = recorded.is_some() && on_disk.as_deref() == recorded;
    let note = match (recorded, allowed.exists() && !verified) {
        (None, _) => Some(NOT_ALLOWED_YET.to_owned()),
        (Some(_), true) => Some(UNVERIFIED.to_owned()),
        (Some(_), false) => None,
    };
    let old = if verified && allowed.is_dir() {
        allowed
    } else {
        scratch.path().join("none")
    };
    let lines: Vec<String> = note
        .into_iter()
        .chain(metadata_changes(&old, &shown)?)
        .chain(content_diff(&old, &shown)?)
        .collect();
    Ok(Preview {
        text: lines.join("\n"),
        digest,
    })
}

fn content_diff(old: &Path, new: &Path) -> Result<Option<String>, CommandError> {
    let output = Command::new(DIFF)
        .args(["-ruN", "--no-dereference"])
        .arg(old)
        .arg(new)
        .output()
        .map_err(io_error(Path::new(DIFF)))?;
    match output.status.code() {
        Some(0) => Ok(None),
        Some(DIFF_DIFFERENT) => Ok(Some(
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned(),
        )),
        _ => Err(CommandError::Refused(format!(
            "cannot compare {} with {}: {}",
            old.display(),
            new.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// One entry of a hooks tree, as far as the digest cares.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    File { mode: u32 },
    Symlink(PathBuf),
    Other,
}

impl Node {
    fn describe(&self) -> String {
        match self {
            Node::File { mode } => format!("file {mode:04o}"),
            Node::Symlink(target) => format!("symlink -> {}", target.display()),
            Node::Other => "special file".to_owned(),
        }
    }
}

/// A line for every path whose type, executable bit or symlink target
/// differs, and for every path added or removed.
fn metadata_changes(old: &Path, new: &Path) -> Result<Vec<String>, CommandError> {
    let (old, new) = (nodes(old)?, nodes(new)?);
    let paths: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).collect();
    Ok(paths
        .into_iter()
        .filter_map(|path| change_line(path, old.get(path), new.get(path)))
        .collect())
}

fn change_line(path: &Path, old: Option<&Node>, new: Option<&Node>) -> Option<String> {
    let shown = path.display();
    match (old, new) {
        (None, Some(node)) => Some(format!("added: {shown} ({})", node.describe())),
        (Some(_), None) => Some(format!("removed: {shown}")),
        (Some(Node::File { mode: a }), Some(Node::File { mode: b })) => {
            ((a ^ b) & EXEC_BITS != 0).then(|| format!("mode {a:04o} -> {b:04o}: {shown}"))
        }
        (Some(Node::Symlink(a)), Some(Node::Symlink(b))) => {
            (a != b).then(|| format!("symlink {shown}: {} -> {}", a.display(), b.display()))
        }
        (Some(a), Some(b)) => {
            (a != b).then(|| format!("type {} -> {}: {shown}", a.describe(), b.describe()))
        }
        (None, None) => None,
    }
}

/// Every non-directory entry under `root` by relative path; a missing root
/// is empty.
fn nodes(root: &Path) -> Result<BTreeMap<PathBuf, Node>, CommandError> {
    let mut found = BTreeMap::new();
    walk(root, Path::new(""), &mut found).map_err(io_error(root))?;
    Ok(found)
}

fn walk(dir: &Path, prefix: &Path, found: &mut BTreeMap<PathBuf, Node>) -> io::Result<()> {
    let items = match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    for item in items {
        let item = item?;
        let relative = prefix.join(item.file_name());
        let meta = fs::symlink_metadata(item.path())?;
        let node = if meta.is_dir() {
            walk(&item.path(), &relative, found)?;
            continue;
        } else if meta.file_type().is_symlink() {
            Node::Symlink(fs::read_link(item.path())?)
        } else if meta.is_file() {
            Node::File {
                mode: meta.permissions().mode() & PERMISSION_BITS,
            }
        } else {
            Node::Other
        };
        found.insert(relative, node);
    }
    Ok(())
}
