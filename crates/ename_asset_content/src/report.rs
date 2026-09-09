//! What a scan found, in a shape the editor and the log can read.
//!
//! This is for inspection. It is not the addressing path: nothing resolves an alias through the
//! report, and dropping it would cost visibility and nothing else.

use bevy::ecs::resource::Resource;
use ename_asset_package::{Problem, Version};
use std::path::PathBuf;

/// One package that made it into the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub id: String,
    pub version: Version,
    pub root: PathBuf,
    /// Aliases this package actually registered, which is its discovered assets minus whatever
    /// was rejected.
    pub aliases: usize,
}

/// Everything one content scan found, problems included.
///
/// The problem list is the scan's own, plus the aliases the alias layer rejected. One list rather
/// than two, because "what is wrong with my content" is one question.
#[derive(Debug, Default, Clone, Resource)]
pub struct ContentReport {
    /// In load order.
    pub packages: Vec<PackageSummary>,
    pub problems: Vec<Problem>,
}
