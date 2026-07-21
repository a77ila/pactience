//! Publication timestamp resolution.
//!
//! Resolution order for repository packages (per the design brief):
//! 1. local cache,
//! 2. the Arch Linux Archive (authoritative),
//! 3. the sync database `%BUILDDATE%` (fallback),
//! 4. unknown.
//!
//! Resolution order for AUR packages:
//! 1. local cache,
//! 2. AUR git history (accurate per-version commit dates),
//! 3. AUR `LastModified` (optional heuristic),
//! 4. unknown.

pub mod archive;
pub mod aur;
pub mod aur_git;

use crate::cache::PublicationCache;
use crate::date::format_date;
use crate::db::SyncDb;
use crate::error::Result;
use crate::logging::Logger;
use crate::model::{PackageSource, Publication, PublicationBasis, UpgradeCandidate};

/// A source of publication timestamps for candidate versions.
pub trait PublicationSource {
    fn publication(&self, candidate: &UpgradeCandidate) -> Result<Publication>;
}

/// All sources needed to resolve one candidate, pre-assembled by `main`.
pub struct Sources<'a> {
    pub archive: archive::ArchivePublicationSource<'a>,
    pub aur: aur::AurPublicationSource<'a>,
    /// AUR git-history source; `None` when disabled via configuration.
    pub aur_git: Option<aur_git::AurGitPublicationSource<'a>>,
    pub syncdb: &'a SyncDb,
}

impl Sources<'_> {
    /// AUR resolution-mode tag for cache keys: `g` = git, `h` = heuristic.
    /// `pub(crate)` so `main` can count cache hits before resolving.
    pub(crate) fn aur_mode(&self) -> &'static str {
        match (self.aur_git.is_some(), self.aur.heuristic) {
            (true, true) => "gh",
            (true, false) => "g",
            (false, true) => "h",
            (false, false) => "",
        }
    }
}

/// Resolve the publication info for one candidate, consulting the cache
/// first and storing the result back. Network/parse problems degrade to
/// fallbacks with a warning rather than aborting the run.
pub fn resolve(
    candidate: &UpgradeCandidate,
    sources: &Sources,
    cache: &mut PublicationCache,
    now: i64,
    log: &Logger,
) -> Publication {
    let key = PublicationCache::key(
        candidate.source,
        &candidate.name,
        &candidate.candidate_version,
        sources.aur_mode(),
    );
    if let Some(cached) = cache.get(&key, now) {
        log.debug(format!("{}: cache hit ({key})", candidate.name));
        report(candidate, &cached, log);
        return cached;
    }

    let publication = match candidate.source {
        PackageSource::Repo => resolve_repo(candidate, sources, log),
        PackageSource::Aur => resolve_aur(candidate, sources, log),
    };

    cache.insert(key, &publication, now);
    report(candidate, &publication, log);
    publication
}

/// `-v` line describing how one candidate resolved.
fn report(candidate: &UpgradeCandidate, publication: &Publication, log: &Logger) {
    match publication.published_at {
        Some(ts) => log.info(format!(
            "{} {}: published {} ({})",
            candidate.name,
            candidate.candidate_version,
            format_date(ts),
            publication.basis
        )),
        None => log.info(format!(
            "{} {}: publication time unknown",
            candidate.name, candidate.candidate_version
        )),
    }
}

fn resolve_repo(candidate: &UpgradeCandidate, sources: &Sources, log: &Logger) -> Publication {
    match sources.archive.publication(candidate) {
        Ok(p) if p.published_at.is_some() => return p,
        Ok(_) => log.debug(format!(
            "{}: not found in Arch Archive; trying repo build date",
            candidate.name
        )),
        Err(e) => log.warn(format!(
            "{}: Arch Archive lookup failed ({e}); falling back to repo build date",
            candidate.name
        )),
    }
    // Fallback: %BUILDDATE% of the candidate version in the sync database.
    if let Some(meta) = sources.syncdb.get(&candidate.name)
        && meta.version == candidate.candidate_version
        && let Some(build_date) = meta.build_date
    {
        return Publication::known(build_date, PublicationBasis::RepoBuildDate);
    }
    Publication::unknown()
}

fn resolve_aur(candidate: &UpgradeCandidate, sources: &Sources, log: &Logger) -> Publication {
    // Primary: accurate per-version dates from the AUR git history.
    if let Some(git) = &sources.aur_git {
        match git.publication(candidate) {
            Ok(p) if p.published_at.is_some() => return p,
            Ok(_) => log.debug(format!(
                "{}: version not found in AUR git history; trying weaker AUR sources",
                candidate.name
            )),
            Err(e) => log.warn(format!(
                "{}: AUR git lookup failed ({e}); trying weaker AUR sources",
                candidate.name
            )),
        }
    }
    // Fallback: LastModified heuristic (only if enabled) or unknown.
    match sources.aur.publication(candidate) {
        Ok(p) => p,
        Err(e) => {
            log.warn(format!(
                "{}: publication lookup failed ({e}); treating as unknown",
                candidate.name
            ));
            Publication::unknown()
        }
    }
}
