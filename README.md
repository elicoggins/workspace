# workspace

`workspace` is a macOS-first developer CLI for saving and restoring desktop window workspaces with exact window geometry.

The MVP is intentionally small: one Rust binary, no daemon, no GUI, no cloud sync, no AI features, and no plugin system. It behaves like a terminal tool: deterministic commands, readable JSON on disk, useful exit codes, and scriptable output.

## Current Status

This repository is ready for a first GitHub push as a working MVP foundation.

Implemented:

- save, restore, list, inspect, delete, and configure commands
- exact window geometry capture and restore through macOS native APIs
- monitor-aware remapping when display arrangements change
- z-order capture and best-effort z-order replay
- per-window enable/disable configuration
- multi-window restore grouped by app
- Chrome multi-window tab URL capture and restore
- conservative app support registry with tests and fixtures
- automated CI for format, tests, Clippy, and release build on macOS

The code is still macOS-first and intentionally relies on real macOS APIs for capture/restore. Unit and fixture tests cover the pure logic and policy behavior; real window movement still needs manual verification on a developer machine with Accessibility permission.

## Commands

```bash
workspace save coding
workspace restore coding
workspace restore coding --dry-run
workspace restore coding --dev-mode
workspace configure coding
workspace configure coding --list
workspace configure coding --disable 1 --disable 2
workspace configure coding --enable 1
workspace list
workspace inspect coding
workspace inspect coding --json
workspace delete coding
```

Global flags:

```bash
workspace --json list
workspace --verbose save coding
```

## Architecture

The implementation keeps native macOS code behind a small boundary and keeps pure behavior testable.

```text
src/main.rs                  CLI entrypoint and process exit handling
src/lib.rs                   command routing
src/cli.rs                   clap command definitions
src/model.rs                 JSON snapshot and restore report schema
src/storage.rs               deterministic local snapshot storage
src/filter.rs                visible-window filtering rules
src/capture.rs               capture orchestration
src/configure.rs             interactive snapshot window enable/disable UI
src/restore.rs               monitor remapping and restore orchestration
src/output.rs                terminal and JSON output
src/macos/display.rs         CoreGraphics display enumeration
src/macos/window.rs          CGWindowListCopyWindowInfo window enumeration
src/macos/app.rs             NSWorkspace / NSRunningApplication helpers
src/macos/accessibility.rs   Accessibility API move/resize control
```

## Snapshot Storage

Snapshots are stored as pretty JSON under the platform data directory. On macOS this normally resolves to:

```text
~/Library/Application Support/workspace
```

Files are named `<workspace-name>.json`. Names are restricted to ASCII letters, numbers, dots, dashes, and underscores so commands remain safe for scripts.

Writes are atomic: the CLI writes a temporary file in the same directory, syncs it, and renames it into place.

## Capture

`workspace save <name>` captures:

- app name
- process name
- bundle identifier when available
- process id
- CoreGraphics window id
- title
- exact global frame: x, y, width, height
- display id and display frame
- monitor-relative frame
- z-order from CoreGraphics list order
- fullscreen/minimized placeholders for schema stability
- per-window enabled/disabled restore configuration
- Chrome tab URLs and active-tab state for each Chrome window

Capture uses `CGWindowListCopyWindowInfo` because it is fast and does not require Accessibility permission. The tool filters normal visible windows only: layer zero, onscreen, non-transparent, non-trivial size, and not known system utility surfaces like Dock, Spotlight, Notification Center, Control Center, or WindowServer-owned overlays.

## Restore

`workspace restore <name>` requires macOS Accessibility permission because public CoreGraphics APIs are read-only for normal app windows.

Grant permission in:

```text
System Settings > Privacy & Security > Accessibility
```

Restore flow:

1. Load the JSON snapshot.
2. Enumerate current displays.
3. Map saved displays to current displays.
4. Compute a target frame for each saved window.
5. Skip windows disabled by `workspace configure`.
6. Skip apps that are not in the supported restore allowlist.
7. Launch a missing supported app by bundle identifier when possible.
8. Wait for newly launched apps to expose Accessibility windows.
9. Group windows by app so multi-window apps are restored together.
10. Create missing app windows when possible.
11. Match live Accessibility windows by title and geometry without reusing one live window for multiple saved windows.
12. Set exact size and position.
13. Read back the Accessibility frame and retry once if needed.
14. Replay z-order for restored windows from back to front.
15. Return a per-window restore report.

Chrome snapshots saved with this version record each Chrome window's tab URLs and active tab. During restore, Chrome windows with saved tab metadata are recreated with those URLs before geometry is applied. Older snapshots still restore Chrome window geometry, but they cannot recreate tabs that were not captured.

Use `--dev-mode` while developing inside VS Code or Cursor. It prevents current editor bundle IDs from being launched or targeted by destructive lifecycle behavior if they are missing. Geometry restore still works for already-running editor windows.

## Configure

`workspace configure <name>` opens an interactive checkbox list for the saved snapshot. Checked windows are enabled for restore; unchecked windows are saved as disabled and are skipped before any app launch or window creation is attempted.

The checkbox list is keyboard-driven. Use up/down arrows to move, space to toggle a window, and enter to save the selection. Mouse selection depends on terminal mouse reporting and is not reliable in VS Code's integrated terminal.

For non-interactive configuration, list windows with indexes and toggle them directly:

```bash
workspace configure coding --list
workspace configure coding --disable 1 --disable 2
workspace configure coding --enable 1
```

Disabled windows remain in the JSON snapshot so they can be re-enabled later.

## Supported Apps

The MVP intentionally restores only apps that have been verified to behave reliably. Unsupported apps remain in snapshots, but `workspace restore` skips them with a clear per-window message instead of trying best-effort native operations that may resize the wrong window or return misleading errors.

Currently enabled for full geometry and z-order restore in the compatibility registry:

- Visual Studio Code: `com.microsoft.VSCode`
- Google Chrome: `com.google.Chrome`
- Safari: `com.apple.Safari`
- Terminal: `com.apple.Terminal`
- iTerm2: `com.googlecode.iterm2`
- Warp: `dev.warp.Warp-Stable`
- Cursor: `com.todesktop.230313mzl4w4u92`
- Xcode: `com.apple.dt.Xcode`
- Finder: `com.apple.finder`
- Notes: `com.apple.Notes`
- Music: `com.apple.Music`
- Messages: `com.apple.MobileSMS`

The automated suite verifies registry coverage, multi-window dry-run restore planning for every supported app, Chrome tab fixtures, and distinct Accessibility window matching. Real Accessibility behavior can still vary by app and macOS version, so app-specific regressions should be captured as fixtures and tests before changing matching or restore behavior.

Add or adjust support in [src/app_support.rs](src/app_support.rs) after verifying save, relaunch, resize, and z-order behavior.

Promotion checklist for a new supported app:

1. Add or update the app entry in [src/app_support.rs](src/app_support.rs).
2. Add a fixture window to [tests/fixtures/common_apps.workspace.json](tests/fixtures/common_apps.workspace.json) if the app is common enough to track permanently.
3. Add policy assertions in [tests/app_support.rs](tests/app_support.rs) and restore-planning assertions in [tests/restore_policy.rs](tests/restore_policy.rs).
4. Manually verify `save`, `restore --dry-run`, `restore`, relaunch, resize, and z-order with one and multiple windows.
5. Run the full verification suite before shipping the change.

## Monitor Remapping

The restore algorithm is exact when the original display is still present with the same identity and geometry.

If a saved monitor is missing or the arrangement changed, `workspace` remaps the window proportionally using the saved monitor-relative frame:

```text
relative_x = (window_x - saved_display_x) / saved_display_width
relative_y = (window_y - saved_display_y) / saved_display_height
relative_w = window_width / saved_display_width
relative_h = window_height / saved_display_height
```

The relative frame is projected onto the best available current display. The current display is selected by stable id first, then exact numeric id/frame, then closest area and aspect ratio with a small preference for the primary display.

After projection, frames are clamped only as needed to prevent windows from spawning fully or mostly off-screen. Exact saved coordinates are preserved for unchanged display arrangements.

## Example Snapshot

See [examples/coding.workspace.json](examples/coding.workspace.json).

```json
{
  "version": 1,
  "name": "coding",
  "created_at": "2026-05-12T15:30:00Z",
  "host": {
    "hostname": "macbook-pro",
    "os": "macos",
    "arch": "aarch64"
  },
  "displays": [
    {
      "id": "cgdisplay-1",
      "numeric_id": 1,
      "name": "Built-in Display",
      "frame": {
        "x": 0,
        "y": 0,
        "width": 2560,
        "height": 1440
      },
      "scale_factor": 2,
      "is_primary": true
    }
  ],
  "windows": [
    {
      "window_id": 231,
      "app_name": "Visual Studio Code",
      "process_name": "Code",
      "bundle_id": "com.microsoft.VSCode",
      "pid": 1234,
      "title": "api.ts",
      "frame": {
        "x": 0,
        "y": 0,
        "width": 1728,
        "height": 1415
      },
      "display_id": "cgdisplay-1",
      "display_frame": {
        "x": 0,
        "y": 0,
        "width": 2560,
        "height": 1440
      },
      "display_relative_frame": {
        "x": 0,
        "y": 0,
        "width": 0.675,
        "height": 0.9826388889
      },
      "z_order": 0,
      "fullscreen": false,
      "minimized": false
    }
  ]
}
```

## Build

Install Rust on macOS, then build:

```bash
rustup toolchain install stable
cargo build --release
```

The binary will be at:

```text
target/release/workspace
```

For local development:

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- save coding
cargo run -- restore coding --dry-run
```

Before pushing:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The GitHub Actions workflow in [.github/workflows/ci.yml](.github/workflows/ci.yml) runs the same checks on `macos-latest`.

## GitHub Push Checklist

This project is ready to push as a source repository. Before the first push:

1. Create the GitHub repository.
2. Replace or remove `publish = false` in [Cargo.toml](Cargo.toml) only if you plan to publish the crate.
3. Add the final repository URL to [Cargo.toml](Cargo.toml) as `repository = "https://github.com/<owner>/<repo>"` once the URL exists.
4. Run the local verification suite from the previous section.
5. Commit source, tests, fixtures, [Cargo.lock](Cargo.lock), README, and CI config.

Do not commit `target/`, local environment files, logs, or generated editor metadata.

## Limitations

- macOS only in the MVP.
- Accessibility permission is required for restore.
- Some apps deny or partially ignore Accessibility move/resize requests.
- Restore uses an explicit app allowlist; unknown apps and windows without bundle identifiers are skipped by default.
- Fullscreen Spaces windows are captured but skipped during restore.
- Z-order is captured from CoreGraphics order and restored only best-effort by activation.
- Chrome tabs are restored for snapshots saved with tab metadata. Other browsers are treated like normal app windows for now.
- Document reopening is intentionally not implemented.
- Stable display identity is currently based on CoreGraphics display ids and geometry; future versions can enrich this with UUID/serial data.

## Roadmap

1. Add app-specific tab/session adapters for Safari and developer tools where public automation support is reliable.
2. Improve display identity with UUID/serial metadata where public APIs expose it reliably.
3. Add richer window matching heuristics for apps with duplicate titles.
4. Add optional import/export paths for snapshots.
5. Add manual ignored-app configuration.
6. Add integration tests gated behind macOS Accessibility permission.
7. Consider a small helper only after the CLI MVP proves reliable without one.
