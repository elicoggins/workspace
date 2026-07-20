# workspace

A macOS CLI that captures your desktop window layout and puts it back, exactly. Written in Rust.

## Install

```bash
rustup toolchain install stable
cargo build --release
# binary: target/release/workspace
```

Grant two permissions once in **System Settings → Privacy & Security**:

- **Accessibility** — required to move windows (`restore`).
- **Screen Recording** — required for window *titles*. Without it, save still works but matching degrades to geometry-only and browser-tab attribution falls back to window order. `workspace doctor` warns when titles are invisible.

## Quick Start

```bash
workspace save coding              # snapshot current layout
workspace restore coding           # restore it
workspace restore coding --dry-run # preview without touching the system
workspace diff coding              # see what's different and what would change
workspace list                     # all saved workspaces
```

## Commands

| Command | What it does |
|---|---|
| `save <name>` | Capture visible windows, geometry, displays, browser tabs. `--force` to overwrite. |
| `restore <name>` | **Plan → execute → verify** with a journal. `--converge N` re-plans until convergence, then replays saved z-order. |
| `plan <name>` | Show the operations `restore` would run, without doing them. |
| `verify <name>` | Compare the live world to a snapshot; reports accuracy & geometry drift. |
| `diff <name>` | `plan` and `verify` together. |
| `list` / `inspect <name>` / `delete <name>` | Manage snapshots. |
| `configure <name>` | Enable/disable specific windows in a snapshot. |
| `doctor` | Check Accessibility, Screen Recording (via title visibility), data dir, displays. |
| `selftest [--live]` | Exercise the real pipeline end-to-end; `--live` briefly moves one window and restores it. |
| `completions <shell>` | Print shell completion script (bash/zsh/fish/powershell/elvish). |

Global flags: `--json` (machine-readable), `--verbose` (debug tracing).

## Restore Modes

Pass `--mode` to `restore`, `plan`, or `diff`:

- **`safe`** (default): only repositions, launches, and creates windows. Never minimizes or closes anything.
- **`reconcile`**: may minimize extra windows of apps being restored.
- **`destructive`**: may close extra windows. Also reachable via `--destructive`.

`--dev-mode` additionally protects VS Code and Cursor from destructive lifecycle actions — useful when you're driving the CLI from inside your editor.

## Convergence

```bash
workspace restore coding --converge 3
```

Each iteration: re-observe the world → plan → execute → verify. Stops early on 100% match or when the plan has nothing actionable left.

## Output: The Journal

`restore` prints (or emits as JSON with `--json`) an **execution journal** — one entry per planned op with status, duration, attempts, and a message:

```
[  0] [OK  ] reposition     Code (12 ms) — repositioned
[  1] [OK  ] chrome_tabs    Google Chrome (418 ms) — restored 1 browser window(s)
[  2] [SKIP] skip           Calendar (0 ms) — this app is not in the supported restore allowlist yet
```

Status: `OK` (success), `PART` (partial — op ran but post-condition not observed), `SKIP`, `FAIL`.

## How It Works

```
save:   CGWindowList (+ AX fullscreen flags, browser tabs) → filter
        → JSON snapshot (~/Library/Application Support/workspace/<name>.json)
restore: load snapshot → observe live world (CG windows + AX minimized windows)
        → planner builds RestorePlan → Executor runs ops
        (AX move/resize, NSWorkspace launch, JXA for browser tabs)
        → Journal records every step → verify checks the result
        → saved z-order replayed
```

The planner is a pure function (`plan::plan_restore`) over snapshot + observed world. The executor is a trait with two impls:

- `MacOsExecutor` — drives real AX / NSWorkspace / browser scripting
- `SimulatedExecutor` — pure in-memory; powers tests and `--dry-run`

Because the unit suite runs against the simulation, `workspace selftest` (and `cargo test --test live_smoke -- --ignored`) exists to prove the *real* executor works on your machine — run it before trusting a new build.

### Window matching

Saved windows are matched to live windows by bundle id, title similarity, and geometry, globally best-pair-first. When titles are unavailable (no Screen Recording permission), only near-exact geometry counts as identity — a weak match will never relocate or rewrite some other window of the same app.

## Multi-Monitor

If a saved display is still present with the same identity, pixels are exact. Otherwise windows are remapped proportionally onto the closest matching current display (stable id → numeric id → area/aspect/primary).

## Supported Apps

The restore allowlist (others are captured but skipped during restore):

VS Code, Cursor, Xcode, Terminal, iTerm2, Warp, Finder, Notes, Music, Messages, Safari, and the Chromium family — Chrome, Chrome Canary, Brave, Edge, Chromium.

**Browser tabs**: Chromium-family browsers get per-window tab capture and restore. On `restore`, a matched browser window that lost some saved tabs gets them re-opened (add-only — nothing you have open is closed), guarded so tabs are never grafted onto an unrelated window. Safari windows are repositioned but tabs are not captured (different scripting model).

Add an app in [src/app_support.rs](src/app_support.rs); add fixture coverage in [tests/](tests).

## Snapshot Format

Pretty JSON, atomic writes, restricted name charset (`[A-Za-z0-9._-]`). Schema-versioned; newer-schema files are refused with exit code 7.

## Development

```bash
cargo fmt
cargo test                                   # 65+ tests: unit, integration, property-based
cargo clippy --all-targets -- -D warnings
cargo test --test live_smoke -- --ignored    # real-executor smoke test (moves a window!)
```

Architecture:

```
src/cli.rs            clap definitions
src/lib.rs            command routing + restore loop
src/capture.rs        save orchestration (CG + AX enrichment + browser tabs)
src/plan.rs           pure planner (snapshot + world → RestorePlan)
src/execute.rs        Executor trait, MacOsExecutor, SimulatedExecutor, ExecutionJournal
src/world.rs          world observation, display remapping, z-order replay, doctor
src/verify.rs         compare live world to snapshot (same matcher as the planner)
src/selftest.rs       end-to-end checks against the real machine
src/storage.rs        atomic JSON read/write
src/macos/*           AX, NSWorkspace, CoreGraphics, browser JXA
tests/plan_properties.rs   proptest invariants (idempotency, safe-mode, convergence)
tests/live_smoke.rs        ignored-by-default real-executor test
```

## Limitations

- macOS only.
- `restore` requires Accessibility permission; reliable matching wants Screen Recording too.
- Unknown apps are skipped (allowlist-only by design).
- Fullscreen windows (detected via AX at save time) are captured but not restored.
- Minimized windows are observed via AX and un-minimized when repositioned, but windows on other Spaces are invisible to capture.
- Browser tab reconciliation matches exact URLs; a tab that redirected since capture may be re-opened as a duplicate.
- Safari tabs are not captured.
- Z-order replay is best-effort (activation-based).
