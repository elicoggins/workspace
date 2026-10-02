# workspace

Save named window layouts on macOS and restore them from the terminal. Snapshots
include window positions, displays, and tabs from Chromium browsers.

## Install

From a checkout, with Rust installed:

```sh
cargo install --path .
```

Grant **Accessibility** access in System Settings > Privacy & Security to move
windows. **Screen Recording** lets macOS expose window titles, which helps with
matching and browser tab capture. Run `workspace doctor` to check permissions
and title visibility.

## Use

```sh
workspace save coding
workspace plan coding
workspace restore coding
```

`save` refuses to overwrite an existing snapshot unless you pass `--force`.
Snapshots are JSON files in `~/Library/Application Support/workspace/`.

To check a layout before changing anything:

```sh
workspace restore coding --dry-run
workspace diff coding
```

`restore` prints a journal of attempted operations and a geometry check.
`--converge 3` allows up to three restore passes, observing the windows again
between passes. `--json` writes command output as JSON; diagnostics go to stderr.
Restore stops early when every restorable window is visible and within two points
of its target frame, and the planned actions report success.

Use `list`, `inspect`, and `delete` to manage snapshots. `configure coding` selects
which saved windows to restore. `workspace --help` lists the commands, and
`workspace restore --help` describes the restore options.

## Restore behavior

The default mode, `safe`, moves matching windows and can launch apps or create
windows. It leaves extra windows open. Other modes also clean up extra windows
belonging to apps in the snapshot:

```sh
workspace restore coding --mode reconcile    # minimize extra windows
workspace restore coding --mode destructive  # close extra windows
```

Cleanup runs only when that app's restore operations report success. Disabled
and fullscreen saved windows are skipped. `--dev-mode` skips launching VS Code
and Cursor and leaves their extra windows alone; matched editor windows can
still be moved.

Windows are matched by app, title, and geometry. Execution resolves each selected
live window once and keeps its handle or browser ID. Missing or ambiguous
identities are skipped. This matching is heuristic, especially when titles are
unavailable.
Reuse requires title agreement or a close geometry match to the saved frame or
its remapped target. Missing, blank, or changed titles need that geometry match.

When a saved display is missing or its layout changes, window geometry is
remapped onto the current displays. The saved coordinates may no longer fit.

## Limits

Snapshots store window titles and geometry. They do not contain editor projects,
documents, or terminal sessions; reopening that context is left to the app.
Restore is limited to the apps listed in [src/app_support.rs](src/app_support.rs);
other apps can be captured but are skipped during restore.

Chromium browsers support tab capture and restore. Existing windows keep their
tabs, and missing saved URLs are reopened. New windows are created for unmatched
saved windows, preserving existing blank windows. URLs that have redirected can
be reopened as duplicates. Without window titles, capture falls back to window
order and can attach the wrong tabs. Safari windows can be moved, but their tabs
are not captured.

Capture includes visible windows on the current desktop. Restore can also find
minimized windows through Accessibility, but capture does not include windows
on other Spaces. Verification checks matching, visibility, and geometry. JSON
reports include `converged` and each matched window's `observed_minimized` state.
Verification does not check tab state or app content. Restoring the saved
stacking order is best effort.

## Development

```sh
cargo fmt --check
cargo test
node tests/browser_behavior.cjs
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The Rust tests use in-memory windows. The Node.js checks run the browser scripts
against fake browser objects. Neither moves windows on your desktop.

For a manual check with Accessibility permission granted:

```sh
workspace selftest --live
# or: cargo test --test live_smoke -- --ignored
```

This moves a window and restores the snapshot. Leave `--live` off to check
capture, planning, and verification without moving windows.

The planner lives in [src/plan.rs](src/plan.rs), execution in
[src/execute.rs](src/execute.rs), and the platform calls in
[src/macos/](src/macos/). [src/world.rs](src/world.rs) handles observation and
display remapping. The planner and verifier share the window matcher.
