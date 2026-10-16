//! How a game exited (`exit.json`), and the crash block a failed run reports.

use std::{path::Path, process::ExitStatus};

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::build_report::tail;

/// How much of `game.log` a crash without a panic record carries.
const LOG_TAIL_LINES: usize = 20;
/// The game's exit code when its BRP port is taken (`ename_remote::port`).
const PORT_IN_USE_EXIT_CODE: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// A stop was requested.
    Requested,
    /// Exit code 0, e.g. the window was closed.
    Clean,
    /// Any other exit code.
    Error,
    /// Killed by a signal (Unix) or a Windows exception code.
    Crash,
}

/// `exit.json`. Readers ignore it when `generation` is not the run they ask about.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExitInfo {
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// The signal's name, Unix only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub reason: ExitReason,
    /// Unix seconds.
    pub at: u64,
    /// For a requested stop: whether the game exited by itself within the grace period.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graceful: Option<bool>,
}

impl ExitInfo {
    pub fn from_status(generation: u64, status: ExitStatus, requested: bool) -> Self {
        let signal = signal_of(status);
        let code = status.code();
        let reason = if requested {
            ExitReason::Requested
        } else if signal.is_some() || code.is_some_and(is_exception) {
            ExitReason::Crash
        } else if code == Some(0) {
            ExitReason::Clean
        } else {
            ExitReason::Error
        };
        Self {
            generation,
            code,
            signal,
            reason,
            at: super::now(),
            graceful: None,
        }
    }

    /// `code 101`, `SIGSEGV`, `exception 0xC0000005 (access violation)`.
    pub fn describe(&self) -> String {
        if let Some(signal) = &self.signal {
            return format!("killed by {signal}");
        }
        match self.code {
            Some(code) if is_exception(code) => {
                let code = code as u32;
                match exception_name(code) {
                    Some(name) => format!("exception {code:#010X} ({name})"),
                    None => format!("exception {code:#010X}"),
                }
            }
            Some(PORT_IN_USE_EXIT_CODE) => {
                format!("code {PORT_IN_USE_EXIT_CODE} (its BRP port is in use)")
            }
            Some(code) => format!("code {code}"),
            None => "unknown status".to_owned(),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self.reason, ExitReason::Error | ExitReason::Crash)
    }
}

/// What a run that exited with an error or a crash reports.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct Crash {
    /// One line: the exit code, signal or exception.
    pub cause: String,
    /// The last lines of `game.log`: without a panic record, the only clue.
    pub log_tail: Vec<String>,
    pub log: String,
}

impl Crash {
    pub fn new(exit: &ExitInfo, log: &Path) -> Self {
        Self {
            cause: exit.describe(),
            log_tail: tail(log, LOG_TAIL_LINES),
            log: log.display().to_string(),
        }
    }
}

/// Windows exception codes (`NTSTATUS` errors) are 0xC0000000 and up.
fn is_exception(code: i32) -> bool {
    cfg!(windows) && (code as u32) >= 0xC000_0000
}

fn exception_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0xC000_0005 => "access violation",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer divide by zero",
        0xC000_00FD => "stack overflow",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "stack buffer overrun or abort",
        0xC000_013A => "terminated by Ctrl+C",
        _ => return None,
    })
}

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt as _;
    let signal = status.signal()?;
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 if cfg!(target_os = "linux") => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 if !cfg!(target_os = "linux") => "SIGBUS",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => return Some(format!("signal {signal}")),
    };
    Some(name.to_owned())
}

#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<String> {
    None
}
