//! How a game that cannot be reached died: its run record, and the known crash it matches.
//!
//! The game keeps the record (`ename_remote::run_record`) at a path both sides derive from the BRP
//! port, so this works for a game the user started by hand too. This crate names no first-party
//! crate, so it reads the format on its own; both sides test against
//! `tests/fixtures/run_record.json`.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

use crate::launch::process;

/// Where known crashes are listed: a path list (`:` on Unix, `;` on Windows) relative to the
/// sidecar's working directory. Defaults to [`DEFAULT_KNOWN_CRASHES`].
const KNOWN_CRASHES_VAR: &str = "ENAME_KNOWN_CRASHES";
const DEFAULT_KNOWN_CRASHES: &str = "docs/known-crashes.md";

/// Seconds a process's start time may read later than the game recorded it.
const START_SLACK_SECONDS: u64 = 2;

/// `<runtime dir>/ename`, else `<temp dir>/ename`: where `ename_remote` writes run records.
pub fn record_dir() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ename")
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunRecord {
    pub run: String,
    pub os_pid: u32,
    #[serde(default)]
    pub exe: Option<PathBuf>,
    pub started_unix_ms: u64,
    #[serde(default)]
    pub panic: Option<PanicRecord>,
    #[serde(default)]
    pub panics: u64,
    #[serde(default)]
    pub exit: Option<ExitRecord>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PanicRecord {
    pub message: String,
    #[serde(default)]
    pub location: Option<String>,
    pub thread: String,
    pub at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExitRecord {
    /// 0 for `AppExit::Success`.
    pub code: u8,
    pub at_unix_ms: u64,
}

impl RunRecord {
    /// The record of whichever game last held `port`.
    pub fn read(dir: &Path, port: u16) -> Option<Self> {
        let text = std::fs::read(dir.join(format!("brp-{port}.json"))).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Whether the recorded process still runs: same pid, same executable, and not started after
    /// the game wrote the record, which rules out a recycled pid.
    pub fn is_alive(&self) -> bool {
        let Some(info) = process::info(self.os_pid) else {
            return false;
        };
        let same_exe = match (&self.exe, &info.exe) {
            (Some(recorded), Some(running)) => recorded == running,
            _ => true,
        };
        same_exe && info.start_time <= self.started_unix_ms / 1000 + START_SLACK_SECONDS
    }

    /// The panic that killed the game: the latest one, unless the game exited cleanly after it.
    pub fn fatal_panic(&self) -> Option<&PanicRecord> {
        self.exit.is_none().then_some(self.panic.as_ref()).flatten()
    }
}

/// The sentence for a call to the game on `port` that got no answer, or `None` without a record.
/// `last_error` is the newest error this session saw the game log.
pub fn unreachable(dir: &Path, port: u16, last_error: Option<&str>) -> Option<String> {
    let record = RunRecord::read(dir, port)?;
    Some(describe(&record, port, record.is_alive(), last_error))
}

fn describe(record: &RunRecord, port: u16, alive: bool, last_error: Option<&str>) -> String {
    let run = &record.run;
    let mut text = if let Some(exit) = &record.exit {
        let how = match exit.code {
            0 => "`AppExit::Success` (for example its window was closed)".to_owned(),
            code => format!("`AppExit::Error({code})`"),
        };
        let mut text = format!(
            "The game (run {run}) exited {} ago with {how}.",
            ago(exit.at_unix_ms)
        );
        if let Some(panic) = &record.panic {
            text += &format!(" It had survived a panic earlier: {}.", short_panic(panic));
        }
        text
    } else if alive {
        let mut text = format!(
            "The game process (pid {}) is running but not answering on {port}: still starting, \
             or hung.",
            record.os_pid
        );
        if let Some(panic) = &record.panic {
            text += &format!(" Its latest panic: {}.", short_panic(panic));
        }
        text
    } else if let Some(panic) = &record.panic {
        let mut text = format!(
            "The game (run {run}) panicked {} ago {}.",
            ago(panic.at_unix_ms),
            short_panic(panic)
        );
        let texts = [
            panic.message.as_str(),
            panic.location.as_deref().unwrap_or(""),
        ];
        if let Some(link) = known_crash(&texts) {
            text += &format!(" Known crash: {link}.");
        }
        text
    } else {
        let mut text = format!(
            "The game (run {run}) ended without a panic or a clean exit: killed, or crashed in \
             native code."
        );
        if let Some(error) = last_error {
            text += &format!(" Last error this session saw: {error}");
            if let Some(link) = known_crash(&[error]) {
                text += &format!(" Known crash: {link}.");
            }
        }
        text
    };
    text += " game_status says more, and game_start starts it again.";
    text
}

/// "at `light.rs:2928`: message".
fn short_panic(panic: &PanicRecord) -> String {
    match &panic.location {
        Some(location) => {
            let file = location.rsplit(['/', '\\']).next().unwrap_or(location);
            // `file:line:column`, column dropped.
            let file = file.rsplit_once(':').map_or(file, |(rest, _)| rest);
            format!("at `{file}`: {}", panic.message)
        }
        None => format!(": {}", panic.message),
    }
}

fn ago(at_unix_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let seconds = now.saturating_sub(at_unix_ms) / 1000;
    match seconds {
        0..120 => format!("{seconds} s"),
        120..7200 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}

/// `docs/known-crashes.md#slug` for the first known crash whose signature one of `texts`
/// contains.
pub fn known_crash(texts: &[&str]) -> Option<String> {
    let paths = std::env::var_os(KNOWN_CRASHES_VAR)
        .map(|list| std::env::split_paths(&list).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![PathBuf::from(DEFAULT_KNOWN_CRASHES)]);
    paths.iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        parse_known_crashes(&text)
            .into_iter()
            .find(|(_, signature)| texts.iter().any(|text| text.contains(signature.as_str())))
            .map(|(heading, _)| format!("{}#{}", path.display(), slug(&heading)))
    })
}

/// `(heading, signature)` for every `## heading` followed by a `Signature:` line. A heading with
/// no signature is skipped, with a warning.
fn parse_known_crashes(text: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut heading: Option<&str> = None;
    let warn_unsigned = |heading: Option<&str>| {
        if let Some(heading) = heading {
            tracing::warn!("known crash `{heading}` has no `Signature:` line; skipped");
        }
    };
    for line in text.lines() {
        if let Some(next) = line.strip_prefix("## ") {
            warn_unsigned(heading.replace(next.trim()));
        } else if let Some(rest) = line.strip_prefix("Signature:")
            && let Some(current) = heading.take()
        {
            let rest = rest.trim();
            let signature = rest
                .strip_prefix('`')
                .and_then(|inner| inner.split_once('`'))
                .map_or(rest, |(inner, _)| inner);
            if !signature.is_empty() {
                entries.push((current.to_owned(), signature.to_owned()));
            }
        }
    }
    warn_unsigned(heading);
    entries
}

/// GitHub's heading anchor: lowercase, punctuation dropped, spaces as `-`.
fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/run_record.json"
    );

    fn fixture() -> RunRecord {
        serde_json::from_slice(&std::fs::read(FIXTURE).unwrap()).unwrap()
    }

    #[test]
    fn a_survived_panic_then_a_clean_exit_reports_the_exit() {
        let record = fixture();
        assert!(record.fatal_panic().is_none());
        let text = describe(&record, 15702, false, None);
        assert!(
            text.contains("exited") && text.contains("AppExit::Success"),
            "{text}"
        );
        assert!(text.contains("survived a panic earlier"), "{text}");
        assert!(text.contains("at `light.rs:2928`"), "{text}");
    }

    #[test]
    fn an_error_exit_names_its_code() {
        let mut record = fixture();
        record.panic = None;
        record.exit.as_mut().unwrap().code = 3;
        let text = describe(&record, 15702, false, None);
        assert!(text.contains("`AppExit::Error(3)`"), "{text}");
        assert!(!text.contains("panic"), "{text}");
    }

    #[test]
    fn a_live_process_is_hung_or_starting() {
        let mut record = fixture();
        record.exit = None;
        let text = describe(&record, 15702, true, None);
        assert!(
            text.contains("pid 41237") && text.contains("not answering on 15702"),
            "{text}"
        );
    }

    #[test]
    fn a_dead_process_with_a_panic_panicked() {
        let mut record = fixture();
        record.exit = None;
        assert!(record.fatal_panic().is_some());
        let text = describe(&record, 15702, false, None);
        assert!(
            text.contains("panicked") && text.contains("visible entities for cascade"),
            "{text}"
        );
    }

    #[test]
    fn a_dead_process_without_either_was_killed() {
        let mut record = fixture();
        record.exit = None;
        record.panic = None;
        let text = describe(&record, 15702, false, Some("render: device lost"));
        assert!(text.contains("killed, or crashed in native code"), "{text}");
        assert!(text.contains("device lost"), "{text}");
    }

    #[test]
    fn this_process_is_alive_and_a_recycled_pid_is_not() {
        let me = process::info(std::process::id()).unwrap();
        let mut record = fixture();
        record.os_pid = std::process::id();
        record.exe = me.exe;
        record.started_unix_ms = me.start_time * 1000;
        assert!(record.is_alive());
        record.started_unix_ms = (me.start_time - 60) * 1000;
        assert!(!record.is_alive(), "started after the record");
    }

    #[test]
    fn known_crashes_parse_and_slug() {
        let text = "# Known crashes\n\n## Sun shadow maps toggled back on\n\nSignature: \
                    `Failed to get directional light`\n\nBody.\n\n## Unsigned one\n\nNo \
                    signature.\n\n## Window resize (Wayland)\nSignature: plain text\n";
        assert_eq!(
            parse_known_crashes(text),
            [
                (
                    "Sun shadow maps toggled back on".to_owned(),
                    "Failed to get directional light".to_owned()
                ),
                (
                    "Window resize (Wayland)".to_owned(),
                    "plain text".to_owned()
                ),
            ]
        );
        assert_eq!(
            slug("Sun shadow maps toggled back on"),
            "sun-shadow-maps-toggled-back-on"
        );
        assert_eq!(slug("Window resize (Wayland)"), "window-resize-wayland");
    }
}
