//! `ename_xtask` -- the command line tool for this workspace's content pipeline.
//! Run through the `xtask` cargo alias: `cargo xtask <subcommand>` (see `.cargo/config.toml`).

use ename_xtask::cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        Ok(command) => {
            // Each subcommand below replaces one arm of this match, one task at a time.
            println!("{command:?} (not wired up yet)");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
