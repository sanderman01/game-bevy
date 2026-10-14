//! Which run of the game process this is.

use std::hash::{BuildHasher, Hasher};

use bevy::prelude::*;

/// Names this run of the game process: eight lowercase letters.
///
/// Entity ids, job ids and anything else a client holds between calls are only meaningful within
/// one run. Every command and job answer carries this, and job ids start with it, so a client can
/// tell that the game restarted under it. 26^8 is about 2e11 values, so two runs share one
/// essentially never.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct RunId(String);

impl RunId {
    /// The pid alone repeats and runs sequentially, so it is mixed with the start time and with
    /// the OS-seeded key `RandomState` draws for each process.
    pub(crate) fn for_this_process() -> Self {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos()),
        );
        Self::from_hash(hasher.finish())
    }

    fn from_hash(mut hash: u64) -> Self {
        let letters = (0..8)
            .map(|_| {
                let letter = b'a' + (hash % 26) as u8;
                hash /= 26;
                char::from(letter)
            })
            .collect();
        Self(letters)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::RunId;

    #[test]
    fn is_eight_lowercase_letters() {
        for hash in [0, 1, u64::MAX, 0xdead_beef] {
            let id = RunId::from_hash(hash);
            assert_eq!(id.as_str().len(), 8, "{id}");
            assert!(id.as_str().bytes().all(|b| b.is_ascii_lowercase()), "{id}");
        }
    }

    #[test]
    fn two_processes_worth_of_ids_differ() {
        assert_ne!(RunId::for_this_process(), RunId::for_this_process());
    }
}
