//! Runs the live selftest against the desktop with Accessibility permission.
//! This moves a window and restores the snapshot.
//!
//! ```sh
//! cargo test --test live_smoke -- --ignored
//! ```

#[test]
#[ignore = "drives the real macOS window server; run manually with -- --ignored"]
fn live_selftest_passes() {
    let report = workspace::selftest::run(true).expect("selftest should run to completion");
    let failed: Vec<_> = report.checks.iter().filter(|check| !check.passed).collect();
    assert!(failed.is_empty(), "failed checks: {failed:#?}");
}
