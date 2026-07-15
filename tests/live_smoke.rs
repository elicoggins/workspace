//! Live smoke test — exercises the REAL macOS executor, which the ordinary
//! suite (built on `SimulatedExecutor`) structurally cannot cover.
//!
//! Run manually before releases, on a machine with Accessibility granted:
//!
//! ```sh
//! cargo test --test live_smoke -- --ignored
//! ```
//!
//! It briefly moves one of your windows by 40 px and puts it back.

#[test]
#[ignore = "drives the real macOS window server; run manually with -- --ignored"]
fn live_selftest_passes() {
    let report = workspace::selftest::run(true).expect("selftest should run to completion");
    let failed: Vec<_> = report.checks.iter().filter(|check| !check.passed).collect();
    assert!(failed.is_empty(), "failed checks: {failed:#?}");
}
