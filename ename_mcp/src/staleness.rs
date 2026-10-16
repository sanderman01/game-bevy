//! Noticing that this process is out of date: its sources moved on, or a newer build is on disk.
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
//! 1. A `.rs` file under this crate's `src` is newer than the executable on disk: nobody built
//!    since the sources changed. Skipped when the sources are not on this machine, or the
//!    executable is not in a cargo target directory, since building would not replace it then.
//! 2. The executable on disk differs from the one this process started from.
//!
//! Both use only what `std::fs::Metadata` reports on every platform: length and modification
//! time. An inode would be the sharper signal, and `MetadataExt` is Unix-only.
//!
//! The first result after a condition starts or changes carries the full text, and so does
//! every failure, since a failure is where a stale sidecar does damage. Other results carry a
//! short form, so the warning stays on every result without drowning it.
//!
//! The state is process-global because that is what it describes -- this executable, not any
//! one connection -- and because the warning has to reach the error path, where there is no
//! server to hang it on.

use std::{
    fmt,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::{Duration, Instant, SystemTime},
};

/// The build this process was launched from. `None` when the executable could not be stat'd at
/// startup, which leaves the check silent rather than guessing.
static LAUNCH: OnceLock<Option<Launch>> = OnceLock::new();

/// The condition the last full text described, so the results after it can say it briefly.
static SHOWN: Mutex<Option<Staleness>> = Mutex::new(None);

/// How long one walk of the sources answers for. Every tool result asks.
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
    Some(staleness?.text(form, build, cfg!(windows)))
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
        let sources = self.rebuild.as_ref().and_then(Rebuild::newest_source);
        assess(self.stamp, Stamp::read(&self.exe), sources)
    }
}

/// The sources this executable is built from, and how to build it again.
struct Rebuild {
    sources: PathBuf,
    build: Build,
    /// The newest source's modification time, and when it was read.
    newest: Mutex<Option<(Instant, Option<SystemTime>)>>,
}

impl Rebuild {
    fn find(exe: &Path) -> Option<Self> {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sources = manifest_dir.join("src");
        // `.fingerprint` sits in every cargo profile directory: without it, this is a copy that
        // no build would replace.
        if !sources.is_dir() || !exe.parent()?.join(".fingerprint").is_dir() {
            return None;
        }
        let workspace = canonical(manifest_dir.parent()?);
        let cwd = canonical(&std::env::current_dir().ok()?);
        let build = Build::new(
            exe,
            &workspace,
            &cwd,
            std::env::var_os("CARGO_TARGET_DIR").is_some(),
        )?;
        Some(Self {
            sources,
            build,
            newest: Mutex::new(None),
        })
    }

    fn newest_source(&self) -> Option<SystemTime> {
        let mut cache = self.newest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((read_at, newest)) = *cache
            && read_at.elapsed() < SOURCE_CACHE
        {
            return newest;
        }
        let newest = newest_source(&self.sources);
        *cache = Some((Instant::now(), newest));
        newest
    }
}

/// The newest modification time of a `.rs` file under `dir`. Hidden files are left out: they
/// are editors' swap and lock files.
fn newest_source(dir: &Path) -> Option<SystemTime> {
    let mut newest = None;
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                newest = newest.max(metadata.modified().ok());
            }
        }
    }
    newest
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The cargo command that rebuilds this executable in place, and the directory to run it in.
#[derive(Debug, PartialEq, Eq)]
struct Build {
    command: String,
    dir: PathBuf,
}

impl Build {
    /// `exe` is `<target dir>/<profile>/ename_mcp`. The command runs in the workspace when the
    /// client's working directory is inside it, and through `--manifest-path` from where the
    /// client runs otherwise (another workspace using this one's crates). `--target-dir` is
    /// named when the default would build somewhere else, or `CARGO_TARGET_DIR` may redirect it.
    fn new(exe: &Path, workspace: &Path, cwd: &Path, target_dir_env: bool) -> Option<Self> {
        let profile_dir = exe.parent()?;
        let target_dir = profile_dir.parent()?;
        let profile = profile_dir.file_name()?.to_str()?;
        let dir = if cwd.starts_with(workspace) {
            workspace
        } else {
            cwd
        };
        let mut command = "cargo build".to_owned();
        if dir != workspace {
            let manifest = relative(dir, &workspace.join("Cargo.toml"));
            command += &format!(" --manifest-path {}", manifest.display());
        }
        command += " -p ename_mcp";
        match profile {
            "debug" => {}
            "release" => command += " --release",
            other => command += &format!(" --profile {other}"),
        }
        if target_dir_env || target_dir != workspace.join("target") {
            command += &format!(" --target-dir {}", relative(dir, target_dir).display());
        }
        Some(Self {
            command,
            dir: dir.to_owned(),
        })
    }
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` in `{}`", self.command, self.dir.display())
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

/// Why this process is out of date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staleness {
    /// A source changed after the build on disk, or there is no build on disk.
    Sources {
        changed: SystemTime,
        on_disk: Option<Stamp>,
    },
    /// The build on disk is not the one this process runs.
    NewerBuild { running: Stamp, on_disk: Stamp },
}

impl Staleness {
    /// Whether `other` is the condition this one already told the agent about. Further source
    /// edits are not news; a new build is.
    fn same_condition(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Sources { on_disk: a, .. }, Self::Sources { on_disk: b, .. }) => a == b,
            (Self::NewerBuild { on_disk: a, .. }, Self::NewerBuild { on_disk: b, .. }) => a == b,
            _ => false,
        }
    }

    fn text(self, form: Form, build: Option<&Build>, windows: bool) -> String {
        const MISSING: &str =
            "Until then, tools added or changed since are missing or behave differently.";
        let build = build.map_or_else(|| "`cargo build -p ename_mcp`".to_owned(), Build::to_string);
        match (self, form) {
            (Self::Sources { changed, .. }, Form::Full) if windows => format!(
                "ename_mcp is out of date: its sources changed {}, after the last build. Windows \
                 locks the running sidecar, so the build fails while it is connected: ask the \
                 user to disable the `game` server in `/mcp`, run {build}, then ask the user to \
                 enable it again (you cannot do either). {MISSING}",
                ago(changed),
            ),
            (Self::Sources { changed, .. }, Form::Full) => format!(
                "ename_mcp is out of date: its sources changed {}, after the last build. Run \
                 {build}, then ask the user to run `/mcp reconnect` (you cannot). {MISSING}",
                ago(changed),
            ),
            (Self::Sources { .. }, Form::Short) if windows => format!(
                "ename_mcp is out of date: ask the user to disable the `game` server in `/mcp`, \
                 run {build}, then ask the user to enable it again."
            ),
            (Self::Sources { .. }, Form::Short) => format!(
                "ename_mcp is out of date: run {build}, then ask the user to run `/mcp reconnect`."
            ),
            (Self::NewerBuild { running, on_disk }, Form::Full) => format!(
                "ename_mcp is out of date: this process runs the build from {}; a newer build \
                 from {} is on disk. Ask the user to run `/mcp reconnect` (you cannot). {MISSING}",
                ago(running.modified),
                ago(on_disk.modified),
            ),
            (Self::NewerBuild { .. }, Form::Short) => {
                "ename_mcp is out of date; ask the user to run `/mcp reconnect`.".to_owned()
            }
        }
    }
}

/// The two checks, sources first: building is the step that has to come before reconnecting.
/// `sources` is `None` when they are not checked.
fn assess(
    running: Stamp,
    on_disk: Option<Stamp>,
    sources: Option<SystemTime>,
) -> Option<Staleness> {
    if let Some(changed) = sources
        && on_disk.is_none_or(|on_disk| changed > on_disk.modified)
    {
        return Some(Staleness::Sources { changed, on_disk });
    }
    let on_disk = on_disk?;
    (on_disk != running).then_some(Staleness::NewerBuild { running, on_disk })
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

/// How long ago, in the coarsest unit that still says something useful.
///
/// A wall clock would have to be formatted in the reader's timezone, and the reader would then
/// have to subtract to learn the thing that actually matters, which is whether the build moved
/// during this session.
fn ago(time: SystemTime) -> String {
    // A modification time in the future means a clock that disagrees with the filesystem's, not
    // a build that has yet to happen. Reporting it as "just now" beats reporting nothing.
    let seconds = time.elapsed().unwrap_or_default().as_secs();
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
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use super::{Build, Form, Staleness, Stamp, ago, assess, form, relative};

    fn ago_by(seconds: u64) -> String {
        ago(SystemTime::now() - Duration::from_secs(seconds))
    }

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
        assert_eq!(
            ago(SystemTime::now() + Duration::from_secs(600)),
            "less than a minute ago"
        );
    }

    #[test]
    fn up_to_date_says_nothing() {
        assert_eq!(assess(stamp(10), Some(stamp(10)), Some(at(5))), None);
    }

    #[test]
    fn sources_newer_than_the_build_on_disk() {
        assert_eq!(
            assess(stamp(10), Some(stamp(10)), Some(at(20))),
            Some(Staleness::Sources {
                changed: at(20),
                on_disk: Some(stamp(10)),
            })
        );
    }

    #[test]
    fn build_on_disk_newer_than_the_running_one() {
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), Some(at(5))),
            Some(Staleness::NewerBuild {
                running: stamp(10),
                on_disk: stamp(20),
            })
        );
    }

    /// Building has to come first, so the sources win.
    #[test]
    fn both_reports_the_sources() {
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), Some(at(30))),
            Some(Staleness::Sources {
                changed: at(30),
                on_disk: Some(stamp(20)),
            })
        );
    }

    #[test]
    fn without_sources_only_the_build_counts() {
        assert_eq!(assess(stamp(10), Some(stamp(10)), None), None);
        assert_eq!(
            assess(stamp(10), Some(stamp(20)), None),
            Some(Staleness::NewerBuild {
                running: stamp(10),
                on_disk: stamp(20),
            })
        );
    }

    #[test]
    fn a_missing_build_needs_building() {
        assert_eq!(
            assess(stamp(10), None, Some(at(5))),
            Some(Staleness::Sources {
                changed: at(5),
                on_disk: None,
            })
        );
        assert_eq!(assess(stamp(10), None, None), None);
    }

    #[test]
    fn full_once_per_condition_and_on_every_failure() {
        let sources = |changed| Staleness::Sources {
            changed: at(changed),
            on_disk: Some(stamp(10)),
        };
        let rebuilt = Staleness::NewerBuild {
            running: stamp(10),
            on_disk: stamp(40),
        };
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
        let rebuilt = |on_disk| Staleness::NewerBuild {
            running: stamp(10),
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
            dir: PathBuf::from("/w/game"),
        };
        let sources = Staleness::Sources {
            changed: SystemTime::now(),
            on_disk: None,
        };
        let full = sources.text(Form::Full, Some(&build), false);
        assert!(
            full.contains("Run `cargo build -p ename_mcp` in `/w/game`"),
            "{full}"
        );
        assert!(
            full.contains("ask the user to run `/mcp reconnect` (you cannot)"),
            "{full}"
        );
        let windows = sources.text(Form::Full, Some(&build), true);
        assert!(windows.contains("disable the `game` server"), "{windows}");
        let rebuilt = Staleness::NewerBuild {
            running: stamp(10),
            on_disk: stamp(20),
        };
        assert_eq!(
            rebuilt.text(Form::Short, Some(&build), false),
            "ename_mcp is out of date; ask the user to run `/mcp reconnect`."
        );
    }

    #[test]
    fn builds_in_the_workspace_from_inside_it() {
        let build = |cwd: &str| {
            Build::new(
                Path::new("/w/game/target/debug/ename_mcp"),
                Path::new("/w/game"),
                Path::new(cwd),
                false,
            )
            .unwrap()
        };
        let expected = Build {
            command: "cargo build -p ename_mcp".to_owned(),
            dir: PathBuf::from("/w/game"),
        };
        assert_eq!(build("/w/game"), expected);
        assert_eq!(build("/w/game/ename_mcp"), expected);
    }

    /// Unix only, like the next one: the command spells paths with the platform's separator.
    #[cfg(unix)]
    #[test]
    fn builds_through_the_manifest_from_another_workspace() {
        let build = Build::new(
            Path::new("/w/game/target/debug/ename_mcp"),
            Path::new("/w/game"),
            Path::new("/w/airshipgame"),
            false,
        )
        .unwrap();
        assert_eq!(
            build,
            Build {
                command: "cargo build --manifest-path ../game/Cargo.toml -p ename_mcp".to_owned(),
                dir: PathBuf::from("/w/airshipgame"),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn names_the_profile_and_target_dir_that_built_it() {
        let build = |exe: &str, target_dir_env| {
            Build::new(
                Path::new(exe),
                Path::new("/w/game"),
                Path::new("/w/game"),
                target_dir_env,
            )
            .unwrap()
            .command
        };
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
}
