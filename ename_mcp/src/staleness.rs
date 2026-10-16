//! Noticing that this process is out of date: its sources moved on, or another build is on disk.
//!
//! The agent's client launches this binary once and keeps the process for the whole session, so
//! a `cargo build` halfway through leaves an old image answering new tools. Nothing in MCP says
//! a server may have been replaced, and the symptoms are indirect: a tool argument the schema
//! no longer describes, or a BRP result whose shape the running code predates. Both read as a
//! bug in the game rather than as an out-of-date sidecar, which is the wrong thing to go
//! investigate. Only the user can replace the process (`/mcp reconnect`), so every text says to
//! ask them.
//!
//! Two checks, in this order:
//!
//! 1. A source of the executable on disk is newer than it, or gone: nobody built since the
//!    sources changed. The sources are the files of this crate that cargo's dep-info next to the
//!    executable lists, so test-only modules and stray files do not count; without dep-info, every
//!    `.rs` under `src`. Skipped when the sources are not on this machine, or the executable is
//!    not in a cargo target directory, since building would not replace it then.
//! 2. The executable on disk differs from the one this process started from.
//!
//! Both use only what `std::fs::Metadata` reports on every platform: length and modification
//! time. An inode would be the sharper signal, and `MetadataExt` is Unix-only.
//!
//! A source edited while cargo was building is missed: the executable ends up newer than the
//! edit. Comparing against the build's start would need its fingerprint directory, whose name is
//! a hash of features, profile and flags that only cargo knows.
//!
//! The first result after a condition starts or changes carries the full text, and so does
//! every failure, since a failure is where a stale sidecar does damage. Other results carry a
//! short form, so the warning stays on every result without drowning it.
//!
//! The state is process-global because that is what it describes -- this executable, not any
//! one connection -- and because the warning has to reach the error path, where there is no
//! server to hang it on.

use std::{
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::{Duration, Instant, SystemTime},
};

/// The build this process was launched from. `None` when the executable could not be stat'd at
/// startup, which leaves the check silent rather than guessing.
static LAUNCH: OnceLock<Option<Launch>> = OnceLock::new();

/// The condition the last full text described, so the results after it can say it briefly.
static SHOWN: Mutex<Option<Staleness>> = Mutex::new(None);

/// How long one look at the sources answers for. Every tool result asks.
const SOURCE_CACHE: Duration = Duration::from_secs(2);

/// Records the build this process is running. Call once, before serving.
pub fn record() {
    LAUNCH.get_or_init(Launch::read);
}

/// The warning for a successful result: the full text the first time a condition shows, the
/// short form after that. `None` while this process is up to date.
pub fn warning() -> Option<String> {
    notice(false)
}

/// The warning for a failed result: always the full text.
pub fn failure_warning() -> Option<String> {
    notice(true)
}

fn notice(failure: bool) -> Option<String> {
    let launch = LAUNCH.get()?.as_ref()?;
    let staleness = launch.assess();
    let form = form(
        staleness,
        &mut SHOWN.lock().unwrap_or_else(PoisonError::into_inner),
        failure,
    )?;
    let build = launch.rebuild.as_ref().map(|rebuild| &rebuild.build);
    Some(staleness?.text(form, build, cfg!(windows), SystemTime::now()))
}

struct Launch {
    exe: PathBuf,
    stamp: Stamp,
    rebuild: Option<Rebuild>,
}

impl Launch {
    /// Resolves the path once, at startup. On Linux `current_exe` reads `/proc/self/exe`, which
    /// starts reporting a `(deleted)` suffix as soon as cargo replaces the file, so asking again
    /// later would compare against a path that no longer exists.
    fn read() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let stamp = Stamp::read(&exe)?;
        let rebuild = Rebuild::find(&exe);
        Some(Self {
            exe,
            stamp,
            rebuild,
        })
    }

    fn assess(&self) -> Option<Staleness> {
        let sources = self.rebuild.as_ref().and_then(Rebuild::newest_change);
        assess(self.stamp, Stamp::read(&self.exe), sources)
    }
}

/// The sources this executable is built from, and how to build it again.
struct Rebuild {
    crate_dir: PathBuf,
    /// Cargo's dep-info for the executable: `ename_mcp.d` beside it.
    dep_info: PathBuf,
    build: Build,
    /// The newest change, and when it was read.
    newest: Mutex<Option<(Instant, Option<Change>)>>,
}

impl Rebuild {
    fn find(exe: &Path) -> Option<Self> {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = crate_dir.parent()?;
        // `.fingerprint` sits in every cargo profile directory: without it, this is a copy that
        // no build would replace.
        if !crate_dir.join("src").is_dir()
            || !workspace.join("Cargo.toml").is_file()
            || !exe.parent()?.join(".fingerprint").is_dir()
        {
            return None;
        }
        let build = Build::new(
            &canonical(exe),
            &canonical(workspace),
            &canonical(&std::env::current_dir().ok()?),
            std::env::var_os("CARGO_TARGET_DIR").is_some(),
            |dir| dir.join(".rustc_info.json").is_file() || dir.join("CACHEDIR.TAG").is_file(),
        )?;
        Some(Self {
            crate_dir: crate_dir.to_owned(),
            dep_info: exe.with_extension("d"),
            build,
            newest: Mutex::new(None),
        })
    }

    fn newest_change(&self) -> Option<Change> {
        let mut cache = self.newest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((read_at, newest)) = *cache
            && read_at.elapsed() < SOURCE_CACHE
        {
            return newest;
        }
        let newest = match std::fs::read_to_string(&self.dep_info) {
            Ok(dep_info) => newest_listed(
                dep_info_paths(&dep_info)
                    .into_iter()
                    .filter(|path| is_crate_source(path, &self.crate_dir)),
            ),
            Err(_) => newest_source(&self.crate_dir.join("src")).map(Change::Modified),
        };
        *cache = Some((Instant::now(), newest));
        newest
    }
}

/// The files a cargo dep-info file (`target: dep dep ...`) lists. Cargo escapes a space in a
/// path as `\ `.
fn dep_info_paths(dep_info: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in dep_info.lines() {
        if line.starts_with('#') {
            continue;
        }
        let Some((_, deps)) = line.split_once(": ") else {
            continue;
        };
        let mut path = String::new();
        let mut chars = deps.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => path.push(chars.next().unwrap_or(' ')),
                ' ' => {
                    if !path.is_empty() {
                        paths.push(PathBuf::from(std::mem::take(&mut path)));
                    }
                }
                c => path.push(c),
            }
        }
        if !path.is_empty() {
            paths.push(PathBuf::from(path));
        }
    }
    paths
}

/// Whether a dep-info entry is one of this crate's own sources. The manifest is left out: cargo
/// hashes it, so an edit that does not change the build would leave the warning up forever.
fn is_crate_source(path: &Path, crate_dir: &Path) -> bool {
    path.starts_with(crate_dir)
        && path
            .file_name()
            .is_none_or(|name| name != "Cargo.toml" && name != "Cargo.lock")
}

/// The newest change among `paths`. A file that is gone counts as changed: it was deleted or
/// renamed since the build.
fn newest_listed(paths: impl IntoIterator<Item = PathBuf>) -> Option<Change> {
    let mut newest = None;
    for path in paths {
        let Ok(modified) = std::fs::metadata(&path).and_then(|metadata| metadata.modified()) else {
            return Some(Change::Removed);
        };
        newest = newest.max(Some(modified));
    }
    newest.map(Change::Modified)
}

/// The newest modification time of a `.rs` file under `dir`. Hidden files are left out: they
/// are editors' swap and lock files. Symlinked directories are not followed.
fn newest_source(dir: &Path) -> Option<SystemTime> {
    let mut newest = None;
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let modified = std::fs::metadata(&path).and_then(|metadata| metadata.modified());
                newest = newest.max(modified.ok());
            }
        }
    }
    newest
}

/// `path` with symlinks and relative parts resolved, without Windows' `\\?\` prefix, which
/// `current_exe` does not carry and Git Bash cannot use.
fn canonical(path: &Path) -> PathBuf {
    strip_verbatim(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()))
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// A path as the agent types it: with `/` on Windows too, which every Windows shell accepts and
/// Git Bash needs.
fn shown(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// The cargo command that rebuilds this executable in place, and the directory to run it in.
#[derive(Debug, PartialEq, Eq)]
struct Build {
    command: String,
    dir: String,
}

impl Build {
    /// `exe` is `<target dir>/[<triple>/]<profile>/ename_mcp`; `is_target_root` tells a target
    /// dir from a triple's directory inside one. The command runs in the workspace when the
    /// client's working directory is inside it, and through `--manifest-path` from where the
    /// client runs otherwise (another workspace using this one's crates). `--target-dir` is
    /// named when the default would build somewhere else, or `CARGO_TARGET_DIR` may redirect it.
    fn new(
        exe: &Path,
        workspace: &Path,
        cwd: &Path,
        target_dir_env: bool,
        is_target_root: impl Fn(&Path) -> bool,
    ) -> Option<Self> {
        let profile_dir = exe.parent()?;
        let profile = profile_dir.file_name()?.to_str()?;
        let mut target_dir = profile_dir.parent()?;
        let mut triple = None;
        if !is_target_root(target_dir)
            && let Some(root) = target_dir.parent()
            && is_target_root(root)
        {
            triple = Some(target_dir.file_name()?.to_str()?);
            target_dir = root;
        }
        let dir = if cwd.starts_with(workspace) {
            workspace
        } else {
            cwd
        };
        let mut command = "cargo build".to_owned();
        if dir != workspace {
            let manifest = relative(dir, &workspace.join("Cargo.toml"));
            command += &format!(" --manifest-path {}", shown(&manifest));
        }
        command += " -p ename_mcp";
        match profile {
            "debug" => {}
            "release" => command += " --release",
            other => command += &format!(" --profile {other}"),
        }
        if let Some(triple) = triple {
            command += &format!(" --target {triple}");
        }
        if target_dir_env || target_dir != workspace.join("target") {
            command += &format!(" --target-dir {}", shown(&relative(dir, target_dir)));
        }
        Some(Self {
            command,
            dir: shown(dir),
        })
    }
}

impl std::fmt::Display for Build {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` in `{}`", self.command, self.dir)
    }
}

/// `to` as seen from `from`, both absolute. Absolute when they share no root, such as two drives
/// on Windows.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return to.to_owned();
    }
    let mut path: PathBuf =
        std::iter::repeat_n(Component::ParentDir, from.len() - common).collect();
    path.extend(&to_parts[common..]);
    if path.as_os_str().is_empty() {
        path.push(".");
    }
    path
}

/// A build of the executable, in the two facts every platform and filesystem reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Stamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
        })
    }
}

/// The newest change to the sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Modified(SystemTime),
    /// A source the build used is gone.
    Removed,
}

/// Why this process is out of date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staleness {
    /// A source changed after the build on disk, or there is no build on disk.
    Sources {
        change: Change,
        on_disk: Option<Stamp>,
    },
    /// The build on disk is not the one this process runs. Not necessarily newer: switching
    /// between `cargo dev-build` and `cargo build -p ename_mcp` swaps in the other feature set's
    /// earlier build, with its earlier time.
    OtherBuild { on_disk: Stamp },
}

impl Staleness {
    /// Whether `other` is the condition this one already told the agent about. Further source
    /// edits are not news; a new build is.
    fn same_condition(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Sources { on_disk: a, .. }, Self::Sources { on_disk: b, .. }) => a == b,
            (Self::OtherBuild { on_disk: a }, Self::OtherBuild { on_disk: b }) => a == b,
            _ => false,
        }
    }

    fn text(self, form: Form, build: Option<&Build>, windows: bool, now: SystemTime) -> String {
        let build = build.map_or_else(|| "`cargo build -p ename_mcp`".to_owned(), Build::to_string);
        let until = if windows {
            "the user enables it again"
        } else {
            "the user reconnects"
        };
        let missing = format!(
            "Until {until}, tools added or changed since this build are missing or behave \
             differently."
        );
        match (self, form) {
            (Self::Sources { change, on_disk }, Form::Full) => {
                let what = match (change, on_disk) {
                    (Change::Modified(at), Some(_)) => {
                        format!("its sources changed {}, after the last build", ago(at, now))
                    }
                    (Change::Modified(at), None) => format!(
                        "its sources changed {}, and no build is on disk",
                        ago(at, now)
                    ),
                    (Change::Removed, _) => "a source file the last build used is gone".to_owned(),
                };
                if windows {
                    format!(
                        "ename_mcp is out of date: {what}. Windows locks the running sidecar, so \
                         the build fails while it is connected: ask the user to disable the \
                         `game` server in `/mcp` and wait for them to confirm, then run {build}, \
                         then ask them to enable it again (you cannot do either). If the build \
                         fails, tell the user. {missing}"
                    )
                } else {
                    format!(
                        "ename_mcp is out of date: {what}. Run {build}, then ask the user to run \
                         `/mcp reconnect` (you cannot). If the build fails, tell the user. \
                         {missing}"
                    )
                }
            }
            (Self::Sources { .. }, Form::Short) if windows => format!(
                "ename_mcp is out of date: ask the user to disable the `game` server in `/mcp` \
                 and wait for them to confirm, then run {build}, then ask them to enable it again."
            ),
            (Self::Sources { .. }, Form::Short) => format!(
                "ename_mcp is out of date: run {build}, then ask the user to run `/mcp reconnect`."
            ),
            (Self::OtherBuild { on_disk }, Form::Full) => format!(
                "ename_mcp is out of date: the build on disk (from {}) is not the one this \
                 process started from. Ask the user to run `/mcp reconnect` (you cannot). \
                 {missing}",
                ago(on_disk.modified, now),
            ),
            (Self::OtherBuild { .. }, Form::Short) => {
                "ename_mcp is out of date; ask the user to run `/mcp reconnect`.".to_owned()
            }
        }
    }
}

/// The two checks, sources first: building is the step that has to come before reconnecting.
/// `sources` is `None` when they are not checked.
fn assess(running: Stamp, on_disk: Option<Stamp>, sources: Option<Change>) -> Option<Staleness> {
    if let Some(change) = sources {
        let stale = match change {
            Change::Modified(at) => on_disk.is_none_or(|on_disk| at > on_disk.modified),
            Change::Removed => true,
        };
        if stale {
            return Some(Staleness::Sources { change, on_disk });
        }
    }
    let on_disk = on_disk?;
    (on_disk != running).then_some(Staleness::OtherBuild { on_disk })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Full,
    Short,
}

/// How to say `current`, given the condition `shown` last described in full, and records it.
fn form(current: Option<Staleness>, shown: &mut Option<Staleness>, failure: bool) -> Option<Form> {
    let Some(current) = current else {
        *shown = None;
        return None;
    };
    let repeat = shown.is_some_and(|shown| shown.same_condition(&current));
    *shown = Some(current);
    Some(if repeat && !failure {
        Form::Short
    } else {
        Form::Full
    })
}

/// How long before `now`, in the coarsest unit that still says something useful.
///
/// A wall clock would have to be formatted in the reader's timezone, and the reader would then
/// have to subtract to learn the thing that actually matters, which is whether the build moved
/// during this session.
fn ago(time: SystemTime, now: SystemTime) -> String {
    // A modification time in the future means a clock that disagrees with the filesystem's, not
    // a build that has yet to happen. Reporting it as "just now" beats reporting nothing.
    let seconds = now.duration_since(time).unwrap_or_default().as_secs();
    let (count, unit) = match seconds {
        0..60 => return "less than a minute ago".to_owned(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use super::{
        Build, Change, Form, Staleness, Stamp, ago, assess, dep_info_paths, form, is_crate_source,
        newest_listed, newest_source, relative, strip_verbatim,
    };

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn stamp(seconds: u64) -> Stamp {
        Stamp {
            len: 100,
            modified: at(seconds),
        }
    }

    #[test]
    fn reports_the_coarsest_useful_unit() {
        let now = at(1_000_000);
        let ago_by = |seconds: u64| ago(at(1_000_000 - seconds), now);
        assert_eq!(ago_by(0), "less than a minute ago");
        assert_eq!(ago_by(59), "less than a minute ago");
        assert_eq!(ago_by(60), "1 minute ago");
        assert_eq!(ago_by(3599), "59 minutes ago");
        assert_eq!(ago_by(3600), "1 hour ago");
        assert_eq!(ago_by(86_399), "23 hours ago");
        assert_eq!(ago_by(86_400), "1 day ago");
        assert_eq!(ago_by(86_400 * 9), "9 days ago");
    }

    /// A clock behind the filesystem's would otherwise panic on the subtraction.
    #[test]
    fn survives_a_modification_time_in_the_future() {
        assert_eq!(ago(at(1600), at(1000)), "less than a minute ago");
    }

    #[test]
    fn up_to_date_says_nothing() {
        let sources = Some(Change::Modified(at(5)));
        assert_eq!(assess(stamp(10), Some(stamp(10)), sources), None);
    }

    #[test]
    fn sources_newer_than_the_build_on_disk() {
        let change = Change::Modified(at(20));
        assert_eq!(
            assess(stamp(10), Some(stamp(10)), Some(change)),
            Some(Staleness::Sources {
                change,
                on_disk: Some(stamp(10)),
            })
        );
    }

    #[test]
    fn a_removed_source_is_a_change() {
        assert_eq!(
            assess(stamp(10), Some(stamp(10)), Some(Change::Removed)),
            Some(Staleness::Sources {
                change: Change::Removed,
                on_disk: Some(stamp(10)),
            })
        );
    }

    #[test]
    fn build_on_disk_differs_from_the_running_one() {
        let sources = Some(Change::Modified(at(5)));
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), sources),
            Some(Staleness::OtherBuild { on_disk: stamp(20) })
        );
    }

    /// Building has to come first, so the sources win.
    #[test]
    fn both_reports_the_sources() {
        let change = Change::Modified(at(30));
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), Some(change)),
            Some(Staleness::Sources {
                change,
                on_disk: Some(stamp(20)),
            })
        );
    }

    #[test]
    fn without_sources_only_the_build_counts() {
        assert_eq!(assess(stamp(10), Some(stamp(10)), None), None);
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), None),
            Some(Staleness::OtherBuild { on_disk: stamp(20) })
        );
    }

    #[test]
    fn a_missing_build_needs_building() {
        let change = Change::Modified(at(5));
        assert_eq!(
            assess(stamp(10), None, Some(change)),
            Some(Staleness::Sources {
                change,
                on_disk: None,
            })
        );
        assert_eq!(assess(stamp(10), None, None), None);
    }

    #[test]
    fn full_once_per_condition_and_on_every_failure() {
        let sources = |changed| Staleness::Sources {
            change: Change::Modified(at(changed)),
            on_disk: Some(stamp(10)),
        };
        let rebuilt = Staleness::OtherBuild { on_disk: stamp(40) };
        let mut shown = None;
        assert_eq!(form(None, &mut shown, false), None);
        assert_eq!(form(Some(sources(20)), &mut shown, false), Some(Form::Full));
        assert_eq!(
            form(Some(sources(20)), &mut shown, false),
            Some(Form::Short)
        );
        // Another edit is the same condition.
        assert_eq!(
            form(Some(sources(30)), &mut shown, false),
            Some(Form::Short)
        );
        assert_eq!(form(Some(sources(30)), &mut shown, true), Some(Form::Full));
        assert_eq!(
            form(Some(sources(30)), &mut shown, false),
            Some(Form::Short)
        );
        assert_eq!(form(Some(rebuilt), &mut shown, false), Some(Form::Full));
        assert_eq!(form(Some(rebuilt), &mut shown, false), Some(Form::Short));
        assert_eq!(form(None, &mut shown, false), None);
        assert_eq!(form(Some(rebuilt), &mut shown, false), Some(Form::Full));
    }

    #[test]
    fn a_new_build_is_a_new_condition() {
        let rebuilt = |on_disk| Staleness::OtherBuild {
            on_disk: stamp(on_disk),
        };
        let mut shown = None;
        assert_eq!(form(Some(rebuilt(20)), &mut shown, false), Some(Form::Full));
        assert_eq!(form(Some(rebuilt(30)), &mut shown, false), Some(Form::Full));
        assert_eq!(
            form(Some(rebuilt(30)), &mut shown, false),
            Some(Form::Short)
        );
    }

    #[test]
    fn texts_say_to_build_and_to_ask_the_user() {
        let build = Build {
            command: "cargo build -p ename_mcp".to_owned(),
            dir: "/w/game".to_owned(),
        };
        let now = at(1000);
        let sources = Staleness::Sources {
            change: Change::Modified(at(1000)),
            on_disk: None,
        };
        let full = sources.text(Form::Full, Some(&build), false, now);
        assert_eq!(
            full,
            "ename_mcp is out of date: its sources changed less than a minute ago, and no build \
             is on disk. Run `cargo build -p ename_mcp` in `/w/game`, then ask the user to run \
             `/mcp reconnect` (you cannot). If the build fails, tell the user. Until the user \
             reconnects, tools added or changed since this build are missing or behave \
             differently."
        );
        let windows = sources.text(Form::Short, Some(&build), true, now);
        assert_eq!(
            windows,
            "ename_mcp is out of date: ask the user to disable the `game` server in `/mcp` and \
             wait for them to confirm, then run `cargo build -p ename_mcp` in `/w/game`, then ask \
             them to enable it again."
        );
        let rebuilt = Staleness::OtherBuild {
            on_disk: stamp(1000 - 120),
        };
        assert_eq!(
            rebuilt.text(Form::Full, Some(&build), false, now),
            "ename_mcp is out of date: the build on disk (from 2 minutes ago) is not the one this \
             process started from. Ask the user to run `/mcp reconnect` (you cannot). Until the \
             user reconnects, tools added or changed since this build are missing or behave \
             differently."
        );
        assert_eq!(
            rebuilt.text(Form::Short, Some(&build), false, now),
            "ename_mcp is out of date; ask the user to run `/mcp reconnect`."
        );
    }

    /// A triple's directory is the only one here that is not a target root.
    fn command(exe: &str, cwd: &str, target_dir_env: bool) -> Build {
        Build::new(
            Path::new(exe),
            Path::new("/w/game"),
            Path::new(cwd),
            target_dir_env,
            |dir| !dir.ends_with("x86_64-pc-windows-msvc"),
        )
        .unwrap()
    }

    #[test]
    fn builds_in_the_workspace_from_inside_it() {
        let expected = Build {
            command: "cargo build -p ename_mcp".to_owned(),
            dir: "/w/game".to_owned(),
        };
        assert_eq!(
            command("/w/game/target/debug/ename_mcp", "/w/game", false),
            expected
        );
        assert_eq!(
            command("/w/game/target/debug/ename_mcp", "/w/game/ename_mcp", false),
            expected
        );
    }

    #[test]
    fn builds_through_the_manifest_from_another_workspace() {
        assert_eq!(
            command("/w/game/target/debug/ename_mcp", "/w/airshipgame", false),
            Build {
                command: "cargo build --manifest-path ../game/Cargo.toml -p ename_mcp".to_owned(),
                dir: "/w/airshipgame".to_owned(),
            }
        );
    }

    #[test]
    fn names_the_profile_target_and_target_dir_that_built_it() {
        let build = |exe, target_dir_env| command(exe, "/w/game", target_dir_env).command;
        assert_eq!(
            build("/w/game/target/release/ename_mcp", false),
            "cargo build -p ename_mcp --release"
        );
        assert_eq!(
            build("/w/game/target/ename-launch-bin/debug/ename_mcp", false),
            "cargo build -p ename_mcp --target-dir target/ename-launch-bin"
        );
        assert_eq!(
            build("/w/game/target/debug/ename_mcp", true),
            "cargo build -p ename_mcp --target-dir target"
        );
        assert_eq!(
            build("/w/game/target/fast/ename_mcp", false),
            "cargo build -p ename_mcp --profile fast"
        );
        assert_eq!(
            build(
                "/w/game/target/x86_64-pc-windows-msvc/debug/ename_mcp.exe",
                false
            ),
            "cargo build -p ename_mcp --target x86_64-pc-windows-msvc"
        );
        assert_eq!(
            build(
                "/w/other/x86_64-pc-windows-msvc/release/ename_mcp.exe",
                false
            ),
            "cargo build -p ename_mcp --release --target x86_64-pc-windows-msvc --target-dir \
             ../other"
        );
    }

    #[test]
    fn strips_the_verbatim_prefix() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\w\game")),
            PathBuf::from(r"C:\w\game")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\host\share\game")),
            PathBuf::from(r"\\host\share\game")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("/w/game")),
            PathBuf::from("/w/game")
        );
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative(Path::new("/w/a"), Path::new("/w/b/c")),
            Path::new("../b/c")
        );
        assert_eq!(
            relative(Path::new("/w/a"), Path::new("/w/a")),
            Path::new(".")
        );
        assert_eq!(
            relative(Path::new("/w/a"), Path::new("/w/a/t")),
            Path::new("t")
        );
    }

    #[test]
    fn reads_cargo_dep_info() {
        let dep_info = "/w/game/target/debug/ename_mcp: /w/game/ename_mcp/src/main.rs \
                        /w/game/ename_mcp/src/my\\ file.rs /w/game/ename_mcp/Cargo.toml \
                        /w/game/other/src/lib.rs\n\n\
                        C:\\w\\target\\debug\\ename_mcp.exe: C:\\w\\ename_mcp\\src\\main.rs\n";
        let paths = dep_info_paths(dep_info);
        assert_eq!(
            paths,
            [
                "/w/game/ename_mcp/src/main.rs",
                "/w/game/ename_mcp/src/my file.rs",
                "/w/game/ename_mcp/Cargo.toml",
                "/w/game/other/src/lib.rs",
                r"C:\w\ename_mcp\src\main.rs",
            ]
            .map(PathBuf::from)
        );
        let crate_dir = Path::new("/w/game/ename_mcp");
        let sources: Vec<_> = paths
            .iter()
            .filter(|path| is_crate_source(path, crate_dir))
            .collect();
        assert_eq!(
            sources,
            [
                Path::new("/w/game/ename_mcp/src/main.rs"),
                Path::new("/w/game/ename_mcp/src/my file.rs"),
            ]
        );
    }

    fn file_at(path: &Path, seconds: u64) {
        File::create(path)
            .unwrap()
            .set_modified(at(seconds))
            .unwrap();
    }

    #[test]
    fn the_newest_listed_source_or_a_removed_one() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rs"), dir.path().join("b.rs"));
        file_at(&a, 100);
        file_at(&b, 200);
        assert_eq!(
            newest_listed([a.clone(), b.clone()]),
            Some(Change::Modified(at(200)))
        );
        assert_eq!(
            newest_listed([a, dir.path().join("gone.rs"), b]),
            Some(Change::Removed)
        );
        assert_eq!(newest_listed([]), None);
    }

    #[test]
    fn the_walk_counts_visible_rust_files_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        file_at(&dir.path().join("a.rs"), 100);
        file_at(&dir.path().join("sub/b.rs"), 200);
        file_at(&dir.path().join(".b.rs.swp"), 300);
        file_at(&dir.path().join(".hidden.rs"), 300);
        file_at(&dir.path().join("notes.txt"), 300);
        assert_eq!(newest_source(dir.path()), Some(at(200)));
    }
}
