//! What a build said, from cargo's JSON messages (`build.jsonl`) and its stderr (`build.log`).

use std::path::Path;

use serde::Deserialize;

/// How many errors a result carries in full.
const MAX_ERRORS: usize = 10;
/// How much of `build.log` a failure without compiler errors carries.
const LOG_TAIL_LINES: usize = 20;

#[derive(Debug, Default, PartialEq)]
pub struct BuildReport {
    /// Cargo's short rendering of each error, `path:line:col: error[E0425]: message`, at most
    /// [`MAX_ERRORS`].
    pub errors: Vec<String>,
    pub error_count: usize,
    pub warning_count: usize,
}

impl BuildReport {
    /// Reads `build.jsonl`, falling back to the tail of `build.log` when the build failed for a
    /// reason that is not a compiler error (lock file, network, unknown feature).
    pub fn read(jsonl: &Path, log: &Path) -> Self {
        let mut report = std::fs::read_to_string(jsonl)
            .map(|text| Self::parse(&text))
            .unwrap_or_default();
        if report.error_count == 0 {
            report.errors = tail(log, LOG_TAIL_LINES);
        }
        report
    }

    pub fn parse(jsonl: &str) -> Self {
        #[derive(Deserialize)]
        struct Line {
            reason: String,
            #[serde(default)]
            message: Option<Diagnostic>,
        }
        #[derive(Deserialize)]
        struct Diagnostic {
            level: String,
            message: String,
            #[serde(default)]
            rendered: Option<String>,
        }

        let mut report = Self::default();
        let diagnostics = jsonl
            .lines()
            .filter_map(|line| serde_json::from_str::<Line>(line).ok())
            .filter(|line| line.reason == "compiler-message")
            .filter_map(|line| line.message);
        for diagnostic in diagnostics {
            // rustc's own totals ("2 warnings emitted", "aborting due to 1 previous error") are
            // diagnostics too.
            if is_summary(&diagnostic.message) {
                continue;
            }
            match diagnostic.level.as_str() {
                "error" | "error: internal compiler error" => {
                    report.error_count += 1;
                    if report.errors.len() < MAX_ERRORS {
                        let rendered = diagnostic.rendered.unwrap_or(diagnostic.message);
                        report.errors.push(rendered.trim_end().to_owned());
                    }
                }
                "warning" => report.warning_count += 1,
                _ => {}
            }
        }
        report
    }
}

fn is_summary(message: &str) -> bool {
    message.starts_with("aborting due to")
        || (message.ends_with(" emitted")
            && message
                .split_whitespace()
                .next()
                .is_some_and(|count| count.chars().all(|c| c.is_ascii_digit())))
}

/// The last `lines` non-empty lines of a text file.
pub fn tail(path: &Path, lines: usize) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::BuildReport;

    /// Recorded from `cargo build --message-format=json-diagnostic-short` with an undefined name.
    const FAILED: &str = r#"{"reason":"compiler-artifact","package_id":"path+file:///w/a#0.0.0","target":{"name":"a"},"fresh":true}
{"reason":"compiler-message","package_id":"path+file:///w/ed#0.0.0","manifest_path":"/w/ed/Cargo.toml","target":{"kind":["bin"],"crate_types":["bin"],"name":"ed","src_path":"/w/ed/src/main.rs","edition":"2024","doc":true,"doctest":false,"test":true},"message":{"rendered":"ed/src/main.rs:7:5: error[E0425]: cannot find value `nope` in this scope\n","$message_type":"diagnostic","children":[],"code":{"code":"E0425","explanation":null},"level":"error","message":"cannot find value `nope` in this scope","spans":[]}}
{"reason":"compiler-message","package_id":"path+file:///w/ed#0.0.0","manifest_path":"/w/ed/Cargo.toml","target":{"kind":["bin"],"crate_types":["bin"],"name":"ed","src_path":"/w/ed/src/main.rs","edition":"2024","doc":true,"doctest":false,"test":true},"message":{"rendered":"ed/src/main.rs:3:9: warning: unused variable: `x`\n","$message_type":"diagnostic","children":[],"code":{"code":"unused_variables","explanation":null},"level":"warning","message":"unused variable: `x`","spans":[]}}
{"reason":"compiler-message","package_id":"path+file:///w/ed#0.0.0","manifest_path":"/w/ed/Cargo.toml","target":{"kind":["bin"],"crate_types":["bin"],"name":"ed","src_path":"/w/ed/src/main.rs","edition":"2024","doc":true,"doctest":false,"test":true},"message":{"rendered":"error: aborting due to 1 previous error; 1 warning emitted\n","$message_type":"diagnostic","children":[],"code":null,"level":"error","message":"aborting due to 1 previous error; 1 warning emitted","spans":[]}}
{"reason":"build-finished","success":false}
"#;

    #[test]
    fn reads_errors_in_cargo_short_form_and_counts() {
        let report = BuildReport::parse(FAILED);
        assert_eq!(
            report.errors,
            ["ed/src/main.rs:7:5: error[E0425]: cannot find value `nope` in this scope"]
        );
        assert_eq!(report.error_count, 1);
        assert_eq!(report.warning_count, 1);
    }

    #[test]
    fn a_clean_build_has_nothing_to_report() {
        let report = BuildReport::parse(r#"{"reason":"build-finished","success":true}"#);
        assert_eq!(report, BuildReport::default());
    }
}
