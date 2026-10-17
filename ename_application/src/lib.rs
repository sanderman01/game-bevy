//! `ename_application` -- where a game keeps its per-user files.
//!
//! Resolves the persistent data and cache directories for an organisation and project on Linux,
//! Windows and macOS, following each OS's convention. No Bevy and no first-party dependencies, so
//! `ename_xtask` computes the same paths as the running game. `ename_engine::application`
//! re-exports it and adds the resource. See `docs/design/crate-layout.md` and the "Per-user
//! directories" section of `docs/conventions.md`.

mod name;
mod paths;

pub use name::{InvalidName, NameError, OrganisationName, ProjectName};
pub use paths::Paths;
