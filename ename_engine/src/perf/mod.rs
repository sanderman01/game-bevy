//! Performance sampling: frame, CPU and GPU times per camera, summarized against run-to-run noise.

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by perf.sample and perf.compare")
)]
mod stats;

#[cfg(test)]
mod tests;
