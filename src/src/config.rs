//! Configuration file handling for `~/.config/pactience/config.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::Cli;
use crate::error::{Error, Result};

/// How to resolve conflicts between the age policy and dependency
/// requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyPolicy {
    /// Automatically promote younger dependencies required by allowed
    /// upgrades (default).
    DependencyRespecting,
    /// Never promote; block any upgrade whose dependency requirements are
    /// not satisfied by installed packages or age-allowed candidates.
    StrictClosure,
}

impl std::fmt::Display for DependencyPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DependencyPolicy::DependencyRespecting => write!(f, "dependency-respecting"),
            DependencyPolicy::StrictClosure => write!(f, "strict-closure"),
        }
    }
}

/// AUR helper used to discover (`-Qua`) and apply (`-S`) AUR upgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AurHelper {
    /// paru (default).
    Paru,
    /// yay.
    Yay,
    /// Disable AUR handling entirely: no AUR discovery, no AUR upgrades.
    None,
}

impl AurHelper {
    /// Executable name, or `None` when AUR handling is disabled.
    pub fn program(&self) -> Option<&'static str> {
        match self {
            AurHelper::Paru => Some("paru"),
            AurHelper::Yay => Some("yay"),
            AurHelper::None => None,
        }
    }
}

impl std::fmt::Display for AurHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AurHelper::Paru => write!(f, "paru"),
            AurHelper::Yay => write!(f, "yay"),
            AurHelper::None => write!(f, "none"),
        }
    }
}

/// Effective configuration after merging file values with CLI overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub min_age_days: u32,
    pub always_allow: Vec<String>,
    pub always_block: Vec<String>,
    pub dependency_policy: DependencyPolicy,
    /// How long a *negative* cache entry ("publication unknown") stays valid.
    /// Positive results are immutable facts and never expire.
    pub cache_ttl_secs: u64,
    pub allow_unknown: bool,
    pub aur_heuristic: bool,
    /// Look up accurate per-version dates from AUR git history (default on).
    pub aur_git: bool,
    /// AUR helper used for discovery and upgrades.
    pub aur_helper: AurHelper,
}

/// Raw TOML representation; every field is optional so users only specify
/// what they want to change.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    min_age_days: Option<u32>,
    always_allow: Option<Vec<String>>,
    always_block: Option<Vec<String>>,
    dependency_policy: Option<DependencyPolicy>,
    cache_ttl_secs: Option<u64>,
    allow_unknown: Option<bool>,
    aur_heuristic: Option<bool>,
    aur_git: Option<bool>,
    aur_helper: Option<AurHelper>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            min_age_days: 4,
            always_allow: Vec::new(),
            always_block: Vec::new(),
            dependency_policy: DependencyPolicy::DependencyRespecting,
            cache_ttl_secs: 86_400,
            allow_unknown: false,
            aur_heuristic: false,
            aur_git: true,
            aur_helper: AurHelper::Paru,
        }
    }
}

impl Config {
    /// Load configuration from `path` (missing file = defaults), then apply
    /// CLI overrides.
    pub fn load(path: &Path, cli: &Cli) -> Result<Config> {
        let mut config = Config::default();
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let file: FileConfig = toml::from_str(&contents)
                    .map_err(|e| Error::config(path.to_path_buf(), e.to_string()))?;
                config.merge_file(file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(path.to_path_buf(), e)),
        }
        config.merge_cli(cli);
        config.validate(path)?;
        Ok(config)
    }

    fn merge_file(&mut self, file: FileConfig) {
        if let Some(v) = file.min_age_days {
            self.min_age_days = v;
        }
        if let Some(v) = file.always_allow {
            self.always_allow = v;
        }
        if let Some(v) = file.always_block {
            self.always_block = v;
        }
        if let Some(v) = file.dependency_policy {
            self.dependency_policy = v;
        }
        if let Some(v) = file.cache_ttl_secs {
            self.cache_ttl_secs = v;
        }
        if let Some(v) = file.allow_unknown {
            self.allow_unknown = v;
        }
        if let Some(v) = file.aur_heuristic {
            self.aur_heuristic = v;
        }
        if let Some(v) = file.aur_git {
            self.aur_git = v;
        }
        if let Some(v) = file.aur_helper {
            self.aur_helper = v;
        }
    }

    /// CLI flags win over the configuration file.
    fn merge_cli(&mut self, cli: &Cli) {
        if let Some(v) = cli.min_age_days {
            self.min_age_days = v;
        }
        if let Some(v) = cli.dependency_policy {
            self.dependency_policy = v;
        }
        if cli.aur_heuristic {
            self.aur_heuristic = true;
        }
        if cli.allow_unknown {
            self.allow_unknown = true;
        }
        if cli.no_aur_git {
            self.aur_git = false;
        }
        if let Some(v) = cli.aur_helper {
            self.aur_helper = v;
        }
    }

    fn validate(&self, path: &Path) -> Result<()> {
        if self.min_age_days > 3650 {
            return Err(Error::config(
                path.to_path_buf(),
                format!("min_age_days={} is unreasonably large", self.min_age_days),
            ));
        }
        for name in self.always_allow.iter().chain(self.always_block.iter()) {
            if !crate::apply::is_valid_package_name(name) {
                return Err(Error::config(
                    path.to_path_buf(),
                    format!("invalid package name in allow/block list: {name:?}"),
                ));
            }
        }
        Ok(())
    }
}

/// Template written on first execution when the default config file is
/// missing. Every option is commented out: the file documents the available
/// settings while behavior stays at defaults until the user edits it.
/// Kept in sync with `FileConfig` by the `template_parses_to_defaults` test.
pub const CONFIG_TEMPLATE: &str = r#"# pactience configuration
#
# Every option is commented out, so this file is documentation until you
# uncomment and change something. CLI flags always win over this file.

# Minimum age (in days) a package version must have before it may be
# upgraded. Versions younger than this are blocked.
# min_age_days = 4

# Whitelist: packages that are always upgraded, regardless of age.
# always_allow = ["linux", "firefox"]

# Blacklist: packages that are never upgraded. They cannot be promoted as
# dependencies either; packages needing them are blocked instead.
# always_block = ["glibc"]

# What to do when an allowed upgrade requires a younger dependency:
#   "dependency-respecting"  (default) promote the younger dependency into
#                                      the upgrade set
#   "strict-closure"         never promote; block the dependent package
# dependency_policy = "dependency-respecting"

# How long (seconds) a "publication unknown" cache entry stays valid before
# it is looked up again. Positive results are historical facts and never
# expire.
# cache_ttl_secs = 86400

# Allow upgrades whose publication time could not be determined.
# Default false: unknown age means block.
# allow_unknown = false

# AUR has no official per-version publication date. Gate AUR packages by the
# RPC LastModified field (heuristic: any PKGBUILD edit refreshes it).
# Default false.
# aur_heuristic = false

# Look up accurate per-version dates from each AUR package's git history
# (one small bare clone per package, cached and only fetched afterwards).
# Default true.
# aur_git = true

# AUR helper used to discover and apply AUR upgrades: "paru" (default),
# "yay", or "none" to disable AUR handling entirely.
# aur_helper = "paru"
"#;

/// Write the prepopulated template to `path`, creating parent directories,
/// and record the AUR helper chosen on first run as an active setting below
/// the commented template. Callers must check existence first; this never
/// truncates an existing file (exclusive create).
pub fn write_config_with_helper(path: &Path, helper: AurHelper) -> Result<()> {
    let contents =
        format!("{CONFIG_TEMPLATE}\n# Chosen on first run.\naur_helper = \"{helper}\"\n");
    write_config(path, &contents)
}

fn write_config(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.to_path_buf(), e))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, contents.as_bytes()))
        .map_err(|e| Error::io(path.to_path_buf(), e))
}

/// Probe for a supported AUR helper via `available` (an executable-presence
/// check). paru wins when both are installed.
pub fn detect_aur_helper_with(available: impl Fn(&str) -> bool) -> Option<AurHelper> {
    if available("paru") {
        Some(AurHelper::Paru)
    } else if available("yay") {
        Some(AurHelper::Yay)
    } else {
        None
    }
}

/// Detect an installed AUR helper by scanning `PATH` for executables.
pub fn detect_aur_helper() -> Option<AurHelper> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    detect_aur_helper_with(|name| dirs.iter().any(|d| is_executable(&d.join(name))))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Parse a first-run prompt answer: empty input means "take the default"
/// (`Ok(None)`), a number or name selects that helper, anything else is
/// invalid (`Err`) and the caller should ask again.
pub fn parse_helper_choice(input: &str) -> std::result::Result<Option<AurHelper>, ()> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "1" | "paru" => Ok(Some(AurHelper::Paru)),
        "2" | "yay" => Ok(Some(AurHelper::Yay)),
        "3" | "none" => Ok(Some(AurHelper::None)),
        _ => Err(()),
    }
}

/// Interactively ask which AUR helper to use, looping until a valid answer.
/// EOF or a read error yields `default` so the prompt can never hang.
pub fn prompt_aur_helper(
    input: &mut impl std::io::BufRead,
    out: &mut impl std::io::Write,
    default: AurHelper,
) -> AurHelper {
    loop {
        let _ = write!(
            out,
            "Select the AUR helper for discovering and applying AUR upgrades:\n\
             \x20 [1] paru\n\
             \x20 [2] yay\n\
             \x20 [3] none (disable AUR handling)\n\
             Choice [default: {default}]: "
        );
        let _ = out.flush();
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return default,
            Ok(_) => match parse_helper_choice(&line) {
                Ok(None) => return default,
                Ok(Some(helper)) => return helper,
                Err(()) => {
                    let _ = writeln!(out, "please answer 1, 2 or 3 (or paru/yay/none)");
                }
            },
        }
    }
}

/// Default configuration file location, honoring `XDG_CONFIG_HOME`.
pub fn default_config_path() -> PathBuf {
    config_home().join("pactience/config.toml")
}

/// Default cache file location, honoring `XDG_CACHE_HOME`.
pub fn default_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cache"));
    base.join("pactience/publications.json")
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        use clap::Parser;
        Cli::try_parse_from(std::iter::once("pactience").chain(args.iter().copied()))
            .expect("test CLI args must parse")
    }

    #[test]
    fn defaults_when_file_missing() {
        let config = Config::load(Path::new("/nonexistent/config.toml"), &cli(&[])).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.min_age_days, 4);
        assert_eq!(
            config.dependency_policy,
            DependencyPolicy::DependencyRespecting
        );
    }

    #[test]
    fn file_values_are_loaded() {
        let dir = std::env::temp_dir().join(format!("aag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
min_age_days = 7
always_allow = ["linux"]
always_block = ["glibc"]
dependency_policy = "strict-closure"
cache_ttl_secs = 3600
allow_unknown = true
aur_heuristic = true
"#,
        )
        .unwrap();
        let config = Config::load(&path, &cli(&[])).unwrap();
        assert_eq!(config.min_age_days, 7);
        assert_eq!(config.always_allow, vec!["linux"]);
        assert_eq!(config.always_block, vec!["glibc"]);
        assert_eq!(config.dependency_policy, DependencyPolicy::StrictClosure);
        assert_eq!(config.cache_ttl_secs, 3600);
        assert!(config.allow_unknown);
        assert!(config.aur_heuristic);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cli_overrides_file() {
        let dir = std::env::temp_dir().join(format!("aag-test-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "min_age_days = 7\n").unwrap();
        let config = Config::load(&path, &cli(&["--min-age-days", "2"])).unwrap();
        assert_eq!(config.min_age_days, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_toml_is_a_config_error() {
        let dir = std::env::temp_dir().join(format!("aag-test-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "min_age_days = \"four\"\n").unwrap();
        let err = Config::load(&path, &cli(&[])).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let dir = std::env::temp_dir().join(format!("aag-test-unk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "min_age_day = 7\n").unwrap();
        assert!(Config::load(&path, &cli(&[])).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn template_parses_to_defaults() {
        // Guards against template drift when options change: every key in the
        // template must be a real option, and a fully-commented template must
        // behave exactly like no file at all.
        let file: FileConfig =
            toml::from_str(CONFIG_TEMPLATE).expect("template must be valid TOML");
        let mut config = Config::default();
        config.merge_file(file);
        assert_eq!(config, Config::default());
    }

    #[test]
    fn template_documents_every_option() {
        for key in [
            "min_age_days",
            "always_allow",
            "always_block",
            "dependency_policy",
            "cache_ttl_secs",
            "allow_unknown",
            "aur_heuristic",
            "aur_git",
            "aur_helper",
        ] {
            assert!(
                CONFIG_TEMPLATE.contains(key),
                "template is missing option {key}"
            );
        }
    }

    #[test]
    fn write_template_creates_parents_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("aag-test-tpl-{}", std::process::id()));
        let path = dir.join("nested/deeper/config.toml");
        write_config(&path, CONFIG_TEMPLATE).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, CONFIG_TEMPLATE);

        // Second write must not clobber user edits.
        std::fs::write(&path, "min_age_days = 9\n").unwrap();
        assert!(write_config(&path, CONFIG_TEMPLATE).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "min_age_days = 9\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aur_helper_from_file_cli_and_default() {
        assert_eq!(Config::default().aur_helper, AurHelper::Paru);

        let dir = std::env::temp_dir().join(format!("aag-test-helper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "aur_helper = \"yay\"\n").unwrap();
        let config = Config::load(&path, &cli(&[])).unwrap();
        assert_eq!(config.aur_helper, AurHelper::Yay);
        // CLI wins over the file.
        let config = Config::load(&path, &cli(&["--aur-helper", "none"])).unwrap();
        assert_eq!(config.aur_helper, AurHelper::None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_config_with_helper_records_active_choice() {
        let dir = std::env::temp_dir().join(format!("aag-test-wh-{}", std::process::id()));
        let path = dir.join("nested/config.toml");
        write_config_with_helper(&path, AurHelper::Yay).unwrap();
        let config = Config::load(&path, &cli(&[])).unwrap();
        assert_eq!(config.aur_helper, AurHelper::Yay);
        // Everything else stays at defaults.
        assert_eq!(config.min_age_days, 4);

        // Never overwrites an existing file.
        assert!(write_config_with_helper(&path, AurHelper::Paru).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_prefers_paru_then_yay() {
        assert_eq!(detect_aur_helper_with(|_| false), None);
        assert_eq!(
            detect_aur_helper_with(|n| n == "paru"),
            Some(AurHelper::Paru)
        );
        assert_eq!(detect_aur_helper_with(|n| n == "yay"), Some(AurHelper::Yay));
        assert_eq!(detect_aur_helper_with(|_| true), Some(AurHelper::Paru));
    }

    #[test]
    fn parse_helper_choice_accepts_numbers_names_and_empty() {
        assert_eq!(parse_helper_choice(""), Ok(None));
        assert_eq!(parse_helper_choice("  \n"), Ok(None));
        assert_eq!(parse_helper_choice("1"), Ok(Some(AurHelper::Paru)));
        assert_eq!(parse_helper_choice("paru"), Ok(Some(AurHelper::Paru)));
        assert_eq!(parse_helper_choice("2"), Ok(Some(AurHelper::Yay)));
        assert_eq!(parse_helper_choice("Yay\n"), Ok(Some(AurHelper::Yay)));
        assert_eq!(parse_helper_choice("3"), Ok(Some(AurHelper::None)));
        assert_eq!(parse_helper_choice("NONE"), Ok(Some(AurHelper::None)));
        assert!(parse_helper_choice("4").is_err());
        assert!(parse_helper_choice("pikaur").is_err());
    }

    #[test]
    fn prompt_loops_until_valid_and_honors_default() {
        // Invalid answer, then a valid one.
        let mut input = std::io::Cursor::new("bogus\n2\n".as_bytes());
        let mut out = Vec::new();
        let helper = prompt_aur_helper(&mut input, &mut out, AurHelper::Paru);
        assert_eq!(helper, AurHelper::Yay);

        // Empty input takes the default.
        let mut input = std::io::Cursor::new("\n".as_bytes());
        let helper = prompt_aur_helper(&mut input, &mut Vec::new(), AurHelper::Yay);
        assert_eq!(helper, AurHelper::Yay);

        // EOF takes the default instead of looping forever.
        let mut input = std::io::Cursor::new("".as_bytes());
        let helper = prompt_aur_helper(&mut input, &mut Vec::new(), AurHelper::None);
        assert_eq!(helper, AurHelper::None);
    }
}
