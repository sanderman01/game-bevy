//! Noticing that this process is no longer the build sitting on disk.
//!
//! The agent's client launches this binary once and keeps the process for the whole session, so
//! a `cargo build` halfway through leaves an old image answering new tools. Nothing in MCP says
//! a server may have been replaced, and the symptoms are indirect: a tool argument the schema
//! no longer describes, or a BRP result whose shape the running code predates. Both read as a
//! bug in the game rather than as an out-of-date sidecar, which is the wrong thing to go
//! investigate.
//!
//! So the process watches its own executable and says so. The check is a `stat`, and it uses
//! only what `std::fs::Metadata` reports on every platform: length and modification time. An
//! inode would be the sharper signal, and `MetadataExt` is Unix-only.
//!
//! The state is process-global because that is what it describes -- this executable, not any
//! one connection -- and because the warning has to reach the error path, where there is no
//! server to hang it on.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};

/// The build this process was launched from. `None` when the executable could not be stat'd at
/// startup, which leaves the check silent rather than guessing.
static LAUNCH: OnceLock<Option<Launch>> = OnceLock::new();

/// Records the build this process is running. Call once, before serving.
pub fn record() {
    LAUNCH.get_or_init(Launch::read);
}

/// What to tell the agent, or `None` while this process is still the build on disk.
pub fn warning() -> Option<String> {
    LAUNCH.get()?.as_ref()?.warning()
}

struct Launch {
    exe: PathBuf,
    stamp: Stamp,
}

impl Launch {
    /// Resolves the path once, at startup. On Linux `current_exe` reads `/proc/self/exe`, which
    /// starts reporting a `(deleted)` suffix as soon as cargo replaces the file, so asking again
    /// later would compare against a path that no longer exists.
    fn read() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let stamp = Stamp::read(&exe)?;
        Some(Self { exe, stamp })
    }

    fn warning(&self) -> Option<String> {
        let current = Stamp::read(&self.exe)?;
        if current == self.stamp {
            return None;
        }
        Some(format!(
            "this ename_mcp process is running a build from {}; the binary on disk changed {}. \
             Run `/mcp reconnect` to pick it up.",
            ago(self.stamp.modified),
            ago(current.modified),
        ))
    }
}

/// A build of the executable, in the two facts every platform and filesystem reports.
#[derive(PartialEq, Eq)]
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
    use std::time::{Duration, SystemTime};

    use super::ago;

    fn ago_by(seconds: u64) -> String {
        ago(SystemTime::now() - Duration::from_secs(seconds))
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
}
