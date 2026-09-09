// The whole file is Bevy: the scan plugin only exists under the `bevy` feature, and the bevy-free
// build still has to compile its test targets.
#![cfg(feature = "bevy")]

//! What a consumer of this crate gets for adding `AliasPlugins` and nothing else.
//!
//! `alias_reader.rs` fills the index by hand, which is the `ename_asset_content` shape. This file
//! is the other half: no other crate, no manifests, no registration code -- a `_rules.toml` and a
//! couple of `.alias` files in the asset tree, and `asset_server.load("alias://fixture::plain")`
//! works. The fixture tree under `tests/fixtures/files` is the whole configuration.
//!
//! Headless: `TaskPoolPlugin` supplies the pools and `AssetPlugin` the asset system. No window,
//! no renderer.

mod bevy_support;

use bevy::{
    app::{App, TaskPoolPlugin},
    asset::{AssetPlugin, AssetServer, Handle, LoadState},
};
use bevy_support::{
    MAX_FRAMES, Shout, Text, assert_loaded, register_test_assets, run_until_settled, shout, text,
};
use ename_asset_alias::{AliasPlugins, AliasScan, ProblemKind};
use std::{path::Path, time::Duration};

/// Where the fixtures live, relative to the workspace root. `BEVY_ASSET_ROOT` is pinned to the
/// workspace root in `.cargo/config.toml`, and Cargo applies `[env]` to `cargo test`, so this
/// resolves the same way from any working directory.
const FIXTURE_ROOT: &str = "crates/ename_asset_alias/tests/fixtures";

/// A headless `App` carrying nothing but the alias plugins and two toy asset types.
///
/// `AliasPlugins` goes in before `AssetPlugin` because it registers an asset source, and
/// `AssetPlugin` turns registered sources into live ones exactly once, when it builds.
fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(AliasPlugins::default().with_asset_root(FIXTURE_ROOT))
        .add_plugins(TaskPoolPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: FIXTURE_ROOT.to_owned(),
            ..Default::default()
        });
    register_test_assets(&mut app);
    app
}

/// Runs frames until the scan has been mirrored into the `World`.
fn run_until_scanned(app: &mut App) -> AliasScan {
    for _ in 0..MAX_FRAMES {
        app.update();
        if let Some(scan) = app.world().get_resource::<AliasScan>() {
            return scan.clone();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the scan was never mirrored within {MAX_FRAMES} frames");
}

fn aliases(scan: &AliasScan) -> Vec<&str> {
    scan.assets.iter().map(|a| a.alias.as_str()).collect()
}

// --- tests ----------------------------------------------------------------------------------

/// The headline claim of the crate: a consumer adds one plugin group, writes a `_rules.toml`, and
/// addresses the file by alias. Nothing here registers an alias by hand.
#[test]
fn adding_the_plugins_is_enough_to_load_an_asset_by_alias() {
    let mut app = test_app();

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://fixture::plain");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "plain");
}

/// The scan runs on `Startup` and finishes on the io pool some frames later. A handle taken before
/// any of that has happened must still resolve: the reader awaits the index inside `bevy_asset`,
/// so a consumer never needs a loading state of their own.
#[test]
fn a_handle_requested_before_the_scan_finishes_still_loads() {
    let mut app = test_app();

    // No `app.update()` has run, so `Startup` has not even spawned the scan yet.
    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://fixture::plain");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "plain");
}

/// `replacement.txt.alias` names `fixture::override_me`. The sidecar replaces what the folder rule
/// would have derived rather than adding to it, so the derived name must be gone.
#[test]
fn a_sidecar_alias_replaces_the_one_the_folder_rule_would_derive() {
    let mut app = test_app();
    let server = app.world().resource::<AssetServer>().clone();

    let overridden: Handle<Text> = server.load("alias://fixture::override_me");
    assert_loaded(run_until_settled(&mut app, &overridden));
    assert_eq!(text(&app, &overridden), "replacement");

    let derived: Handle<Text> = server.load("alias://fixture::replacement");
    assert!(
        matches!(run_until_settled(&mut app, &derived), LoadState::Failed(_)),
        "the rule's name must not survive alongside the sidecar's"
    );
}

/// `files/props/_rules.toml` replaces the rule it inherited, so the walk has to descend and to
/// prefer the deeper file. `fixture::props/barrel`, not `fixture::barrel`.
#[test]
fn a_rule_in_a_subdirectory_replaces_the_inherited_one() {
    let mut app = test_app();

    let handle: Handle<Text> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://fixture::props/barrel");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(text(&app, &handle), "barrel");
}

/// The folder rule includes only `*.txt`, and `noisy.shout` is addressable anyway because a
/// `.alias` file names it. Loading it by label also exercises the synthesized `.meta`: an alias
/// carries no extension, and a labelled `AssetPath` makes `bevy_asset` skip the by-asset-type
/// loader lookup, so the two features have to work together or this fails.
#[test]
fn a_sidecar_includes_a_file_the_folder_rule_excludes() {
    let mut app = test_app();

    let handle: Handle<Shout> = app
        .world()
        .resource::<AssetServer>()
        .load("alias://fixture::noisy#Loud");

    assert_loaded(run_until_settled(&mut app, &handle));
    assert_eq!(shout(&app, &handle), "QUIETLY");
}

/// The scan lands in the `World` so a game can show what it found and what it could not. The
/// fixture tree carries one deliberate mistake -- `gone.txt.alias` with no `gone.txt` -- and the
/// scan has to report it without costing the assets around it.
#[test]
fn the_scan_is_mirrored_into_the_world_with_its_problems() {
    let mut app = test_app();
    let scan = run_until_scanned(&mut app);

    assert_eq!(
        aliases(&scan),
        [
            "fixture::noisy",
            "fixture::plain",
            "fixture::override_me",
            "fixture::props/barrel",
        ],
        "in walk order: files alphabetically, then subdirectories"
    );

    let problems: Vec<_> = scan.problems.iter().map(|p| (p.kind, &p.path)).collect();
    assert_eq!(
        problems,
        [(
            ProblemKind::OrphanAliasFile,
            &Path::new("files/gone.txt.alias").to_path_buf()
        )],
        "got {:?}",
        scan.problems
    );
}
