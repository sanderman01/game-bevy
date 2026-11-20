//! The content catalog, over real package trees in a temporary directory.
//!
//! The catalog is a view of the resolved index, so every test here states a tree, scans it the way
//! the game does, and asserts on the entries and the report lines.

use ename_asset_alias::IgnoredFiles;
use ename_asset_package::{
    CatalogEntry, ContentCatalog, ContentKinds, ContentReport, Discovery, LoadOrder, ProblemKind,
    build_index_with_catalog, discover_packages, resolve_discovery,
};
use ename_asset_vfs::StdVfs;
use futures_lite::future::block_on;
use std::path::Path;

const SEARCH_PATHS: [&str; 2] = ["game", "mods"];

fn kinds() -> ContentKinds {
    let mut kinds = ContentKinds::default();
    kinds.insert("map", "map.ron").unwrap();
    kinds.insert("landscape", "landscape.ron").unwrap();
    kinds
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A package manifest; `extra` is appended to the `[package]` table.
fn package(root: &Path, dir: &str, extra: &str) {
    let id = dir.rsplit('/').next().unwrap();
    write(
        root,
        &format!("{dir}/package-info.toml"),
        &format!(
            "[package]\nid = \"{id}\"\nversion = \"1.0.0\"\nauthors = []\ntitle = \"{id}\"\n\
             description = \"\"\n{extra}\n"
        ),
    );
}

fn discover(root: &Path) -> Discovery {
    let search_paths: Vec<String> = SEARCH_PATHS.iter().map(|s| (*s).to_owned()).collect();
    block_on(discover_packages(
        &StdVfs::new(root),
        &search_paths,
        &IgnoredFiles::default(),
    ))
}

fn catalog(root: &Path) -> (ContentCatalog, ContentReport) {
    let scan = resolve_discovery(discover(root), &LoadOrder::default());
    let (_, catalog, report) = build_index_with_catalog(&scan, &kinds());
    (catalog, report)
}

fn entry(kind: &str, id: &str, package: &str) -> CatalogEntry {
    CatalogEntry {
        kind: kind.to_owned(),
        id: id.to_owned(),
        package: package.to_owned(),
    }
}

fn problem_kinds(report: &ContentReport) -> Vec<ProblemKind> {
    report.problems.iter().map(|problem| problem.kind).collect()
}

/// The base tree: `core` holds one map and one landscape, and a texture that is no content.
fn base(root: &Path) {
    package(root, "game/core", "");
    write(root, "game/core/island.map.ron", "()");
    write(root, "game/core/island_land.landscape.ron", "()");
    write(root, "game/core/island_colour.png", "");
}

#[test]
fn every_file_with_a_kind_extension_is_an_entry() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    let (catalog, report) = catalog(root.path());

    assert_eq!(
        catalog.entries("map").collect::<Vec<_>>(),
        [&entry("map", "core::island", "core")]
    );
    assert_eq!(
        catalog.get("landscape", "core::island_land"),
        Some(&entry("landscape", "core::island_land", "core"))
    );
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog.entries("vehicle").count(), 0);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[test]
fn a_declared_override_moves_the_entry_to_the_later_package() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/bigisland", r#"overrides = ["core"]"#);
    write(
        root.path(),
        "mods/bigisland/island.map.ron.alias",
        "alias = \"core::island\"\nguid = \"0190d5b0-0000-7000-8000-000000000001\"\n",
    );
    write(root.path(), "mods/bigisland/island.map.ron", "()");
    let (catalog, report) = catalog(root.path());

    assert_eq!(
        catalog.get("map", "core::island"),
        Some(&entry("map", "core::island", "bigisland"))
    );
    assert_eq!(catalog.len(), 2);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.contests.len(), 1);
}

#[test]
fn an_undeclared_override_moves_the_entry_and_warns() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/bigisland", "");
    write(
        root.path(),
        "mods/bigisland/island.map.ron.alias",
        "alias = \"core::island\"\nguid = \"0190d5b0-0000-7000-8000-000000000001\"\n",
    );
    write(root.path(), "mods/bigisland/island.map.ron", "()");
    let (catalog, report) = catalog(root.path());

    assert_eq!(
        catalog
            .get("map", "core::island")
            .map(|e| e.package.as_str()),
        Some("bigisland")
    );
    assert_eq!(problem_kinds(&report), [ProblemKind::UndeclaredOverride]);
}

#[test]
fn a_removed_alias_drops_its_entry_silently() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(
        root.path(),
        "mods/noisland",
        r#"removes = ["core::island"]"#,
    );
    let (catalog, report) = catalog(root.path());

    assert_eq!(catalog.get("map", "core::island"), None);
    assert_eq!(catalog.len(), 1);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// The kind comes from the winning file, so an override with another extension takes the entry out
/// of the old kind, and the report says so.
#[test]
fn an_override_with_another_extension_changes_the_kind_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/flat", r#"overrides = ["core"]"#);
    write(
        root.path(),
        "mods/flat/island.landscape.ron.alias",
        "alias = \"core::island\"\nguid = \"0190d5b0-0000-7000-8000-000000000002\"\n",
    );
    write(root.path(), "mods/flat/island.landscape.ron", "()");
    let (catalog, report) = catalog(root.path());

    assert_eq!(catalog.get("map", "core::island"), None);
    assert_eq!(
        catalog.get("landscape", "core::island"),
        Some(&entry("landscape", "core::island", "flat"))
    );
    assert_eq!(problem_kinds(&report), [ProblemKind::ContentKindChanged]);
    assert!(
        report.problems[0].detail.contains("`map`"),
        "{:?}",
        report.problems[0]
    );
}

/// The alias rule cuts the stem at the first dot, so these two files both claim `core::island`.
/// The walk keeps the first by path and reports the second; with a content file involved the
/// report says so.
#[test]
fn a_duplicate_alias_of_a_content_file_in_one_package_is_its_own_problem() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "game/core", "");
    write(root.path(), "game/core/island.landscape.ron", "()");
    write(root.path(), "game/core/island.map.ron", "()");
    write(root.path(), "game/core/rock.png", "");
    write(root.path(), "game/core/rock.webp", "");
    let (catalog, report) = catalog(root.path());

    let mut kinds = problem_kinds(&report);
    kinds.sort_by_key(|kind| format!("{kind:?}"));
    assert_eq!(
        kinds,
        [
            ProblemKind::DuplicateAlias,
            ProblemKind::DuplicateContentAlias
        ]
    );
    assert_eq!(
        catalog.get("landscape", "core::island"),
        Some(&entry("landscape", "core::island", "core"))
    );
    assert_eq!(catalog.get("map", "core::island"), None);
}

/// A content file and a non-content file with one stem: the walk keeps `island.map.ron`, which
/// sorts first, and the report names the content kind.
#[test]
fn a_content_file_and_a_texture_with_one_stem_are_a_duplicate_content_alias() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "game/core", "");
    write(root.path(), "game/core/island.map.ron", "()");
    write(root.path(), "game/core/island.png", "");
    let (catalog, report) = catalog(root.path());

    assert_eq!(problem_kinds(&report), [ProblemKind::DuplicateContentAlias]);
    assert!(
        report.problems[0]
            .detail
            .contains("a `map` hides a non-content file"),
        "{:?}",
        report.problems[0]
    );
    assert_eq!(
        catalog.get("map", "core::island"),
        Some(&entry("map", "core::island", "core"))
    );
}

/// The browser reads a baked discovery instead of walking, and must end up with the same catalog.
#[test]
fn a_baked_discovery_gives_the_same_catalog() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/flat", r#"overrides = ["core"]"#);
    write(
        root.path(),
        "mods/flat/island.landscape.ron.alias",
        "alias = \"core::island\"\nguid = \"0190d5b0-0000-7000-8000-000000000002\"\n",
    );
    write(root.path(), "mods/flat/island.landscape.ron", "()");
    write(root.path(), "mods/flat/dupe.map.ron", "()");
    write(root.path(), "mods/flat/dupe.png", "");

    let live = discover(root.path());
    let baked = Discovery::decode(&live.encode().unwrap()).unwrap();
    let build = |discovery| {
        let scan = resolve_discovery(discovery, &LoadOrder::default());
        build_index_with_catalog(&scan, &kinds())
    };
    let (_, live_catalog, live_report) = build(live);
    let (_, baked_catalog, baked_report) = build(baked);

    assert_eq!(baked_catalog, live_catalog);
    assert_eq!(baked_catalog.len(), 3);
    assert_eq!(problem_kinds(&baked_report), problem_kinds(&live_report));
    assert!(problem_kinds(&baked_report).contains(&ProblemKind::DuplicateContentAlias));
}

/// `dir` claims `alias` with `file`, through a `.alias` sidecar.
fn claim(root: &Path, dir: &str, file: &str, alias: &str, guid: u8) {
    write(
        root,
        &format!("{dir}/{file}.alias"),
        &format!("alias = \"{alias}\"\nguid = \"0190d5b0-0000-7000-8000-0000000000{guid:02x}\"\n"),
    );
    write(root, &format!("{dir}/{file}"), "()");
}

/// Removal is a step in load order: a later package may claim the alias back, and the entry is
/// its file's.
#[test]
fn an_alias_removed_and_claimed_back_with_the_same_kind_is_the_new_packages() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(
        root.path(),
        "mods/a_remove",
        r#"removes = ["core::island"]"#,
    );
    package(root.path(), "mods/b_back", r#"overrides = ["core"]"#);
    claim(
        root.path(),
        "mods/b_back",
        "island.map.ron",
        "core::island",
        3,
    );
    let (catalog, report) = catalog(root.path());

    assert_eq!(
        catalog.get("map", "core::island"),
        Some(&entry("map", "core::island", "b_back"))
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// After a `removes` the alias starts over, so claiming it back with another kind is a new entry
/// and no kind change.
#[test]
fn an_alias_removed_and_claimed_back_with_another_kind_reports_no_kind_change() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(
        root.path(),
        "mods/a_remove",
        r#"removes = ["core::island"]"#,
    );
    package(root.path(), "mods/b_back", r#"overrides = ["core"]"#);
    claim(
        root.path(),
        "mods/b_back",
        "island.landscape.ron",
        "core::island",
        4,
    );
    let (catalog, report) = catalog(root.path());

    assert_eq!(catalog.get("map", "core::island"), None);
    assert_eq!(
        catalog.get("landscape", "core::island"),
        Some(&entry("landscape", "core::island", "b_back"))
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// A map replaced by a texture and then by a map again is still a map: no kind change to report.
#[test]
fn a_kind_change_that_a_later_package_reverts_reports_nothing() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/b_png", r#"overrides = ["core"]"#);
    claim(root.path(), "mods/b_png", "island.png", "core::island", 5);
    package(root.path(), "mods/c_map", r#"overrides = ["b_png"]"#);
    claim(
        root.path(),
        "mods/c_map",
        "island.map.ron",
        "core::island",
        6,
    );
    let (catalog, report) = catalog(root.path());

    assert_eq!(
        catalog.get("map", "core::island"),
        Some(&entry("map", "core::island", "c_map"))
    );
    assert_eq!(report.contests.len(), 2);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// A package whose `requires` fails is not loaded, so its content is not in the catalog.
#[test]
fn a_disabled_packages_content_is_not_catalogued() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    package(root.path(), "mods/needy", r#"requires = ["absent >= 1.0"]"#);
    write(root.path(), "mods/needy/cove.map.ron", "()");
    let (catalog, report) = catalog(root.path());

    assert_eq!(catalog.get("map", "needy::cove"), None);
    assert_eq!(catalog.len(), 2);
    assert_eq!(report.disabled.len(), 1);
}

/// A hidden file has no stem before its first dot, so it is no content.
#[test]
fn a_hidden_file_is_not_content() {
    let root = tempfile::tempdir().unwrap();
    base(root.path());
    write(root.path(), "game/core/.map.ron", "()");
    let (catalog, _) = catalog(root.path());

    assert_eq!(catalog.len(), 2);
    assert!(catalog.iter().all(|entry| entry.id != "core::"));
}
