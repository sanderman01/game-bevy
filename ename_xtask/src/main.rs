//! `ename_xtask` -- the command line tool for this workspace's content pipeline and helpers.
//! Run through the `xtask` cargo alias: `cargo xtask <subcommand>` (see `.cargo/config.toml`).

use ename_xtask::{check, cli, content_build, dep_src, fix, list, mv, policy};
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };

    match command {
        cli::Command::Check => {
            let asset_root = Path::new(policy::ASSET_ROOT);
            let search_paths = policy::search_paths();
            let load_order_path = policy::load_order_path();
            if check::check(asset_root, &search_paths, load_order_path.as_deref()) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        cli::Command::List => {
            let asset_root = Path::new(policy::ASSET_ROOT);
            let search_paths = policy::search_paths();
            let load_order_path = policy::load_order_path();
            list::list(asset_root, &search_paths, load_order_path.as_deref());
            ExitCode::SUCCESS
        }
        cli::Command::ContentBuild => {
            content_build::content_build();
            ExitCode::SUCCESS
        }
        cli::Command::Fix => {
            let asset_root = Path::new(policy::ASSET_ROOT);
            let search_paths = policy::search_paths();
            let load_order_path = policy::load_order_path();
            let summary = fix::fix(asset_root, &search_paths, load_order_path.as_deref());
            println!(
                "wrote {} new .alias files, assigned {} guids, {} failed, {} skipped inside archives",
                summary.created, summary.guids_assigned, summary.failed, summary.archived
            );
            if summary.failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        cli::Command::Mv { from, to } => match mv::mv(Path::new(policy::ASSET_ROOT), &from, &to) {
            Ok(moved) => {
                for m in &moved {
                    println!(
                        "{} {} -> {}",
                        if m.via_git { "git mv" } else { "mv" },
                        m.from.display(),
                        m.to.display()
                    );
                }
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("ename_mv: {err}");
                ExitCode::FAILURE
            }
        },
        cli::Command::DepSrc { name } => dep_src(&name),
    }
}

/// Runs `cargo metadata` in the current directory, so it answers for the caller's workspace.
fn dep_src(name: &str) -> ExitCode {
    // `cargo run` sets `CARGO` to the cargo that launched us.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let metadata = |all_features: bool| {
        let mut command = Command::new(&cargo);
        command.args(["metadata", "--format-version", "1", "--locked", "--offline"]);
        if all_features {
            command.arg("--all-features");
        }
        command.output()
    };
    // `--all-features` reaches lock entries only optional features pull in. Offline it fails if
    // such a crate was never downloaded, so fall back to the default features.
    let mut complete = true;
    let output = match metadata(true) {
        Ok(output) if output.status.success() => Ok(output),
        _ => {
            eprintln!(
                "ename_dep_src: --all-features metadata failed; listing default features only"
            );
            complete = false;
            metadata(false)
        }
    };
    let output = match output {
        Ok(output) => output,
        Err(err) => {
            eprintln!("ename_dep_src: could not run cargo metadata: {err}");
            return ExitCode::FAILURE;
        }
    };
    if !output.status.success() {
        let _ = std::io::stderr().write_all(&output.stderr);
        return ExitCode::FAILURE;
    }
    let metadata = match dep_src::Metadata::from_json(&output.stdout) {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!("ename_dep_src: could not read cargo metadata output: {err}");
            return ExitCode::FAILURE;
        }
    };

    let found = dep_src::find(&metadata, name);
    if found.is_empty() {
        if complete {
            eprintln!("{name} is not in this workspace's Cargo.lock");
        } else {
            eprintln!(
                "{name} is not in this workspace's default-feature graph; the list may be incomplete"
            );
        }
        for near in dep_src::near_misses(&metadata, name, 10) {
            eprintln!("  {near}");
        }
        return ExitCode::FAILURE;
    }
    for source in &found {
        println!("{source}");
    }
    let mut versions: Vec<&str> = found.iter().map(|s| s.version.as_str()).collect();
    versions.dedup();
    if let [_, .., last] = versions.as_slice() {
        eprintln!(
            "{} versions; cargo tree --target all --all-features -i {}@{} shows who pulls each in",
            versions.len(),
            found[0].name,
            last
        );
    }
    ExitCode::SUCCESS
}
