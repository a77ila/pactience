//! `pactience`: enforce a minimum package age policy before upgrading
//! Arch Linux packages. Dry-run by default; `--apply` performs the safe set.

mod apply;
mod cache;
mod cli;
mod config;
mod date;
mod db;
mod deps;
mod discovery;
mod error;
mod http;
mod logging;
mod model;
mod output;
mod policy;
mod progress;
mod publication;
mod vercmp;

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;
use crate::config::{AurHelper, Config};
use crate::db::{DepSpec, LocalDb, SyncDb};
use crate::error::Result;
use crate::logging::Logger;
use crate::model::{PackageSource, Publication, UpgradeCandidate};

const PACMAN_SYNC_DIR: &str = "/var/lib/pacman/sync";
const PACMAN_LOCAL_DIR: &str = "/var/lib/pacman/local";

fn main() -> ExitCode {
    let cli = Cli::parse();
    let log = Logger::from(&cli);
    match run(&cli, &log) {
        Ok(code) => code,
        Err(e) => {
            log.error(format!("{e}"));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli, log: &Logger) -> Result<ExitCode> {
    // Analysis needs no privileges; only `--apply` elevates (via sudo, or
    // directly when already root). Running the whole tool as root mostly
    // means cache/config state lands in /root instead of the user's home.
    let as_root = apply::effective_uid() == Some(0);
    if as_root {
        log.warn(
            "running as root is discouraged: pactience needs no privileges for analysis \
             and elevates via sudo only when --apply is used",
        );
    }

    // Cache maintenance is a standalone action: clear and exit before any
    // config creation or analysis happens.
    if cli.clear_cache {
        let cache_path = config::default_cache_path();
        let dir = cache_path.parent().unwrap_or(Path::new("."));
        if cache::clear(dir)? {
            println!("cleared cache directory {}", dir.display());
        } else {
            println!("cache directory {} did not exist", dir.display());
        }
        return Ok(ExitCode::SUCCESS);
    }

    let (config_path, explicit_path) = match &cli.config {
        Some(path) => (path.clone(), true),
        None => (config::default_config_path(), false),
    };

    // Persisting a setting is a standalone action: update the config file
    // and exit before any first-run creation or analysis happens.
    if let Some(days) = cli.set_min_age {
        config::set_min_age_days(&config_path, days)?;
        println!("set min_age_days = {days} in {}", config_path.display());
        return Ok(ExitCode::SUCCESS);
    }

    let config_existed = config_path.exists();
    if !config_existed {
        if explicit_path {
            // A missing explicit path is most likely a typo; do not create
            // files at surprising locations.
            log.warn(format!(
                "config file {} not found; using built-in defaults",
                config_path.display()
            ));
        } else {
            let (sources, helper) = first_run_choices(cli, log);
            if let Err(e) = config::write_config_with_choices(&config_path, &sources, helper) {
                log.warn(format!(
                    "cannot create default config {}: {e}; using built-in defaults",
                    config_path.display()
                ));
            } else {
                log.info(format!(
                    "created default configuration at {} (sources = {}, aur_helper = {helper})",
                    config_path.display(),
                    config::format_sources(&sources)
                ));
            }
        }
    }
    // Upgrade path: a config written by an older version has no `sources`
    // key. Ask once (interactive runs only) and record the choice so the
    // question never repeats; non-interactive runs keep the `both` default.
    // An explicit --sources flag suppresses the question for this run.
    if config_existed
        && cli.sources.is_none()
        && !cli.json
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && config::sources_missing_from(&config_path)?
    {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut stderr = std::io::stderr();
        let sources = config::prompt_sources(
            &mut input,
            &mut stderr,
            &[PackageSource::Repo, PackageSource::Aur],
        );
        if let Err(e) = config::record_sources(&config_path, &sources) {
            log.warn(format!(
                "cannot record sources in {}: {e}; using sources = [\"repo\", \"aur\"]",
                config_path.display()
            ));
        } else {
            log.info(format!(
                "recorded sources = {} in {}",
                config::format_sources(&sources),
                config_path.display()
            ));
        }
    }
    let config = Config::load(&config_path, cli)?;
    log.debug(format!(
        "configuration: min_age_days={}, dependency_policy={}, allow_unknown={}, aur_heuristic={}, cache_ttl_secs={}, aur_helper={}, sources={} (from {})",
        config.min_age_days,
        config.dependency_policy,
        config.allow_unknown,
        config.aur_heuristic,
        config.cache_ttl_secs,
        config.aur_helper,
        config::format_sources(&config.sources),
        config_path.display()
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // 1. Discover upgradable packages.
    let runner = discovery::SystemCommandRunner;
    let (candidates, warnings) = discovery::discover(&runner, config.aur_helper, &config.sources)?;
    for warning in &warnings {
        log.warn(warning);
    }
    let repo_count = candidates
        .iter()
        .filter(|c| c.source == PackageSource::Repo)
        .count();
    log.info(format!(
        "discovery: {} repository + {} AUR candidate(s)",
        repo_count,
        candidates.len() - repo_count
    ));
    for candidate in &candidates {
        log.debug(format!(
            "candidate: {} {} -> {} ({})",
            candidate.name,
            candidate.installed_version,
            candidate.candidate_version,
            candidate.source
        ));
    }
    if candidates.is_empty() {
        if cli.json {
            println!("{}", output::render_json(&[], &config, now));
        } else {
            println!("No upgrades available.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // 2. Load pacman databases (best-effort: needed for fallback timestamps
    //    and dependency metadata, but a broken DB must not stop the run).
    let syncdb = SyncDb::load(Path::new(PACMAN_SYNC_DIR)).unwrap_or_else(|e| {
        log.warn(format!(
            "cannot read sync databases: {e}; repo metadata unavailable"
        ));
        SyncDb::default()
    });
    let localdb = LocalDb::load(Path::new(PACMAN_LOCAL_DIR)).unwrap_or_else(|e| {
        log.warn(format!(
            "cannot read local pacman database: {e}; dependency checks disabled"
        ));
        LocalDb::default()
    });
    log.info(format!(
        "databases: {} repo package(s), {} installed package(s)",
        syncdb.packages.len(),
        localdb.installed.len()
    ));

    // 3. Pre-fetch AUR metadata (publication heuristic + dependency info).
    let http = http::UreqClient::new();
    let aur_names: Vec<String> = candidates
        .iter()
        .filter(|c| c.source == PackageSource::Aur)
        .map(|c| c.name.clone())
        .collect();
    let aur_infos = if aur_names.is_empty() {
        HashMap::new()
    } else {
        log.debug(format!(
            "AUR RPC multi-info query for: {}",
            aur_names.join(", ")
        ));
        publication::aur::fetch_infos(&http, publication::aur::DEFAULT_BASE_URL, &aur_names)
            .unwrap_or_else(|e| {
                log.warn(format!(
                    "AUR RPC query failed: {e}; AUR metadata unavailable"
                ));
                HashMap::new()
            })
    };
    if !aur_names.is_empty() {
        log.info(format!(
            "AUR: metadata for {}/{} package(s)",
            aur_infos.len(),
            aur_names.len()
        ));
    }

    // 4. Resolve publication timestamps (cache -> Archive -> repo builddate).
    let cache_path = config::default_cache_path();
    let (mut cache, cache_warning) =
        cache::PublicationCache::load(&cache_path, config.cache_ttl_secs);
    if let Some(warning) = cache_warning {
        log.warn(warning);
    }
    let sources = publication::Sources {
        archive: publication::archive::ArchivePublicationSource::new(&http),
        aur: publication::aur::AurPublicationSource {
            infos: &aur_infos,
            heuristic: config.aur_heuristic,
        },
        aur_git: if config.aur_git {
            let git_dir = cache_path
                .parent()
                .map(|p| p.join("aur-git"))
                .unwrap_or_else(|| Path::new("aur-git").to_path_buf());
            Some(publication::aur_git::AurGitPublicationSource::new(
                &runner, git_dir,
            ))
        } else {
            None
        },
        syncdb: &syncdb,
    };
    // Tell the user up front how much work the resolution pass involves:
    // cache hits are instant, misses may mean network lookups.
    let aur_mode = sources.aur_mode();
    let cached_count = candidates
        .iter()
        .filter(|c| {
            cache
                .get(
                    &cache::PublicationCache::key(
                        c.source,
                        &c.name,
                        &c.candidate_version,
                        aur_mode,
                    ),
                    now,
                )
                .is_some()
        })
        .count();
    log.notice(format!(
        "checking publication dates for {} package(s): {} cached, {} to look up",
        candidates.len(),
        cached_count,
        candidates.len() - cached_count
    ));
    let mut publications: HashMap<String, Publication> = HashMap::new();
    let mut progress = progress::Progress::start(candidates.len(), log);
    for candidate in &candidates {
        let p = publication::resolve(candidate, &sources, &mut cache, now, log);
        publications.insert(candidate.name.clone(), p);
        progress.advance(log, &candidate.name);
    }
    progress.finish(log);
    if let Err(e) = cache.save() {
        log.warn(format!("cannot write cache {}: {e}", cache_path.display()));
    } else {
        log.debug(format!("cache saved to {}", cache_path.display()));
    }

    // 5. Dependency requirement analysis.
    let candidate_deps = collect_candidate_deps(&candidates, &syncdb, &aur_infos);
    let requirements = deps::analyze(&candidates, &candidate_deps, &syncdb, &localdb);
    log.info(format!(
        "dependency analysis: {} requirement(s)",
        requirements.len()
    ));
    for req in &requirements {
        log.debug(format!(
            "requirement: {} needs {} -> {:?}",
            req.dependent, req.dep.name, req.status
        ));
    }

    // 6. Policy evaluation.
    let decisions = policy::evaluate(&candidates, &publications, &requirements, &config, now);
    for decision in &decisions {
        log.debug(format!(
            "verdict: {} -> {} ({})",
            decision.candidate.name,
            decision.verdict,
            decision.reasons.join("; ")
        ));
    }

    // 7. Output. Diagnostics stay on stderr so JSON stdout remains clean.
    let colorize = match cli.color {
        cli::ColorChoice::Always => true,
        cli::ColorChoice::Never => false,
        cli::ColorChoice::Auto => {
            std::io::IsTerminal::is_terminal(&std::io::stdout())
                && std::env::var_os("NO_COLOR").is_none()
        }
    };
    if cli.json {
        println!("{}", output::render_json(&decisions, &config, now));
    } else {
        if !cli.summary_only {
            println!(
                "{}",
                output::render_table(&decisions, now, config.min_age_days, colorize)
            );
        }
        println!("{}", output::render_summary(&decisions));
        if let Some(hint) = output::render_hint(&decisions, &config, &config_path) {
            println!("{hint}");
        }
    }

    // 8. Optional apply. Dry-run is the default and requires explicit opt-in.
    let commands = apply::plan(&decisions, as_root, config.aur_helper)?;
    if cli.apply {
        if commands.is_empty() {
            println!("Nothing to apply: no upgrade passed the policy.");
        } else {
            for command in &commands {
                log.info(format!("running: {command}"));
                println!("running: {command}");
            }
            apply::execute(&commands, &apply::SystemExecutor)?;
        }
    } else if !commands.is_empty() && !cli.json {
        println!("dry-run: re-run with --apply to perform the allowed upgrades");
    }

    Ok(ExitCode::SUCCESS)
}

/// Choose the managed sources and the AUR helper on first run: interactively
/// when stdin is a terminal (and stdout is not reserved for JSON), otherwise
/// both sources plus a PATH probe, with paru as the final fallback. When AUR
/// is excluded the helper is recorded from the probe without asking. The
/// prompt can never fire when `--clear-cache`, `--set-min-age`, or an
/// explicit `--config` path is given — all return before this is called.
fn first_run_choices(cli: &Cli, log: &Logger) -> (Vec<PackageSource>, AurHelper) {
    const BOTH: &[PackageSource] = &[PackageSource::Repo, PackageSource::Aur];
    let detected = config::detect_aur_helper();
    let interactive = !cli.json && std::io::IsTerminal::is_terminal(&std::io::stdin());
    if interactive {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut stderr = std::io::stderr();
        let sources = match cli.sources.clone() {
            Some(sources) => sources,
            None => config::prompt_sources(&mut input, &mut stderr, BOTH),
        };
        let helper = if sources.contains(&PackageSource::Aur) {
            config::prompt_aur_helper(&mut input, &mut stderr, detected.unwrap_or(AurHelper::Paru))
        } else {
            detected.unwrap_or(AurHelper::Paru)
        };
        return (sources, helper);
    }
    let sources = cli.sources.clone().unwrap_or_else(|| BOTH.to_vec());
    let helper = detected.unwrap_or(AurHelper::Paru);
    log.info(format!(
        "selected sources = {}, AUR helper {helper}; change them via the config file",
        config::format_sources(&sources)
    ));
    (sources, helper)
}

/// Gather the dependency declarations of each candidate's *candidate*
/// version: sync DB metadata for repo packages, AUR RPC data for AUR ones.
fn collect_candidate_deps(
    candidates: &[UpgradeCandidate],
    syncdb: &SyncDb,
    aur_infos: &HashMap<String, publication::aur::AurInfo>,
) -> HashMap<String, Vec<DepSpec>> {
    let mut map = HashMap::new();
    for candidate in candidates {
        let deps = match candidate.source {
            PackageSource::Repo => syncdb
                .get(&candidate.name)
                .filter(|meta| meta.version == candidate.candidate_version)
                .map(|meta| meta.depends.clone()),
            PackageSource::Aur => aur_infos
                .get(&candidate.name)
                .filter(|info| info.version == candidate.candidate_version)
                .map(|info| info.depends.clone()),
        };
        if let Some(deps) = deps {
            map.insert(candidate.name.clone(), deps);
        }
    }
    map
}
