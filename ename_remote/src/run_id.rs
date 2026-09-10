//! A short identifier for this run of the game process.
//!
//! Entity ids do not survive a restart, and neither does anything else the agent holds between
//! tool calls. Without a way to notice that the process changed, a plan made against one run
//! keeps being applied to the next one and the failures look like unrelated bugs. Every tool
//! result carries this value so the change is visible without asking for it.

use bevy::{prelude::*, remote::BrpResult};
use serde::Serialize;
use serde_json::Value;

pub const GET_METHOD: &str = "game.pid.get";

/// Three lowercase letters naming this process. Short because it rides on every response.
#[derive(Resource, Clone)]
pub(crate) struct RunId(String);

/// Owns [`RunId`], which is fixed for the life of the process.
pub(crate) struct RunIdPlugin;

impl Plugin for RunIdPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RunId::for_this_process());
    }
}

impl RunId {
    fn for_this_process() -> Self {
        // The pid alone is a poor source: Linux hands out sequential pids, so consecutive runs
        // would land in nearby buckets, and a recycled pid would repeat outright. Mixing in the
        // start time makes two runs differ whatever the scheduler does with pids.
        let pid = u64::from(std::process::id());
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mut source = pid.to_le_bytes().to_vec();
        source.extend_from_slice(&started.to_le_bytes());
        Self(three_letters(&source))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// FNV-1a, then three base-26 digits.
///
/// 17576 possible values, so two successive runs collide about once in 17576. That is a missed
/// warning, never a false one, which is the right way round: this exists to catch a restart the
/// agent did not expect, and an unnoticed restart still fails loudly on the next entity id.
fn three_letters(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (0..3)
        .map(|i| char::from(b'a' + u8::try_from((hash >> (i * 16)) % 26).unwrap_or(0)))
        .collect()
}

#[derive(Serialize)]
struct PidResponse {
    pid: String,
}

pub(crate) fn get(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    crate::to_value(PidResponse {
        pid: world.resource::<RunId>().as_str().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::three_letters;

    #[test]
    fn is_always_three_lowercase_letters() {
        for seed in 0u64..500 {
            let id = three_letters(&seed.to_le_bytes());
            assert_eq!(id.len(), 3, "{id}");
            assert!(id.bytes().all(|b| b.is_ascii_lowercase()), "{id}");
        }
    }

    #[test]
    fn neighbouring_sources_do_not_share_an_id() {
        // The real source mixes a timestamp in, but sequential pids are the case worth checking:
        // if the hash did not scatter them, every restart would look like the same process.
        let ids: Vec<String> = (1000u64..1010)
            .map(|p| three_letters(&p.to_le_bytes()))
            .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert!(unique.len() >= 9, "sequential sources collided: {ids:?}");
    }
}
