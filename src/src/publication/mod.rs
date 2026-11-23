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

use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// Failed Arch Archive requests so far this run, shared across the
    /// parallel resolution workers. Once `ARCHIVE_FAILURE_CUTOFF` is
    /// reached the archive is skipped for the rest of the run.
    pub archive_failures: &'a AtomicUsize,
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
/// fallbacks with a warning rather than aborting the run. Sequential
/// callers (e.g. newly discovered dependencies) use this; the main
/// candidate pass parallelizes via `lookup` and does cache get/insert
/// itself.
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

    let publication = lookup(candidate, sources, log);
    cache.insert(key, &publication, now);
    publication
}

/// Determine a candidate's publication from the live sources, without
/// consulting or updating the cache. Touches only shared state, so it is
/// safe to call from multiple resolution workers concurrently.
pub fn lookup(candidate: &UpgradeCandidate, sources: &Sources, log: &Logger) -> Publication {
    let publication = match candidate.source {
        PackageSource::Repo => resolve_repo(candidate, sources, log),
        PackageSource::Aur => resolve_aur(candidate, sources, log),
    };
    report(candidate, &publication, log);
    publication
}

/// `-v` line describing how one candidate resolved.
pub(crate) fn report(candidate: &UpgradeCandidate, publication: &Publication, log: &Logger) {
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

/// Number of failed Arch Archive requests after which the rest of the run
/// stops trying. When the archive is unreachable, every remaining lookup
/// would only burn the full HTTP timeout before falling back anyway; a few
/// isolated failures stay below the cutoff and keep the archive in use.
const ARCHIVE_FAILURE_CUTOFF: usize = 5;

fn resolve_repo(candidate: &UpgradeCandidate, sources: &Sources, log: &Logger) -> Publication {
    let failures = sources.archive_failures.load(Ordering::Relaxed);
    if failures >= ARCHIVE_FAILURE_CUTOFF {
        log.debug(format!(
            "{}: skipping Arch Archive lookup ({failures} failed attempts); using repo build date",
            candidate.name
        ));
    } else {
        match sources.archive.publication(candidate) {
            Ok(p) if p.published_at.is_some() => return p,
            Ok(_) => log.debug(format!(
                "{}: not found in Arch Archive; trying repo build date",
                candidate.name
            )),
            Err(e) => {
                let failures = sources.archive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                if failures >= ARCHIVE_FAILURE_CUTOFF {
                    log.warn(format!(
                        "Arch Archive lookup failed repeatedly ({e}); falling back to \
                         repo build dates for the remaining packages"
                    ));
                } else {
                    log.warn(format!(
                        "{}: Arch Archive lookup failed ({e}); falling back to repo build date",
                        candidate.name
                    ));
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RepoPackageMeta;
    use crate::error::Error;
    use crate::http::{FetchOutcome, HttpClient};
    use std::collections::HashMap;

    /// Fails every request, counting them so tests can assert how many
    /// archive lookups were actually attempted.
    struct FailingHttp {
        requests: AtomicUsize,
    }

    impl HttpClient for FailingHttp {
        fn get(&self, url: &str) -> Result<FetchOutcome> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            Err(Error::http(url, "boom"))
        }
    }

    fn repo_candidate(name: &str, version: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: "1.0-1".to_string(),
            candidate_version: version.to_string(),
            source: PackageSource::Repo,
        }
    }

    fn sources_with<'a>(
        http: &'a dyn HttpClient,
        syncdb: &'a SyncDb,
        failures: &'a AtomicUsize,
        infos: &'a HashMap<String, aur::AurInfo>,
    ) -> Sources<'a> {
        Sources {
            archive: archive::ArchivePublicationSource::with_base_url(http, "https://example.test"),
            aur: aur::AurPublicationSource {
                infos,
                heuristic: false,
            },
            aur_git: None,
            syncdb,
            archive_failures: failures,
        }
    }

    #[test]
    fn archive_is_skipped_after_repeated_failures() {
        let http = FailingHttp {
            requests: AtomicUsize::new(0),
        };
        let mut packages = HashMap::new();
        packages.insert(
            "foo".to_string(),
            RepoPackageMeta {
                name: "foo".to_string(),
                version: "2.0-1".to_string(),
                build_date: Some(12345),
                depends: Vec::new(),
                provides: Vec::new(),
            },
        );
        let syncdb = SyncDb { packages };
        let failures = AtomicUsize::new(0);
        let infos = HashMap::new();
        let sources = sources_with(&http, &syncdb, &failures, &infos);
        let log = Logger::new(crate::logging::Level::Error);

        // More candidates than the failure cutoff: once the archive has
        // failed enough times, the remaining lookups must not issue any
        // further requests and fall back to the repo build date.
        for _ in 0..ARCHIVE_FAILURE_CUTOFF + 2 {
            let p = lookup(&repo_candidate("foo", "2.0-1"), &sources, &log);
            assert_eq!(p.published_at, Some(12345));
            assert_eq!(p.basis, PublicationBasis::RepoBuildDate);
        }
        assert_eq!(
            http.requests.load(Ordering::Relaxed),
            ARCHIVE_FAILURE_CUTOFF
        );
    }

    #[test]
    fn isolated_archive_failures_do_not_trip_the_cutoff() {
        let http = FailingHttp {
            requests: AtomicUsize::new(0),
        };
        let syncdb = SyncDb::default();
        let failures = AtomicUsize::new(0);
        let infos = HashMap::new();
        let sources = sources_with(&http, &syncdb, &failures, &infos);
        let log = Logger::new(crate::logging::Level::Error);

        // Fewer failures than the cutoff: every lookup still tries the
        // archive (and the counter does not reset between successes).
        for _ in 0..ARCHIVE_FAILURE_CUTOFF - 1 {
            let _ = lookup(&repo_candidate("foo", "2.0-1"), &sources, &log);
        }
        assert_eq!(
            http.requests.load(Ordering::Relaxed),
            ARCHIVE_FAILURE_CUTOFF - 1
        );
    }
}
