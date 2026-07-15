# workspace

A macOS CLI that captures your desktop window layout and puts it back, exactly.

One Rust binary. No daemon, no GUI, no cloud, no AI. Deterministic commands, JSON on disk, real exit codes, scriptable output.

## Install

```bash
rustup toolchain install stable
cargo build --release
# binary: target/release/workspace
```

Grant Accessibility once: **System Settings → Privacy & Security → Accessibility**. Required for restore; not needed for save.

## Quick Start

```bash
workspace save coding              # snapshot current layout
workspace apply coding             # restore it (planner-driven, recommended)
workspace apply coding --dry-run   # preview without touching the system
workspace diff coding              # see what's different and what would change
workspace list                     # all saved workspaces
```

## Commands

| Command | What it does |
|---|---|
| `save <name>` | Capture visible windows, geometry, displays, Chrome tabs. `--force` to overwrite. |
| `apply <name>` | **Plan → execute → verify** with a journal. Supports `--converge N` to re-plan until convergence. |
| `restore <name>` | Legacy single-pass restore. Same effect as `apply --converge 1`. |
| `plan <name>` | Show the operations `apply` would run, without doing them. |
| `verify <name>` | Compare the live world to a snapshot; reports accuracy & geometry drift. |
| `diff <name>` | `plan` and `verify` together. |
| `list` / `inspect <name>` / `delete <name>` | Manage snapshots. |
| `configure <name>` | Enable/disable specific windows in a snapshot. |
| `doctor` | Check Accessibility, data dir, displays. |
| `completions <shell>` | Print shell completion script (bash/zsh/fish/powershell/elvish). |

Global flags: `--json` (machine-readable), `--verbose` (debug tracing).

## Restore Modes

Pass `--mode` to `apply`, `restore`, `plan`, or `diff`:

- **`safe`** (default): only repositions, launches, and creates windows. Never minimizes or closes anything.
- **`reconcile`**: may minimize extra windows of apps being restored.
- **`destructive`**: may close extra windows. Also reachable via `--destructive`.

`--dev-mode` additionally protects VS Code and Cursor from destructive lifecycle actions — useful when you're driving the CLI from inside your editor.

## Convergence

```bash
workspace apply coding --converge 3
```

Each iteration: re-observe the world → plan → execute → verify. Stops early on 100% match. Use this when apps drift after restore (e.g. Chrome resizing itself on tab load).

## Output: The Journal

`apply` prints (or emits as JSON with `--json`) an **execution journal** — one entry per planned op with status, duration, attempts, and a message:

```
[  0] [OK  ] reposition     Code (12 ms) — repositioned
[  1] [PART] reposition     Terminal (148 ms, x2) — AX match failed
[  2] [SKIP] launch         Finder (0 ms) — dry run
```

Status: `OK` (success), `PART` (partial — op ran but post-condition not observed), `SKIP`, `FAIL`.

## Shell Completions

```bash
workspace completions zsh  >> ~/.zfunc/_workspace
workspace completions bash >> ~/.local/share/bash-completion/completions/workspace
workspace completions fish >  ~/.config/fish/completions/workspace.fish
```

## How It Works

```
save:   CGWindowList → filter → JSON snapshot (~/Library/Application Support/workspace/<name>.json)
apply:  load snapshot → observe live world → planner builds RestorePlan
        → Executor runs ops (AX move/resize, NSWorkspace launch, AppleScript for Chrome)
        → Journal records every step → verify checks the result
```

The planner is a pure function (`plan::plan_restore`) over snapshot + observed world. The executor is a trait with two impls:

- `MacOsExecutor` — drives real AX / NSWorkspace / Chrome
- `SimulatedExecutor` — pure in-memory; powers tests and `--dry-run`

This split is why `apply --dry-run` faithfully shows what *would* happen without any FFI calls.

## Multi-Monitor

If a saved display is still present with the same identity, pixels are exact. Otherwise windows are remapped proportionally onto the closest matching current display (stable id → numeric id → area/aspect/primary).

## Supported Apps

The restore allowlist (others are captured but skipped during restore):

VS Code, Cursor, Xcode, Terminal, iTerm2, Warp, Chrome, Safari, Finder, Notes, Music, Messages.

Chrome additionally captures and re-opens tab URLs per window. Other browsers are treated as plain windows.

Add an app in [src/app_support.rs](src/app_support.rs); add fixture coverage in [tests/](tests).

## Snapshot Format

Pretty JSON, atomic writes, restricted name charset (`[A-Za-z0-9._-]`). See [examples/coding.workspace.json](examples/coding.workspace.json).

## Development

```bash
cargo fmt
cargo test           # 60+ tests: unit, integration, property-based (proptest)
cargo clippy --all-targets -- -D warnings
```

Architecture:

```
src/cli.rs            clap definitions
src/lib.rs            command routing
src/capture.rs        save orchestration
src/plan.rs           pure planner (snapshot + world → RestorePlan)
src/execute.rs        Executor trait, MacOsExecutor, SimulatedExecutor, ExecutionJournal
src/restore.rs        legacy restore path + world observation helpers
src/verify.rs         compare live world to snapshot
src/storage.rs        atomic JSON read/write
src/macos/*           AX, NSWorkspace, CoreGraphics, Chrome AppleScript
tests/plan_properties.rs   proptest invariants (idempotency, safe-mode, convergence)
```

## Limitations

- macOS only.
- Restore requires Accessibility permission.
- Unknown apps are skipped (allowlist-only by design).
- Fullscreen Spaces windows are captured but not restored.
- Z-order replay is best-effort (activation-based).
- `MacOsExecutor` minimize/close use AX `AXMinimized` / `AXCloseButton`; some apps may ignore them.
