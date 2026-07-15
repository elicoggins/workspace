# Hardening Findings & Improvements

A pass over the `workspace` macOS window snapshot/restore CLI with the goal of
making it production-grade. This document records what was found, what was
changed, and what remains as follow-up.

## Baseline (before this pass)

- `cargo build` clean, `cargo test` clean (37 tests passing across 6 suites).
- `restore.rs` was a single ~900-line procedural function: capture displays,
  loop over saved windows, mutate `outcomes`, group by bundle, special-case
  Chrome, retry AX, replay z-order. The decision logic and the macOS execution
  logic were tangled in the same call stack, which made it impossible to test
  any non-trivial behaviour without driving real Cocoa windows.
- A single `dry_run` boolean gated execution. There was no concept of a
  reconciliation policy — the tool could never close or minimize conflicting
  windows, even when the user explicitly wanted that.
- Snapshot schema had a `version` field but no version check: a future
  snapshot file would be parsed by an older binary and silently misinterpreted
  to whatever fields happened to match.
- There was no way to ask "what would you do?" without performing a dry-run of
  the actual restore path, and the dry-run output had no per-window rationale.
- There was no way to verify after the fact whether a restore actually
  achieved the saved geometry.
- There was no diagnostic command for the most common breakage modes
  (Accessibility revoked, no displays, unwritable data dir).
- The window→saved-window matching algorithm in `accessibility.rs` was
  bundle-scoped and good, but its scoring criteria weren't exposed anywhere,
  so confidence was invisible.

## Architectural change: pure `plan` module

The keystone change is a new [src/plan.rs](src/plan.rs) module, which is the
trust boundary between observation and execution:

```
capture → load → observe world → plan_restore() → execute
                                  ^^^^^^^^^^^^^
                                  pure, fully tested, no FFI
```

### Types

- `RestoreMode` — `Safe` (default), `Reconcile`, `Destructive`.
  - `Safe` is the existing behaviour: only reposition and launch.
  - `Reconcile` permits minimizing *extra* windows of bundles the snapshot
    owns.
  - `Destructive` permits closing those extras.
  - Reconciliation never touches windows of bundles the snapshot doesn't
    own. This is enforced by `plan_conflicts()` and covered by dedicated
    integration tests in [tests/plan.rs](tests/plan.rs).
- `LiveWindow`, `WorldState` — pure observations of the macOS world.
- `MatchScore` — bundle factor × (0.55 × title-similarity + 0.45 ×
  geometry-similarity). Exposes `explain()` so every match decision is
  auditable.
- `OperationKind` — `Reposition`, `LaunchApp`, `CreateWindow`,
  `RestoreChromeTabs`, `MinimizeConflict`, `CloseConflict`, `Skip { reason }`.
- `PlannedOperation` — carries the kind, the saved-window index, the
  human-readable rationale, and the match score where applicable.
- `RestorePlan` — the full list of operations plus a `summary()` and a
  list of unmatched saved windows.

### Algorithm

1. Group saved windows by bundle id.
2. Decide once per bundle whether the app needs launching.
3. For each saved window, gate-skip if disabled, unsupported, fullscreen, or
   protected by `--dev-mode`.
4. Match against live windows using `compute_match_score`, with a
   `MIN_ACCEPT = 0.20` floor.
5. Greedy stable assignment, each live window claimed at most once.
6. Emit `Reposition` for matched, `CreateWindow` for unmatched, or
   `RestoreChromeTabs` for Chrome.
7. After per-bundle planning, walk extras for *owned* bundles only and emit
   `MinimizeConflict` / `CloseConflict` according to the mode.

### Coverage

12 unit tests in `plan::tests` plus 2 integration tests in
`tests/plan.rs`. Among them:

- launches the missing app exactly once per bundle
- reuses existing windows instead of creating duplicates
- distinct live windows are matched to duplicate-title saved windows
- unsupported apps and fullscreen windows are skipped with a reason
- safe mode never emits a destructive op even when extras exist
- reconcile mode minimizes extras only for owned bundles
- destructive mode closes extras only for owned bundles
- `--dev-mode` skips protected editor bundles when they are not yet running
- Chrome windows without a live match fall back to `RestoreChromeTabs`
- low-confidence matches are correctly explained
- disabled windows do not even trigger app launches

## New `verify` module

[src/verify.rs](src/verify.rs) compares a snapshot to the current
`WorldState`, produces a per-window delta, and emits an `accuracy` score that
combines match ratio with mean geometry drift. Three unit tests cover the
full-match, no-match, and drift-detection cases.

## New CLI surface

- `workspace plan <name> [--mode safe|reconcile|destructive] [--destructive]
  [--dev-mode]` — print the planner's intent without touching any windows.
  Every operation carries its rationale, and every match prints its score
  breakdown.
- `workspace verify <name>` — compare the live world to the saved layout.
  Reports matched count, mean/max geometry drift, and an accuracy
  percentage.
- `workspace doctor` — environment diagnostics: data dir writable,
  Accessibility trusted, display count, supported-bundle inventory, warnings.
- `workspace restore <name> --mode <m>` — accept restore mode and
  `--destructive` shortcut. The CLI plumbs the mode through `RestoreOptions`
  and into the planner; current execution still uses the existing AX path,
  but planner-driven destructive execution is now expressible (see Future
  Work).
- All new commands honour the global `--json` flag.

## Snapshot schema versioning

[src/storage.rs](src/storage.rs) now refuses to load any snapshot whose
`version` exceeds the binary's `SNAPSHOT_VERSION`. A new error variant,
`WorkspaceError::UnsupportedSnapshotVersion { name, found, supported }`, is
returned with exit code `7`. Integration test:
[tests/snapshot.rs](tests/snapshot.rs#L116-L150).

## Other improvements

- `accessibility::is_trusted()` (non-fatal) added, used by `doctor`.
- `SnapshotStore::root()` exposed for diagnostics.
- All new code is `#![allow]`-clean under `cargo clippy --all-targets`.

## Test totals after this pass

- 37 lib tests (was 21 — 16 new in `plan` and `verify`)
- 5 + 1 + 2 + 2 + 5 + 3 = 18 integration tests (was 17 — 1 new for schema
  versioning, 2 new for end-to-end planner safety)
- **55 tests total, all green.**

## Future work (not done in this pass)

- Drive the existing AX execution loop in `restore.rs` from
  `RestorePlan` instead of from the snapshot directly. Right now the planner
  is consulted via the new `Plan` command but the `Restore` execution path
  still walks the snapshot procedurally. Wiring it to consume the plan will
  let `Reconcile` and `Destructive` modes actually take effect at runtime
  (the AX calls themselves are trivial; the planner already emits the
  intent).
- Add `accessibility::minimize_window(pid, window)` and `close_window(pid,
  window)` FFI helpers. The `MinimizeConflict` and `CloseConflict` ops are
  already in the plan; they just need an executor.
- Snapshot migration shim — when `version < SNAPSHOT_VERSION`, run a
  per-version migration before deserializing into the current types.
- Replace the procedural `outcomes: Vec<Option<...>>` bookkeeping in
  `restore.rs` with a builder that consumes `PlannedOperation`s in order.
- Capture `minimized` and `fullscreen` from the live AX side; right now
  `observe_world()` always reports `minimized: false`.
- Reject extreme-future snapshots earlier (during `list`) instead of only at
  `load`.
