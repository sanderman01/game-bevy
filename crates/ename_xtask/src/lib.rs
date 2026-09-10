//! `ename_xtask` -- the command line tool for this workspace's content pipeline.
//!
//! Depends on `ename_asset_alias` and `ename_asset_package` with `default-features = false`, so it
//! never links Bevy, and reads the same tree the running game does through `StdVfs` instead of an
//! `AssetReader`. See `scratch/content-addressing-design.md`'s Tooling section and
//! `docs/design/crate-layout.md`.
//!
//! Every subcommand is a function that takes its inputs and returns a value, callable from a test
//! with no `env::args()` or process spawn involved. `main.rs` is the only place that reads argv,
//! decides the project's real asset root and search paths, and turns a result into an `ExitCode`.

pub mod check;
pub mod cli;
pub mod content_build;
pub mod fix;
pub mod list;
pub mod mv;
pub mod policy;
mod scan;
