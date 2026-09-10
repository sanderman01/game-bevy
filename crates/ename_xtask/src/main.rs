//! `ename_xtask` -- the command line tool for this workspace's content pipeline.
//! Run through the `xtask` cargo alias: `cargo xtask <subcommand>` (see `.cargo/config.toml`).

use ename_xtask::{check, cli, content_build, fix, list, policy};
use std::path::Path;
use std::process::ExitCode;

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
                "wrote {} new .alias files, assigned {} guids",
                summary.created, summary.guids_assigned
            );
            ExitCode::SUCCESS
        }
        other => {
            println!("{other:?} (not wired up yet)");
            ExitCode::SUCCESS
        }
    }
}
