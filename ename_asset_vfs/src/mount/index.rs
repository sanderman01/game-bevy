//! One archive's member index: what is in it, where each member's bytes are, and the directory
//! listings implied by the member paths. Only this stays in memory; member bytes are read on
//! demand.

use super::Limits;
use std::collections::{HashMap, HashSet};

/// The longest member path accepted, in bytes.
const MAX_PATH_LEN: usize = 4096;
/// The longest single path component accepted, in bytes. Most filesystems refuse longer names.
const MAX_COMPONENT_LEN: usize = 255;

/// How a member's bytes are stored.
#[derive(Debug)]
pub(crate) enum Storage {
    /// Raw bytes at `offset`.
    #[cfg(feature = "tar")]
    Tar { offset: u64 },
    #[cfg(feature = "zip")]
    Zip(super::zip_index::ZipMember),
}

#[derive(Debug)]
pub(crate) struct Member {
    pub(crate) size: u64,
    pub(crate) storage: Storage,
}

/// One entry of a directory inside an archive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Child {
    pub(crate) name: Box<str>,
    pub(crate) is_dir: bool,
}

/// Every member of one archive, keyed by its normalized path (`/`-separated, no leading `/`).
#[derive(Debug, Default)]
pub(crate) struct ArchiveIndex {
    members: HashMap<Box<str>, Member>,
    /// Each directory's sorted children. The archive root is `""`.
    dirs: HashMap<Box<str>, Vec<Child>>,
}

impl ArchiveIndex {
    pub(crate) fn member(&self, path: &str) -> Option<&Member> {
        self.members.get(path)
    }

    pub(crate) fn children(&self, path: &str) -> Option<&[Child]> {
        self.dirs.get(path).map(Vec::as_slice)
    }

    pub(crate) fn is_dir(&self, path: &str) -> bool {
        self.dirs.contains_key(path)
    }
}

/// Collects members while an archive is indexed, rejecting anything the mount must not serve.
pub(crate) struct IndexBuilder<'a> {
    limits: &'a Limits,
    members: HashMap<Box<str>, Member>,
    explicit_dirs: HashSet<Box<str>>,
}

impl<'a> IndexBuilder<'a> {
    pub(crate) fn new(limits: &'a Limits) -> Self {
        Self {
            limits,
            members: HashMap::new(),
            explicit_dirs: HashSet::new(),
        }
    }

    /// Checked before anything is allocated per member.
    pub(crate) fn check_count(&self, count: usize) -> Result<(), String> {
        if count > self.limits.max_members {
            return Err(format!(
                "{count} members, more than the limit of {}",
                self.limits.max_members
            ));
        }
        Ok(())
    }

    pub(crate) fn check_size(&self, name: &str, size: u64) -> Result<(), String> {
        if size > self.limits.max_member_size {
            return Err(format!(
                "member {name:?} is {size} bytes, more than the limit of {}",
                self.limits.max_member_size
            ));
        }
        Ok(())
    }

    pub(crate) fn add_dir(&mut self, raw: &str) -> Result<(), String> {
        if let Some(path) = normalize(raw)? {
            self.explicit_dirs.insert(path.into());
        }
        Ok(())
    }

    pub(crate) fn add_file(&mut self, raw: &str, member: Member) -> Result<(), String> {
        self.check_count(self.members.len() + 1)?;
        self.check_size(raw, member.size)?;
        let Some(path) = normalize(raw)? else {
            return Ok(());
        };
        if self.members.insert(path.into(), member).is_some() {
            return Err(format!("member {raw:?} appears twice"));
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<ArchiveIndex, String> {
        let mut dirs: HashMap<Box<str>, HashSet<Child>> = HashMap::new();
        dirs.entry("".into()).or_default();
        let mut add = |path: &str, is_dir: bool| {
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            dirs.entry(parent.into()).or_default().insert(Child {
                name: name.into(),
                is_dir,
            });
        };
        for path in self.members.keys() {
            add(path, false);
            for parent in parents(path) {
                add(parent, true);
            }
        }
        for path in &self.explicit_dirs {
            add(path, true);
            for parent in parents(path) {
                add(parent, true);
            }
        }
        // Every directory with a child is a key by now; an explicit empty one may not be.
        for path in &self.explicit_dirs {
            dirs.entry(path.clone()).or_default();
        }
        let mut dirs: HashMap<Box<str>, Vec<Child>> = dirs
            .into_iter()
            .map(|(path, children)| {
                let mut children: Vec<Child> = children.into_iter().collect();
                children.sort();
                (path, children)
            })
            .collect();
        for path in dirs.keys() {
            if self.members.contains_key(path) {
                return Err(format!("{path:?} is both a file and a directory"));
            }
        }
        dirs.shrink_to_fit();
        Ok(ArchiveIndex {
            members: self.members,
            dirs,
        })
    }
}

/// Every proper ancestor of `path`, nearest first, excluding the root.
fn parents(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices()
        .rev()
        .filter(|(_, c)| *c == '/')
        .map(|(i, _)| &path[..i])
}

/// A member path in the form the index keys on, or `None` for an entry the mount skips (the root
/// itself, and `__MACOSX/`). An error names why the whole archive is refused.
///
/// `.` components and repeated slashes are dropped, because `tar -C dir .` writes `./a.txt`.
fn normalize(raw: &str) -> Result<Option<String>, String> {
    let invalid = |why: &str| Err(format!("member {raw:?} {why}"));
    if raw.len() > MAX_PATH_LEN {
        return invalid("has a path that is too long");
    }
    if raw.contains('\\') || raw.contains('\0') {
        return invalid("contains a backslash or a NUL");
    }
    if raw.starts_with('/') {
        return invalid("is an absolute path");
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => return invalid("climbs out with `..`"),
            _ if part.contains(':') => return invalid("contains `:` (a drive prefix?)"),
            _ if part.len() > MAX_COMPONENT_LEN => return invalid("has a name that is too long"),
            _ => parts.push(part),
        }
    }
    if parts.first() == Some(&"__MACOSX") || parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_accepts_relative_paths_and_drops_dots() {
        assert_eq!(normalize("a/b.txt").unwrap().as_deref(), Some("a/b.txt"));
        assert_eq!(normalize("./a//b/").unwrap().as_deref(), Some("a/b"));
        assert_eq!(normalize("./").unwrap(), None);
        assert_eq!(normalize("__MACOSX/._a").unwrap(), None);
    }

    #[test]
    fn normalize_refuses_paths_that_escape_or_are_not_portable() {
        for raw in ["/etc/passwd", "a/../../b", "C:/x", "a\\b", "a/c:d"] {
            assert!(normalize(raw).is_err(), "{raw}");
        }
        assert!(normalize(&"a".repeat(256)).is_err());
    }
}
