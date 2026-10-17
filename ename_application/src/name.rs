use std::{borrow::Cow, fmt};

/// Longest accepted name, in bytes.
const MAX_LEN: usize = 64;

/// Windows device names, which no file or directory may be called.
const DEVICE_NAMES: [&str; 24] = [
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why a name was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidName {
    #[error("is empty")]
    Empty,
    #[error("is longer than 64 bytes")]
    TooLong,
    #[error("may only contain ASCII letters, digits, '-' and spaces")]
    InvalidCharacter,
    #[error("must start and end with a letter or digit")]
    InvalidEdge,
    #[error("is a Windows device name")]
    DeviceName,
}

/// A name that failed validation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind} name {name:?} {reason}")]
pub struct NameError {
    pub kind: &'static str,
    pub name: String,
    pub reason: InvalidName,
}

/// The rules both name types share. A name is a path component on three filesystems and a label
/// in the macOS identifier `com.<organisation>.<project>`, lowercased with spaces removed:
/// - no `.`, so the identifier splits back unambiguously and `..` cannot occur;
/// - no `_`, which a bundle identifier does not allow;
/// - letter or digit at both ends, since Windows strips trailing spaces.
///
/// Case is kept, so names differing only by case share a folder on Windows and macOS.
const fn validate(name: &str) -> Result<(), InvalidName> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(InvalidName::Empty);
    }
    if bytes.len() > MAX_LEN {
        return Err(InvalidName::TooLong);
    }
    let mut i = 0;
    while i < bytes.len() {
        if !matches!(bytes[i], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b' ') {
            return Err(InvalidName::InvalidCharacter);
        }
        i += 1;
    }
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(InvalidName::InvalidEdge);
    }
    // With no `.` allowed and no trailing space, a device name can only match the whole name.
    let mut d = 0;
    while d < DEVICE_NAMES.len() {
        if bytes.eq_ignore_ascii_case(DEVICE_NAMES[d].as_bytes()) {
            return Err(InvalidName::DeviceName);
        }
        d += 1;
    }
    Ok(())
}

/// Fails const evaluation with a message naming the rule, for `from_static`.
const fn reject(reason: InvalidName) -> ! {
    match reason {
        InvalidName::Empty => panic!("name is empty"),
        InvalidName::TooLong => panic!("name is longer than 64 bytes"),
        InvalidName::InvalidCharacter => {
            panic!("name may only contain ASCII letters, digits, '-' and spaces")
        }
        InvalidName::InvalidEdge => panic!("name must start and end with a letter or digit"),
        InvalidName::DeviceName => panic!("name is a Windows device name"),
    }
}

macro_rules! name_type {
    ($(#[$doc:meta])* $ty:ident, $kind:literal) => {
        $(#[$doc])*
        ///
        /// A storage identifier: changing it makes existing saves look lost. It is separate from
        /// the window title and any display name.
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $ty(Cow<'static, str>);

        impl $ty {
            pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
                let name = name.into();
                match validate(&name) {
                    Ok(()) => Ok(Self(Cow::Owned(name))),
                    Err(reason) => Err(NameError {
                        kind: $kind,
                        name,
                        reason,
                    }),
                }
            }

            /// Validates during const evaluation, so a bad literal in a `const` fails the build.
            pub const fn from_static(name: &'static str) -> Self {
                match validate(name) {
                    Ok(()) => Self(Cow::Borrowed(name)),
                    Err(reason) => reject(reason),
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// The organisation that publishes the project. Used on Windows and macOS.
    OrganisationName,
    "organisation"
);
name_type!(
    /// The project, used on every OS.
    ProjectName,
    "project"
);

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: ProjectName = ProjectName::from_static("example-game");

    #[test]
    fn accepts_valid_names() {
        for name in [
            "a",
            "ename",
            "example-game",
            "game2",
            "2d-game",
            "a-b-c",
            "console",
            "com10",
            "Airship Game",
            "Alexander Verbeek",
            "Game 2",
        ] {
            assert!(ProjectName::new(name).is_ok(), "{name}");
            assert!(OrganisationName::new(name).is_ok(), "{name}");
        }
        assert_eq!(EXAMPLE.as_str(), "example-game");
    }

    #[test]
    fn length_limit_is_64_bytes() {
        assert!(ProjectName::new("a".repeat(64)).is_ok());
        assert_eq!(
            ProjectName::new("a".repeat(65)).unwrap_err().reason,
            InvalidName::TooLong
        );
    }

    #[test]
    fn rejects_invalid_names() {
        let cases = [
            ("", InvalidName::Empty),
            ("my_game", InvalidName::InvalidCharacter),
            ("my\tgame", InvalidName::InvalidCharacter),
            ("my.game", InvalidName::InvalidCharacter),
            ("..", InvalidName::InvalidCharacter),
            ("a/b", InvalidName::InvalidCharacter),
            ("a\\b", InvalidName::InvalidCharacter),
            ("c:", InvalidName::InvalidCharacter),
            ("spiel-ä", InvalidName::InvalidCharacter),
            ("-game", InvalidName::InvalidEdge),
            ("game-", InvalidName::InvalidEdge),
            ("-", InvalidName::InvalidEdge),
            (" game", InvalidName::InvalidEdge),
            ("game ", InvalidName::InvalidEdge),
            ("nul", InvalidName::DeviceName),
            ("NUL", InvalidName::DeviceName),
            ("Con", InvalidName::DeviceName),
            ("com0", InvalidName::DeviceName),
            ("lpt9", InvalidName::DeviceName),
        ];
        for (name, reason) in cases {
            assert_eq!(
                ProjectName::new(name).unwrap_err().reason,
                reason,
                "{name:?}"
            );
            assert_eq!(
                OrganisationName::new(name).unwrap_err().reason,
                reason,
                "{name:?}"
            );
        }
    }

    #[test]
    fn error_names_the_kind() {
        let err = OrganisationName::new("Bad_Name").unwrap_err();
        assert_eq!(
            err.to_string(),
            "organisation name \"Bad_Name\" may only contain ASCII letters, digits, '-' and spaces"
        );
    }
}
