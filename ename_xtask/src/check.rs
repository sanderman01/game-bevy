//! `ename_check` -- fails loudly on anything a released content tree should not ship with.

use ename_asset_package::{ContentCatalog, ContentKinds, ContentReport, build_index_with_catalog};
use std::path::Path;

/// Runs the scan and prints a report. Returns whether the tree is clean.
///
/// A contest is not always a mistake -- an author asking to override a package is the system
/// working -- but this is the CI gate, not the game's own tolerant scan: every problem
/// `build_index` found and every alias two packages both claimed fail it, because "does this
/// repo's content check out clean" is a question CI should never answer with a guess about which
/// contest was deliberate. Confirming an intentional override is what `ename_content_list` is for.
///
/// `kinds` names the content kinds, so a duplicate alias that hides a content file and an override
/// that changes an entry's kind are reported as such, and the catalog is listed. Whether each
/// descriptor loads is the game's to check: this tool has no loaders.
pub fn check(
    asset_root: &Path,
    search_paths: &[String],
    load_order_path: Option<&Path>,
    kinds: &ContentKinds,
) -> bool {
    let (scan, _) = crate::scan::scan(asset_root, search_paths, load_order_path);
    let (_, catalog, report) = build_index_with_catalog(&scan, kinds);
    print_report(&report);
    print_catalog(&catalog);
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

fn print_catalog(catalog: &ContentCatalog) {
    for (kind, extension) in catalog.kinds().iter() {
        println!(
            "{kind} (*.{extension}): {} entries",
            catalog.entries(kind).count()
        );
        for entry in catalog.entries(kind) {
            println!("  {:32} {}", entry.id, entry.package);
        }
    }
}
