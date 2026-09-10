//! `ename_mv` -- moves an asset and its `.meta`/`.alias` sidecars together, the way `git mv`
//! moves a file and its history together.
//!
//! Three independent files, none required to exist except the asset itself: `<name>.meta` (Bevy's
//! import settings) and `<name>.alias` (ours). Inside a git work tree each existing one is moved
//! with `git mv` so history follows it; a file `git mv` refuses -- typically because it is not yet
//! tracked, which a freshly written `.alias` often is not -- falls back to a plain filesystem
//! rename rather than failing the whole command over one untracked sidecar. Outside a git work
//! tree, every file is a plain rename.
//!
//! This command does not touch alias content. A derived alias that named the old path goes stale
//! until `ename_fix` is run again, which recomputes it -- the same two-step story
//! `scratch/content-addressing-design.md` describes for a bulk asset's alias surviving a move.

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One file `ename_mv` moved, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub from: PathBuf,
    pub to: PathBuf,
    pub via_git: bool,
}

/// Why `ename_mv` refused to run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MvError {
    SourceMissing(PathBuf),
}

impl Display for MvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing(path) => write!(f, "{} does not exist", path.display()),
        }
    }
}

/// Moves `from` and, if they exist, `from` + `.meta` and `from` + `.alias`, to the matching `to`
/// paths. Every path is resolved relative to the current directory, the same as `git mv`.
pub fn mv(from: &Path, to: &Path) -> Result<Vec<Moved>, MvError> {
    if !from.exists() {
        return Err(MvError::SourceMissing(from.to_path_buf()));
    }
    // Git commands are run with this as their working directory rather than the process's own,
    // so a caller passing an absolute path into an unrelated repository (as the tests do, against
    // a scratch directory) is detected and moved correctly rather than against whatever repo the
    // process happened to start in.
    let git_dir = from.parent().unwrap_or(Path::new("."));
    let in_git = is_inside_git_work_tree(git_dir);

    let mut moved = vec![move_one(git_dir, from, to, in_git)];
    for suffix in [".meta", ".alias"] {
        let sidecar_from = append(from, suffix);
        let sidecar_to = append(to, suffix);
        if sidecar_from.exists() {
            moved.push(move_one(git_dir, &sidecar_from, &sidecar_to, in_git));
        }
    }
    Ok(moved)
}

fn append(path: &Path, suffix: &str) -> PathBuf {
    let mut out = path.as_os_str().to_owned();
    out.push(suffix);
    PathBuf::from(out)
}

fn move_one(git_dir: &Path, from: &Path, to: &Path, in_git: bool) -> Moved {
    if in_git && git_mv(git_dir, from, to) {
        return Moved {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            via_git: true,
        };
    }
    if let Err(err) = std::fs::rename(from, to) {
        eprintln!(
            "ename_mv: could not move {} to {}: {err}",
            from.display(),
            to.display()
        );
    }
    Moved {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        via_git: false,
    }
}

fn git_mv(dir: &Path, from: &Path, to: &Path) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args(["mv", "--"])
        .arg(from)
        .arg(to)
        .status()
        .is_ok_and(|status| status.success())
}

fn is_inside_git_work_tree(dir: &Path) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|out| out.status.success())
}
