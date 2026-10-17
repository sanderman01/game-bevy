//! The `log` notice: new errors and warnings the game logged since the last result said so.
//!
//! The game sends a cumulative tally beside every run id (`ename_remote::logs::Tally`); this keeps
//! the highest one each run has reported to the agent and turns the difference into a notice. Tool
//! calls run concurrently, so an older tally can arrive after a newer one: a tally at or below the
//! mark adds nothing and never lowers it. In memory only, so a new sidecar process repeats the
//! run's tally once.

use std::{collections::VecDeque, sync::Mutex};

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Runs whose marks are kept, so a late answer from before a restart finds its own.
const RUNS_KEPT: usize = 4;

/// The tally as the game sends it. Absent from a game that predates it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Tally {
    pub errors: u64,
    pub warnings: u64,
    #[serde(default)]
    pub last_sequence: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_warning: Option<String>,
}

/// What a result carries when the game logged something new at WARN or ERROR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct LogNotice {
    pub new_errors: u64,
    pub new_warnings: u64,
    /// The newest error, `target: message`, cut short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The newest warning, only when there are no new errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_warning: Option<String>,
    /// log_get_entries with this `after_sequence` and `min_level` WARN lists exactly the new
    /// lines. Absent for the first notice of a run: read from the start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<u64>,
}

impl LogNotice {
    /// The notice as a sentence, for a failed result's text.
    pub fn sentence(&self) -> String {
        let mut text = format!(
            "The game logged {} new error(s) and {} new warning(s)",
            self.new_errors, self.new_warnings
        );
        if let Some(last) = self.last_error.as_ref().or(self.last_warning.as_ref()) {
            text += &format!("; the newest: {last}");
        }
        text += ". log_get_entries with min_level WARN";
        if let Some(after) = self.after_sequence {
            text += &format!(" and after_sequence {after}");
        }
        text + " lists them."
    }
}

#[derive(Clone, Copy, Default)]
struct Mark {
    errors: u64,
    warnings: u64,
    last_sequence: Option<u64>,
}

#[derive(Default)]
pub struct NoticeState {
    /// Oldest first.
    marks: Mutex<VecDeque<(String, Mark)>>,
}

impl NoticeState {
    /// The notice for a result from `run` carrying `tally`, advancing the run's mark. `None` when
    /// nothing is new.
    pub fn notice(&self, run: &str, tally: &Tally) -> Option<LogNotice> {
        let mut marks = self.marks.lock().unwrap();
        let index = match marks.iter().position(|(known, _)| known == run) {
            Some(index) => index,
            None => {
                if marks.len() == RUNS_KEPT {
                    marks.pop_front();
                }
                marks.push_back((run.to_owned(), Mark::default()));
                marks.len() - 1
            }
        };
        let mark = &mut marks[index].1;
        let new_errors = tally.errors.saturating_sub(mark.errors);
        let new_warnings = tally.warnings.saturating_sub(mark.warnings);
        if new_errors == 0 && new_warnings == 0 {
            return None;
        }
        let notice = LogNotice {
            new_errors,
            new_warnings,
            last_error: tally.last_error.clone().filter(|_| new_errors > 0),
            last_warning: tally
                .last_warning
                .clone()
                .filter(|_| new_errors == 0 && new_warnings > 0),
            after_sequence: mark.last_sequence,
        };
        *mark = Mark {
            errors: mark.errors.max(tally.errors),
            warnings: mark.warnings.max(tally.warnings),
            last_sequence: mark.last_sequence.max(tally.last_sequence),
        };
        Some(notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(errors: u64, warnings: u64, last_sequence: u64) -> Tally {
        Tally {
            errors,
            warnings,
            last_sequence: Some(last_sequence),
            last_error: Some(format!("e{errors}")),
            last_warning: Some(format!("w{warnings}")),
        }
    }

    #[test]
    fn reports_deltas_once() {
        let state = NoticeState::default();
        assert_eq!(state.notice("a", &tally(0, 0, 10)), None);
        let first = state.notice("a", &tally(1, 2, 20)).unwrap();
        assert_eq!((first.new_errors, first.new_warnings), (1, 2));
        assert_eq!(first.last_error.as_deref(), Some("e1"));
        assert_eq!(first.last_warning, None, "errors win");
        assert_eq!(
            first.after_sequence, None,
            "the first notice reads from the start"
        );
        assert_eq!(state.notice("a", &tally(1, 2, 20)), None);

        let second = state.notice("a", &tally(1, 5, 30)).unwrap();
        assert_eq!((second.new_errors, second.new_warnings), (0, 3));
        assert_eq!(second.last_warning.as_deref(), Some("w5"));
        assert_eq!(second.after_sequence, Some(20));
    }

    #[test]
    fn a_late_answer_neither_notices_nor_lowers_the_mark() {
        let state = NoticeState::default();
        state.notice("a", &tally(3, 0, 30)).unwrap();
        assert_eq!(state.notice("a", &tally(1, 0, 10)), None, "reversed order");
        let next = state.notice("a", &tally(4, 0, 40)).unwrap();
        assert_eq!(next.new_errors, 1);
        assert_eq!(next.after_sequence, Some(30));
    }

    #[test]
    fn each_run_has_its_own_baseline() {
        let state = NoticeState::default();
        state.notice("a", &tally(3, 0, 30)).unwrap();
        let restarted = state.notice("b", &tally(1, 0, 5)).unwrap();
        assert_eq!(restarted.new_errors, 1);
        // A pre-restart answer arriving late finds run a's mark, not b's.
        assert_eq!(state.notice("a", &tally(3, 0, 30)), None);
        assert_eq!(state.notice("b", &tally(1, 0, 5)), None);
    }
}
