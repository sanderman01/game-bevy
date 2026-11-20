//! Wall-clock time on every target, the browser included, where `std::time::SystemTime::now`
//! panics.

use web_time::{SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch, UTC; 0 for a clock set before it.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_after_2020() {
        assert!(now_unix() > 1_577_836_800);
    }
}
