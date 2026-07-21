//! Leveled diagnostic logging to stderr.
//!
//! Diagnostics (warnings, progress, debug detail) go to stderr so stdout
//! stays reserved for the report itself (table or JSON). The report is never
//! affected by the chosen verbosity.

use std::cell::Cell;

use crate::cli::Cli;

/// Diagnostic severity. Ordered: a lower level is more severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Always shown, even with `--quiet`.
    Error = 0,
    /// Default level: degraded-operation warnings.
    Warn = 1,
    /// `-v`: one line per action (discovery counts, per-package resolution,
    /// policy summary, ...).
    Info = 2,
    /// `-vv`: internal detail (cache hits, URLs, requirement classification,
    /// per-candidate verdicts).
    Debug = 3,
}

pub struct Logger {
    level: Level,
    /// Set while a progress-bar line is on the terminal; diagnostics erase
    /// it before printing so the bar can be redrawn cleanly afterwards.
    progress_active: Cell<bool>,
}

impl Logger {
    pub fn new(level: Level) -> Self {
        Logger {
            level,
            progress_active: Cell::new(false),
        }
    }

    /// Is `level` currently enabled? (Testable without capturing stderr.)
    pub fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.emit(Level::Error, "error", message);
    }

    /// Progress notice shown by default; only `--quiet` suppresses it. Used
    /// for the few lines a user should always see (e.g. the startup "checking
    /// N packages" summary before potentially slow network lookups).
    pub fn notice(&self, message: impl AsRef<str>) {
        if self.notice_enabled() {
            self.clear_progress_line();
            eprintln!("info: {}", message.as_ref());
        }
    }

    /// Is the default-visible notice level enabled? (Testable without
    /// capturing stderr.) Only the quiet mapping (`Level::Error`) disables it.
    pub fn notice_enabled(&self) -> bool {
        self.level > Level::Error
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.emit(Level::Warn, "warning", message);
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.emit(Level::Info, "info", message);
    }

    pub fn debug(&self, message: impl AsRef<str>) {
        self.emit(Level::Debug, "debug", message);
    }

    fn emit(&self, level: Level, label: &str, message: impl AsRef<str>) {
        if self.enabled(level) {
            self.clear_progress_line();
            eprintln!("{label}: {}", message.as_ref());
        }
    }

    /// Mark that a progress-bar line is currently on the terminal (or gone).
    pub(crate) fn set_progress_active(&self, active: bool) {
        self.progress_active.set(active);
    }

    /// Erase an in-progress progress-bar line so a diagnostic prints on a
    /// clean line; the bar redraws itself on the next tick.
    fn clear_progress_line(&self) {
        if self.progress_active.get() {
            eprint!("\r\x1b[2K");
        }
    }
}

impl From<&Cli> for Logger {
    fn from(cli: &Cli) -> Self {
        let level = if cli.quiet {
            Level::Error
        } else {
            match cli.verbose {
                0 => Level::Warn,
                1 => Level::Info,
                _ => Level::Debug,
            }
        };
        Logger::new(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("pactience").chain(args.iter().copied()))
            .expect("test CLI args must parse")
    }

    #[test]
    fn default_level_is_warn() {
        let log = Logger::from(&cli(&[]));
        assert!(log.enabled(Level::Error));
        assert!(log.enabled(Level::Warn));
        assert!(!log.enabled(Level::Info));
        assert!(!log.enabled(Level::Debug));
    }

    #[test]
    fn quiet_shows_only_errors() {
        let log = Logger::from(&cli(&["--quiet"]));
        assert!(log.enabled(Level::Error));
        assert!(!log.enabled(Level::Warn));
        assert!(!log.enabled(Level::Info));
    }

    #[test]
    fn verbose_levels() {
        let log = Logger::from(&cli(&["-v"]));
        assert!(log.enabled(Level::Info));
        assert!(!log.enabled(Level::Debug));

        let log = Logger::from(&cli(&["-vv"]));
        assert!(log.enabled(Level::Debug));

        // Repetition beyond two stays at debug.
        let log = Logger::from(&cli(&["-vvvv"]));
        assert!(log.enabled(Level::Debug));
    }

    #[test]
    fn notice_shown_by_default_hidden_by_quiet() {
        assert!(Logger::from(&cli(&[])).notice_enabled());
        assert!(Logger::from(&cli(&["-v"])).notice_enabled());
        assert!(Logger::from(&cli(&["-vv"])).notice_enabled());
        assert!(!Logger::from(&cli(&["--quiet"])).notice_enabled());
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        assert!(Cli::try_parse_from(["pactience", "-q", "-v"]).is_err());
    }
}
