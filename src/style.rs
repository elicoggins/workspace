//! Terminal styling helpers.
//!
//! Honours `NO_COLOR` and only emits ANSI sequences when stdout is a TTY.
//! All helpers are zero-allocation for the no-color path.

use std::io::IsTerminal;
use std::sync::OnceLock;

use anstyle::{AnsiColor, Color, Style};

static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        std::io::stdout().is_terminal()
    })
}

fn paint(style: Style, text: &str) -> String {
    if enabled() {
        format!("{style}{text}{style:#}")
    } else {
        text.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint(Style::new().bold(), s)
}
pub fn dim(s: &str) -> String {
    paint(Style::new().dimmed(), s)
}
pub fn green(s: &str) -> String {
    paint(
        Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
        s,
    )
}
pub fn yellow(s: &str) -> String {
    paint(
        Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        s,
    )
}
pub fn red(s: &str) -> String {
    paint(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))), s)
}
pub fn cyan(s: &str) -> String {
    paint(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))), s)
}

/// `[ ok ]`, `[warn]`, `[fail]`, `[skip]`, `[part]` — fixed 6-char width.
pub fn tag_ok() -> String {
    green("[ ok ]")
}
pub fn tag_warn() -> String {
    yellow("[warn]")
}
pub fn tag_fail() -> String {
    red("[fail]")
}
pub fn tag_skip() -> String {
    dim("[skip]")
}
pub fn tag_part() -> String {
    yellow("[part]")
}
pub fn tag_plan() -> String {
    cyan("[plan]")
}
