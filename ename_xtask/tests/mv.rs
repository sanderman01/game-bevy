//! `ename_mv` over a real filesystem, with and without git.

mod support;

use ename_xtask::mv::mv;
use std::path::Path;
use std::process::Command;

#[test]
fn moves_the_asset_and_both_sidecars_without_git() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    let from = root.join("basegame/core/ships/airship.glb");
    let to = root.join("basegame/core/ships/warship.glb");

    let moved = mv(&from, &to).expect("the asset exists");
    assert_eq!(moved.len(), 3, "the asset plus its .meta and .alias");
    assert!(moved.iter().all(|m| !m.via_git));

    assert!(!from.exists());
    assert!(to.exists());
    assert!(root.join("basegame/core/ships/warship.glb.meta").exists());
    assert!(root.join("basegame/core/ships/warship.glb.alias").exists());
    assert!(!root.join("basegame/core/ships/airship.glb.meta").exists());
    assert!(!root.join("basegame/core/ships/airship.glb.alias").exists());
}

#[test]
fn a_missing_sidecar_is_skipped_without_error() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    std::fs::remove_file(root.join("basegame/core/ships/airship.glb.meta")).unwrap();

    let from = root.join("basegame/core/ships/airship.glb");
    let to = root.join("basegame/core/ships/warship.glb");
    let moved = mv(&from, &to).expect("the asset exists");

    assert_eq!(
        moved.len(),
        2,
        "only the asset and the .alias, the .meta never existed"
    );
    assert!(!root.join("basegame/core/ships/warship.glb.meta").exists());
}

#[test]
fn a_missing_source_asset_is_an_error() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    let from = root.join("basegame/core/ships/does_not_exist.glb");
    let to = root.join("basegame/core/ships/warship.glb");
    assert!(mv(&from, &to).is_err());
}

/// `std::fs::rename` fails on all platforms this project targets when the destination's parent
/// directory does not exist. `ename_mv` must report that as an error rather than claiming success
/// while the asset is still sitting at `from`.
#[test]
fn a_failed_rename_is_an_error_and_leaves_the_source_in_place() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    let from = root.join("basegame/core/ships/airship.glb");
    let to = root.join("basegame/core/ships/no/such/dir/warship.glb");

    assert!(mv(&from, &to).is_err());
    assert!(from.exists(), "nothing should be silently half-moved");
    assert!(!to.exists());
}

/// Inside a git work tree, a tracked file is moved with `git mv`, which stages the rename; an
/// untracked one -- a `.alias` nobody has run `git add` on yet, the common case right after
/// `ename_fix` creates one -- falls back to a plain rename instead of failing the whole command.
#[test]
fn a_tracked_file_uses_git_mv_and_an_untracked_one_falls_back() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(
        root,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "add",
            "basegame/core/ships/airship.glb",
            "basegame/core/ships/airship.glb.meta",
        ],
    );
    git(
        root,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );
    // The `.alias` is left untracked on purpose.

    let from = root.join("basegame/core/ships/airship.glb");
    let to = root.join("basegame/core/ships/warship.glb");
    let moved = mv(&from, &to).expect("the asset exists");

    let via_git: Vec<bool> = moved.iter().map(|m| m.via_git).collect();
    assert_eq!(
        via_git,
        [true, true, false],
        "the asset and .meta are tracked, the .alias is not"
    );

    let status = git_output(root, &["status", "--porcelain"]);
    assert!(
        status.contains("R  "),
        "the tracked files must show as a staged rename:\n{status}"
    );
    assert!(to.exists());
    assert!(root.join("basegame/core/ships/warship.glb.alias").exists());
}

/// Regression test for a bug where `ename_mv` handed `git mv` a *relative* `from`/`to` while also
/// pointing the child process's `current_dir` at a directory derived from `from`: git resolved the
/// relative pathspec against that directory a second time, so `git mv` always failed and only the
/// `std::fs::rename` fallback ever ran -- silently, since the fallback still moves the file. This
/// only shows up with a relative path, which is exactly how `cargo xtask ename_mv <from> <to>` is
/// invoked from a repo checkout, so the absolute `TempDir`-rooted paths every other test in this
/// file uses can't catch it.
///
/// This is the one test in this file that changes the test process's current directory.
/// `std::env::set_current_dir` is process-global and Rust tests in one binary run in parallel by
/// default, so no other test here may ever do the same; `CwdGuard` restores the original cwd on
/// drop, including on panic, so a failing assertion can't leave it changed for whatever runs next.
/// The other tests are safe to run alongside this one because none of them reads the process cwd:
/// every path they use is either already absolute (`TempDir`-rooted) or handed to a spawned `git`
/// via an explicit `current_dir(root)`.
#[test]
fn a_relative_path_argument_from_the_repo_root_still_uses_git_mv() {
    let dir = support::copy_fixture("mv_source");
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(
        root,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "add",
            "basegame/core/ships/airship.glb",
            "basegame/core/ships/airship.glb.meta",
        ],
    );
    git(
        root,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );

    let _cwd_guard = CwdGuard::change_to(root);

    let from = Path::new("basegame/core/ships/airship.glb");
    let to = Path::new("basegame/core/ships/warship.glb");
    let moved = mv(from, to).expect("the asset exists");

    let via_git: Vec<bool> = moved.iter().map(|m| m.via_git).collect();
    assert_eq!(
        via_git,
        [true, true, false],
        "git mv must actually run for the tracked files, not silently fall back to rename"
    );

    let status = git_output(root, &["status", "--porcelain"]);
    assert!(
        status.contains("R  "),
        "the tracked files must show as a staged rename:\n{status}"
    );
}

/// Restores the process's current directory on drop, so a test that must change it (process-global
/// state, unsafe to share with parallel tests) can't leave it changed for another test -- including
/// when the guarded test panics.
struct CwdGuard {
    original: std::path::PathBuf,
}

impl CwdGuard {
    fn change_to(dir: &Path) -> Self {
        let original = std::env::current_dir().expect("the process has a current directory");
        std::env::set_current_dir(dir).expect("change into the scratch repo");
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("git is on PATH");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git is on PATH");
    String::from_utf8(out.stdout).expect("git prints UTF-8")
}
