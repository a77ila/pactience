//! Command-line interface definition.

use std::path::PathBuf;

use clap::Parser;

use crate::config::{AurHelper, DependencyPolicy};
use crate::model::PackageSource;

/// Enforce a minimum package age before upgrading Arch Linux packages.
#[derive(Debug, Parser)]
#[command(name = "pactience", version, about)]
pub struct Cli {
    /// Perform the safe upgrade set with pacman and the configured AUR
    /// helper. Without this flag the tool only reports what it would do
    /// (dry-run).
    #[arg(long)]
    pub apply: bool,

    /// Emit machine-readable JSON instead of the summary table.
    #[arg(long)]
    pub json: bool,

    /// Path to the configuration file
    /// [default: ~/.config/pactience/config.toml].
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Minimum package age in days required before a package may be upgraded.
    #[arg(short = 'm', long, value_name = "DAYS")]
    pub min_age_days: Option<u32>,

    /// Persist DAYS as min_age_days in the configuration file, then exit.
    /// The file is created from the template when missing; an existing
    /// active min_age_days line is replaced in place.
    #[arg(long, value_name = "DAYS", conflicts_with = "min_age_days")]
    pub set_min_age: Option<u32>,

    /// How to handle upgrades that require younger dependencies.
    #[arg(long, value_enum)]
    pub dependency_policy: Option<DependencyPolicy>,

    /// Use the AUR `LastModified` field as a heuristic publication time for
    /// AUR packages. Off by default because the AUR exposes no per-version
    /// publication timestamp.
    #[arg(long)]
    pub aur_heuristic: bool,

    /// Allow upgrades whose publication time could not be determined.
    /// By default they are blocked.
    #[arg(long)]
    pub allow_unknown: bool,

    /// Disable the AUR git-history lookup (accurate per-version dates, at the
    /// cost of a shallow bare clone per AUR package on first encounter).
    /// RPC-based sources are still used.
    #[arg(long)]
    pub no_aur_git: bool,

    /// AUR helper used to discover and apply AUR upgrades; `none` disables
    /// AUR handling entirely.
    #[arg(long, value_enum, value_name = "HELPER")]
    pub aur_helper: Option<AurHelper>,

    /// Which package sources to manage for this run, as a comma-separated
    /// list (`repo,aur`, `repo`, or `aur`). Overrides the sources setting
    /// from the configuration file and suppresses the first-run/upgrade
    /// prompt for it.
    #[arg(long, value_enum, value_name = "SOURCES", value_delimiter = ',')]
    pub sources: Option<Vec<PackageSource>>,

    /// Increase diagnostic verbosity on stderr. Repeat for more detail:
    /// `-v` shows one line per action, `-vv` shows internal detail.
    /// Never affects the report on stdout.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress all diagnostics except errors.
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// When to use ANSI colors in the table output. `auto` (default) colors
    /// only when stdout is a terminal and NO_COLOR is not set.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Print only the summary line (and hints), without the per-package
    /// table. Keeps output volume small for CI/CD logs.
    #[arg(long, conflicts_with = "json")]
    pub summary_only: bool,

    /// Remove the entire cache directory [default: ~/.cache/pactience],
    /// including the publication cache and AUR git clones, then exit.
    /// No analysis is performed.
    #[arg(long)]
    pub clear_cache: bool,
}

/// Color output mode for `--color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}
