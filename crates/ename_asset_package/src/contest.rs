//! What happened when two packages claimed one alias.
//!
//! Every override produces one of these, winner and loser both, because "which mod is actually
//! providing this" is a question with no other answer once more than one package is installed.
//!
//! The reason is the part worth reading. A contest an author asked for -- `after` or `overrides`
//! -- is the system working. A contest nothing relates was settled by a tiebreaker neither author
//! chose, which is the one a user can act on, by writing a `[[constraint]]` of their own.

use crate::{ConstraintSource, Version};
use std::{fmt::Display, path::PathBuf};

/// A package, named the way a report line names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRef {
    pub id: String,
    pub version: Version,
}

impl Display for PackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({})", self.id, self.version)
    }
}

/// What settled a contest nothing ordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tiebreak {
    /// The two packages are in different search paths, so the target's list decided.
    SearchPath { winner: PathBuf, loser: PathBuf },
    /// Same search path, so directory name decided. This is the arbitrary one.
    DirectoryName { search_path: PathBuf },
}

impl Display for Tiebreak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SearchPath { winner, loser } => write!(
                f,
                "won on search path order, {} after {}",
                winner.display(),
                loser.display()
            ),
            Self::DirectoryName { search_path } => write!(
                f,
                "won on directory name, both in {}/",
                search_path.display()
            ),
        }
    }
}

/// Why the winner won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContestReason {
    /// A constraint puts the winner later. `direct` distinguishes "bigships says it loads after
    /// core" from "a chain of other packages does", which changes where a user would go to alter
    /// it.
    Ordered {
        source: ConstraintSource,
        direct: bool,
    },
    /// Nothing relates the two packages.
    Unordered { tiebreak: Tiebreak },
}

/// One alias, claimed by two packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContestedAlias {
    pub alias: String,
    pub winner: PackageRef,
    pub loser: PackageRef,
    pub reason: ContestReason,
}

impl Display for ContestedAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:24} {} over {}", self.alias, self.winner, self.loser)?;
        match &self.reason {
            ContestReason::Ordered {
                source,
                direct: true,
            } => write!(f, " [{source}]"),
            ContestReason::Ordered {
                source,
                direct: false,
            } => write!(f, " [ordered through other packages, from {source}]"),
            ContestReason::Unordered { tiebreak } => write!(f, " UNORDERED -> {tiebreak}"),
        }
    }
}
