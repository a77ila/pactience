//! A minimal stderr progress bar for potentially slow passes.
//!
//! The bar is a convenience for interactive runs only: it activates solely
//! on a terminal at the default verbosity. `-v`/`-vv` already log one line
//! per item and `--quiet` suppresses everything but errors, so the bar
//! stays off in both cases. It writes only to stderr, keeping the table
//! and JSON reports on stdout untouched.

use std::io::IsTerminal;

use crate::logging::{Level, Logger};

/// Width of the bar itself, in characters.
const BAR_WIDTH: usize = 30;
/// Longest item label shown after the counts, to keep the line short.
const LABEL_MAX: usize = 40;

/// A redraw-in-place progress bar. All methods are no-ops when inactive.
pub struct Progress {
    total: usize,
    current: usize,
    active: bool,
}

impl Progress {
    /// Start a bar for `total` steps. Inactive (silent) unless stderr is a
    /// terminal and the verbosity would not already narrate each step.
    pub fn start(total: usize, log: &Logger) -> Self {
        Self::start_on(total, log, std::io::stderr().is_terminal())
    }

    /// `start` with the terminal detection injectable, so tests do not
    /// depend on whether the test process itself runs on a TTY.
    fn start_on(total: usize, log: &Logger, stderr_is_terminal: bool) -> Self {
        let active = total > 0
            && enabled_by_verbosity(log)
            && stderr_is_terminal
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        Progress {
            total,
            current: 0,
            active,
        }
    }

    /// Advance one step, naming the item currently being processed.
    pub fn advance(&mut self, log: &Logger, label: &str) {
        if !self.active {
            return;
        }
        self.current += 1;
        eprint!("\r\x1b[2K{}", render(self.current, self.total, label));
        log.set_progress_active(true);
    }

    /// Erase the bar line. Consumes the bar so it cannot linger.
    pub fn finish(self, log: &Logger) {
        if self.active {
            eprint!("\r\x1b[2K");
            log.set_progress_active(false);
        }
    }
}

/// The bar is redundant with `-v` per-item lines and forbidden by `--quiet`.
fn enabled_by_verbosity(log: &Logger) -> bool {
    log.enabled(Level::Warn) && !log.enabled(Level::Info)
}

/// One bar line: `[###----] 3/10 package-name`.
fn render(current: usize, total: usize, label: &str) -> String {
    let current = current.min(total);
    let filled = (current * BAR_WIDTH)
        .checked_div(total)
        .unwrap_or(BAR_WIDTH);
    let bar = "#".repeat(filled) + &"-".repeat(BAR_WIDTH - filled);
    let label: String = label.chars().take(LABEL_MAX).collect();
    format!("[{bar}] {current}/{total} {label}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn logger(args: &[&str]) -> Logger {
        let cli = Cli::try_parse_from(std::iter::once("pactience").chain(args.iter().copied()))
            .expect("test CLI args must parse");
        Logger::from(&cli)
    }

    #[test]
    fn verbosity_gating() {
        assert!(enabled_by_verbosity(&logger(&[])));
        assert!(!enabled_by_verbosity(&logger(&["--quiet"])));
        assert!(!enabled_by_verbosity(&logger(&["-v"])));
        assert!(!enabled_by_verbosity(&logger(&["-vv"])));
    }

    #[test]
    fn render_proportions() {
        assert_eq!(render(0, 4, "a"), format!("[{}] 0/4 a", "-".repeat(30)));
        assert_eq!(
            render(2, 4, "b"),
            format!("[{}{}] 2/4 b", "#".repeat(15), "-".repeat(15))
        );
        assert!(render(4, 4, "c").starts_with(&format!("[{}]", "#".repeat(30))));
        // Out-of-range input is clamped instead of panicking.
        assert!(render(9, 4, "c").contains("4/4"));
    }

    #[test]
    fn render_truncates_long_labels() {
        let line = render(1, 2, &"x".repeat(100));
        assert_eq!(
            line,
            format!(
                "[{}] 1/2 {}",
                "#".repeat(15) + &"-".repeat(15),
                "x".repeat(40)
            )
        );
    }

    #[test]
    fn inactive_without_terminal() {
        // libtest captures output in-process without redirecting fd 2, so
        // the real stderr may still be a TTY; pass the flag explicitly.
        let log = logger(&[]);
        let mut progress = Progress::start_on(3, &log, false);
        assert!(!progress.active);
        // Inactive bars are silent no-ops.
        progress.advance(&log, "pkg");
        progress.finish(&log);
    }
}
