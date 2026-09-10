//! `ename_check` -- fails loudly on anything a released content tree should not ship with.

use ename_asset_package::{ContentReport, build_index};
use std::path::Path;

/// Runs the scan and prints a report. Returns whether the tree is clean.
///
/// A contest is not always a mistake -- an author asking to override a package is the system
/// working -- but this is the CI gate, not the game's own tolerant scan: every problem
/// `build_index` found and every alias two packages both claimed fail it, because "does this
/// repo's content check out clean" is a question CI should never answer with a guess about which
/// contest was deliberate. Confirming an intentional override is what `ename_list` is for.
pub fn check(asset_root: &Path, search_paths: &[String], load_order_path: Option<&Path>) -> bool {
    let scan = crate::scan::scan(asset_root, search_paths, load_order_path);
    let (_, report) = build_index(&scan);
    print_report(&report);
    report.problems.is_empty() && report.contests.is_empty()
}

fn print_report(report: &ContentReport) {
    println!(
        "{} packages, {} disabled, {} problems, {} contested aliases",
        report.packages.len(),
        report.disabled.len(),
        report.problems.len(),
        report.contests.len()
    );
    for disabled in &report.disabled {
        println!("  -- {} disabled: {}", disabled.id, disabled.reason);
    }
    for problem in &report.problems {
        println!(
            "  !! {} at {}: {}",
            problem.kind,
            problem.path.display(),
            problem.detail
        );
    }
    for contest in &report.contests {
        println!("  ?? {contest}");
    }
}
