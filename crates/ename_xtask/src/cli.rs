//! Argument parsing for the `ename_xtask` binary.
//!
//! No argument-parsing crate: five flat subcommands and at most two positional arguments do not
//! need one, and a hand-rolled parser takes a plain iterator of `String`, which a test can build
//! without touching `std::env`.

use std::fmt::Display;
use std::path::PathBuf;

/// One invocation of `cargo xtask <name> ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ename_check` -- non-zero exit if the content tree has a problem or a contested alias.
    Check,
    /// `ename_fix` -- writes missing `.alias` files and assigns missing guids.
    Fix,
    /// `ename_content_build` -- stub. Will bake `content-index.ron` for a shipping build.
    ContentBuild,
    /// `ename_list` -- dumps the resolved alias index.
    List,
    /// `ename_mv <from> <to>` -- moves an asset and its `.meta`/`.alias` sidecars.
    Mv { from: PathBuf, to: PathBuf },
}

/// Why the command line could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    NoCommand,
    Unknown(String),
    MvNeedsTwoPaths,
}

impl Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCommand => write!(
                f,
                "usage: cargo xtask <ename_check|ename_fix|ename_content_build|ename_list|ename_mv <from> <to>>"
            ),
            Self::Unknown(name) => write!(f, "unknown subcommand `{name}`"),
            Self::MvNeedsTwoPaths => write!(f, "ename_mv needs a <from> and a <to> path"),
        }
    }
}

/// Parses argv, without the program name, into a [`Command`].
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command, CliError> {
    let name = args.next().ok_or(CliError::NoCommand)?;
    match name.as_str() {
        "ename_check" => Ok(Command::Check),
        "ename_fix" => Ok(Command::Fix),
        "ename_content_build" => Ok(Command::ContentBuild),
        "ename_list" => Ok(Command::List),
        "ename_mv" => {
            let from = args.next().ok_or(CliError::MvNeedsTwoPaths)?;
            let to = args.next().ok_or(CliError::MvNeedsTwoPaths)?;
            Ok(Command::Mv {
                from: from.into(),
                to: to.into(),
            })
        }
        other => Err(CliError::Unknown(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::{CliError, Command, parse};

    fn args(words: &[&str]) -> impl Iterator<Item = String> {
        words
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn each_flat_subcommand_parses() {
        assert_eq!(parse(args(&["ename_check"])), Ok(Command::Check));
        assert_eq!(parse(args(&["ename_fix"])), Ok(Command::Fix));
        assert_eq!(
            parse(args(&["ename_content_build"])),
            Ok(Command::ContentBuild)
        );
        assert_eq!(parse(args(&["ename_list"])), Ok(Command::List));
    }

    #[test]
    fn mv_takes_two_positional_paths() {
        assert_eq!(
            parse(args(&["ename_mv", "a/b.png", "a/c.png"])),
            Ok(Command::Mv {
                from: "a/b.png".into(),
                to: "a/c.png".into()
            })
        );
    }

    #[test]
    fn mv_without_both_paths_is_an_error() {
        assert_eq!(
            parse(args(&["ename_mv", "a/b.png"])),
            Err(CliError::MvNeedsTwoPaths)
        );
        assert_eq!(parse(args(&["ename_mv"])), Err(CliError::MvNeedsTwoPaths));
    }

    #[test]
    fn no_subcommand_is_an_error() {
        assert_eq!(parse(args(&[])), Err(CliError::NoCommand));
    }

    #[test]
    fn an_unrecognised_name_is_an_error() {
        assert_eq!(
            parse(args(&["content"])),
            Err(CliError::Unknown("content".into()))
        );
    }
}
