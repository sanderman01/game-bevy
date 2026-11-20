//! Record keys and blob names, and the grammar every backend can store.

use std::{borrow::Cow, cmp::Ordering, fmt};

/// Longest segment of a key, in bytes.
pub const MAX_SEGMENT_LEN: usize = 64;
/// Longest key, in bytes, separators included.
pub const MAX_KEY_LEN: usize = 128;
/// Most segments in a key.
pub const MAX_KEY_DEPTH: usize = 8;
/// Longest blob name, in bytes.
pub const MAX_BLOB_NAME_LEN: usize = 32;

/// Windows device names, which no file or directory may be called, with or without an extension.
const DEVICE_NAMES: [&str; 24] = [
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why a key, key prefix or blob name was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidName {
    #[error("is empty")]
    Empty,
    #[error("is longer than its limit")]
    TooLong,
    #[error("has more than 8 segments")]
    TooDeep,
    #[error("has an empty segment")]
    EmptySegment,
    #[error("has a segment longer than 64 bytes")]
    SegmentTooLong,
    #[error("may only contain 'a'-'z', '0'-'9', '-' and '_', and '/' between segments")]
    InvalidCharacter,
    #[error("has a segment that does not start with a letter or digit")]
    InvalidStart,
    #[error("has a segment that is a Windows device name")]
    DeviceName,
}

/// A key, key prefix or blob name that failed validation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("storage {what} {value:?} {reason}")]
pub struct NameError {
    pub what: &'static str,
    pub value: String,
    pub reason: InvalidName,
}

/// One segment: lowercase ASCII letters, digits, `-` and `_`, starting with a letter or digit, at
/// most `max` bytes, not a Windows device name.
///
/// Lowercase only, so two keys never differ only by case and collide on a case-insensitive
/// filesystem. No `.`, so `.` and `..` cannot occur, Windows never strips a trailing dot, and a
/// backend can use dotted names for its own files.
const fn check_segment(bytes: &[u8], max: usize) -> Result<(), InvalidName> {
    if bytes.is_empty() {
        return Err(InvalidName::EmptySegment);
    }
    if bytes.len() > max {
        return Err(InvalidName::SegmentTooLong);
    }
    let mut i = 0;
    while i < bytes.len() {
        if !matches!(bytes[i], b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_') {
            return Err(InvalidName::InvalidCharacter);
        }
        i += 1;
    }
    if !matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9') {
        return Err(InvalidName::InvalidStart);
    }
    let mut d = 0;
    while d < DEVICE_NAMES.len() {
        if is_same(bytes, DEVICE_NAMES[d].as_bytes()) {
            return Err(InvalidName::DeviceName);
        }
        d += 1;
    }
    Ok(())
}

const fn is_same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Checks segments separated by `/`. An empty string has no segments.
fn check_path(value: &str) -> Result<usize, InvalidName> {
    if value.is_empty() {
        return Ok(0);
    }
    if value.len() > MAX_KEY_LEN {
        return Err(InvalidName::TooLong);
    }
    let mut depth = 0;
    for segment in value.split('/') {
        check_segment(segment.as_bytes(), MAX_SEGMENT_LEN)?;
        depth += 1;
    }
    if depth > MAX_KEY_DEPTH {
        return Err(InvalidName::TooDeep);
    }
    Ok(depth)
}

/// The logical address of a record, such as `saves/<session>/<save>` or `difficulty/<name>`.
///
/// One to eight segments separated by `/`, at most 128 bytes. Each segment is 1 to 64 bytes of
/// `a`-`z`, `0`-`9`, `-` and `_`, starts with a letter or digit, and is not a Windows device name
/// (`con`, `nul`, `com1`, ...). A UUID in its usual lowercase form is a valid segment. A key is not
/// a path: only the filesystem backend maps it to one. A user-chosen display name belongs in a
/// blob, not in the key.
///
/// Keys order by segments, so `a/b` sorts before `a-b`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

impl Key {
    pub fn new(key: impl Into<String>) -> Result<Self, NameError> {
        let key = key.into();
        match check_path(&key) {
            Ok(0) => Err(error("key", key, InvalidName::Empty)),
            Ok(_) => Ok(Self(key)),
            Err(reason) => Err(error("key", key, reason)),
        }
    }

    /// Joins the segments with `/` and validates the result.
    pub fn from_segments<I, S>(segments: I) -> Result<Self, NameError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::new(join(segments))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// This key with one more segment.
    pub fn child(&self, segment: &str) -> Result<Self, NameError> {
        Self::new(format!("{}/{segment}", self.0))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        self.segments().cmp(other.segments())
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({:?})", self.0)
    }
}

/// Leading whole segments of a key, for listing: `saves` matches `saves/a/1` and `saves` itself,
/// never `saves-old/1`. The empty prefix matches every key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyPrefix(String);

impl KeyPrefix {
    /// Same grammar as [`Key`], except that it may be empty.
    pub fn new(prefix: impl Into<String>) -> Result<Self, NameError> {
        let prefix = prefix.into();
        match check_path(&prefix) {
            Ok(_) => Ok(Self(prefix)),
            Err(reason) => Err(error("key prefix", prefix, reason)),
        }
    }

    pub fn from_segments<I, S>(segments: I) -> Result<Self, NameError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::new(join(segments))
    }

    /// Every key.
    pub fn all() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, key: &Key) -> bool {
        self.0.is_empty()
            || key
                .0
                .strip_prefix(&self.0)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl From<Key> for KeyPrefix {
    fn from(key: Key) -> Self {
        Self(key.0)
    }
}

impl fmt::Display for KeyPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name of one blob of a record, such as `meta`, `thumbnail` or `state`: one key segment of
/// at most 32 bytes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobName(Cow<'static, str>);

impl BlobName {
    pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
        let name = name.into();
        match check_blob(&name) {
            Ok(()) => Ok(Self(Cow::Owned(name))),
            Err(reason) => Err(error("blob name", name, reason)),
        }
    }

    /// Validates during const evaluation, so a bad literal in a `const` fails the build.
    pub const fn from_static(name: &'static str) -> Self {
        match check_blob(name) {
            Ok(()) => Self(Cow::Borrowed(name)),
            Err(_) => panic!("invalid blob name: see `BlobName` for the grammar"),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const fn check_blob(name: &str) -> Result<(), InvalidName> {
    if name.is_empty() {
        return Err(InvalidName::Empty);
    }
    if name.len() > MAX_BLOB_NAME_LEN {
        return Err(InvalidName::TooLong);
    }
    check_segment(name.as_bytes(), MAX_BLOB_NAME_LEN)
}

impl fmt::Display for BlobName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for BlobName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobName({:?})", self.0)
    }
}

fn join<I, S>(segments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut joined = String::new();
    for segment in segments {
        if !joined.is_empty() {
            joined.push('/');
        }
        joined.push_str(segment.as_ref());
    }
    joined
}

fn error(what: &'static str, value: String, reason: InvalidName) -> NameError {
    NameError {
        what,
        value,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: BlobName = BlobName::from_static("meta");

    #[test]
    fn accepts_valid_keys() {
        for key in [
            "a",
            "saves/3f2b8c1e-9d4a-4f7b-8a2e-1c5d6e7f8a9b/0b1c2d3e-4f5a-6b7c-8d9e-0f1a2b3c4d5e",
            "difficulty/hard_mode",
            "a/b/c/d/e/f/g/h",
            "com10",
            "console",
            "2d",
        ] {
            assert!(Key::new(key).is_ok(), "{key}");
        }
        assert_eq!(META.as_str(), "meta");
    }

    #[test]
    fn rejects_invalid_keys() {
        let segment = "a".repeat(65);
        let cases = [
            ("", InvalidName::Empty),
            ("/", InvalidName::EmptySegment),
            ("a/", InvalidName::EmptySegment),
            ("/a", InvalidName::EmptySegment),
            ("a//b", InvalidName::EmptySegment),
            ("a/b/c/d/e/f/g/h/i", InvalidName::TooDeep),
            (segment.as_str(), InvalidName::SegmentTooLong),
            ("Saves", InvalidName::InvalidCharacter),
            ("a.b", InvalidName::InvalidCharacter),
            ("..", InvalidName::InvalidCharacter),
            (".", InvalidName::InvalidCharacter),
            ("a\\b", InvalidName::InvalidCharacter),
            ("a b", InvalidName::InvalidCharacter),
            ("c:", InvalidName::InvalidCharacter),
            ("ä", InvalidName::InvalidCharacter),
            ("-a", InvalidName::InvalidStart),
            ("_a", InvalidName::InvalidStart),
            ("nul", InvalidName::DeviceName),
            ("saves/com1", InvalidName::DeviceName),
            ("lpt9/a", InvalidName::DeviceName),
        ];
        for (key, reason) in cases {
            assert_eq!(Key::new(key).unwrap_err().reason, reason, "{key:?}");
        }
    }

    #[test]
    fn key_length_counts_separators() {
        let long = ["a".repeat(63), "b".repeat(63)].join("/");
        assert_eq!(long.len(), 127);
        assert!(Key::new(&long).is_ok());
        let too_long = format!("{long}/c");
        assert_eq!(Key::new(too_long).unwrap_err().reason, InvalidName::TooLong);
    }

    #[test]
    fn blob_names_are_one_short_segment() {
        assert!(BlobName::new("thumbnail").is_ok());
        assert!(BlobName::new("a".repeat(32)).is_ok());
        assert_eq!(
            BlobName::new("a".repeat(33)).unwrap_err().reason,
            InvalidName::TooLong
        );
        assert_eq!(
            BlobName::new("a/b").unwrap_err().reason,
            InvalidName::InvalidCharacter
        );
        assert_eq!(BlobName::new("").unwrap_err().reason, InvalidName::Empty);
        assert_eq!(
            BlobName::new("aux").unwrap_err().reason,
            InvalidName::DeviceName
        );
    }

    #[test]
    fn prefix_matches_whole_segments() {
        let key = Key::new("saves/a/1").unwrap();
        for prefix in ["", "saves", "saves/a", "saves/a/1"] {
            assert!(KeyPrefix::new(prefix).unwrap().matches(&key), "{prefix}");
        }
        for prefix in ["save", "saves/a/1/x", "saves/b", "saves-old"] {
            assert!(!KeyPrefix::new(prefix).unwrap().matches(&key), "{prefix}");
        }
        assert!(KeyPrefix::new("saves/").is_err());
    }

    #[test]
    fn keys_order_by_segments() {
        let mut keys: Vec<Key> = ["a-b/x", "a/x", "a/x/y", "a"]
            .into_iter()
            .map(|k| Key::new(k).unwrap())
            .collect();
        keys.sort();
        let sorted: Vec<&str> = keys.iter().map(Key::as_str).collect();
        assert_eq!(sorted, ["a", "a/x", "a/x/y", "a-b/x"]);
    }

    #[test]
    fn builds_from_segments() {
        let key = Key::from_segments(["saves", "s1", "a"]).unwrap();
        assert_eq!(key.as_str(), "saves/s1/a");
        assert_eq!(key.child("b").unwrap().as_str(), "saves/s1/a/b");
        assert!(Key::from_segments(["saves", "S1"]).is_err());
        assert_eq!(
            KeyPrefix::from_segments(Vec::<&str>::new()).unwrap(),
            KeyPrefix::all()
        );
    }

    #[test]
    fn error_names_what_was_rejected() {
        let err = BlobName::new("Meta").unwrap_err();
        assert!(
            err.to_string().starts_with("storage blob name \"Meta\""),
            "{err}"
        );
    }
}
