//! Local cache for publication timestamps.
//!
//! A version's publication date is an immutable historical fact, so positive
//! results never expire. Only negative results ("unknown") are refreshed
//! after `cache_ttl_secs`, because a version may appear in the Archive later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{PackageSource, Publication, PublicationBasis};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    published_at: Option<i64>,
    basis: PublicationBasis,
    fetched_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    entries: HashMap<String, CacheEntry>,
}

/// Keyed by `{source}:{name}@{version}`.
pub struct PublicationCache {
    path: PathBuf,
    ttl_secs: u64,
    entries: HashMap<String, CacheEntry>,
}

impl PublicationCache {
    /// Load the cache file. Returns the cache plus an optional warning: a
    /// corrupt cache is not fatal, it is simply rebuilt from scratch.
    pub fn load(path: &Path, ttl_secs: u64) -> (PublicationCache, Option<String>) {
        let mut cache = PublicationCache {
            path: path.to_path_buf(),
            ttl_secs,
            entries: HashMap::new(),
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<CacheFile>(&contents) {
                Ok(file) => cache.entries = file.entries,
                Err(e) => {
                    return (
                        cache,
                        Some(format!("ignoring corrupt cache {}: {e}", path.display())),
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return (
                    cache,
                    Some(format!("cannot read cache {}: {e}", path.display())),
                );
            }
        }
        (cache, None)
    }

    /// Cache identity of one lookup. AUR results depend on the resolution
    /// mode (git history and/or LastModified heuristic give different answers
    /// for the same name@version), so the mode tag is part of the key and no
    /// mode can shadow another's entries.
    pub fn key(source: PackageSource, name: &str, version: &str, aur_mode: &str) -> String {
        match source {
            PackageSource::Repo => format!("repo:{name}@{version}"),
            PackageSource::Aur => format!("aur{aur_mode}:{name}@{version}"),
        }
    }

    /// Look up a cached publication. Positive entries never expire; negative
    /// ones expire after the configured TTL.
    pub fn get(&self, key: &str, now: i64) -> Option<Publication> {
        let entry = self.entries.get(key)?;
        match entry.published_at {
            Some(ts) => Some(Publication::known(ts, entry.basis)),
            None => {
                let age = now.saturating_sub(entry.fetched_at);
                if age >= 0 && (age as u64) < self.ttl_secs {
                    Some(Publication::unknown())
                } else {
                    None
                }
            }
        }
    }

    pub fn insert(&mut self, key: String, publication: &Publication, now: i64) {
        self.entries.insert(
            key,
            CacheEntry {
                published_at: publication.published_at,
                basis: publication.basis,
                fetched_at: now,
            },
        );
    }

    /// Persist the cache, creating parent directories as needed. Writes via a
    /// temporary file + rename so a crash cannot leave a half-written cache.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.to_path_buf(), e))?;
        }
        let file = CacheFile {
            entries: self.entries.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| Error::Cache(format!("serialization failed: {e}")))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| Error::io(tmp.clone(), e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| Error::io(self.path.clone(), e))?;
        Ok(())
    }
}

/// Remove the entire cache directory (publication cache, AUR git clones).
/// Returns `true` when something was actually removed; a missing directory
/// is not an error.
pub fn clear(dir: &Path) -> Result<bool> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(dir.to_path_buf(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with(entries: &[(&str, Option<i64>, i64)], ttl: u64) -> PublicationCache {
        let mut map = HashMap::new();
        for (key, published_at, fetched_at) in entries {
            map.insert(
                key.to_string(),
                CacheEntry {
                    published_at: *published_at,
                    basis: PublicationBasis::Archive,
                    fetched_at: *fetched_at,
                },
            );
        }
        PublicationCache {
            path: PathBuf::from("/nonexistent/cache.json"),
            ttl_secs: ttl,
            entries: map,
        }
    }

    #[test]
    fn positive_entries_never_expire() {
        let cache = cache_with(&[("repo:foo@1.0-1", Some(1000), 2000)], 60);
        let pub_ = cache.get("repo:foo@1.0-1", 1_000_000).unwrap();
        assert_eq!(pub_.published_at, Some(1000));
        assert_eq!(pub_.basis, PublicationBasis::Archive);
    }

    #[test]
    fn negative_entries_expire_after_ttl() {
        let cache = cache_with(&[("repo:foo@1.0-1", None, 1000)], 60);
        assert!(cache.get("repo:foo@1.0-1", 1050).is_some());
        assert!(cache.get("repo:foo@1.0-1", 1061).is_none());
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let dir = std::env::temp_dir().join(format!("aag-cache-{}", std::process::id()));
        let path = dir.join("publications.json");
        let mut cache = PublicationCache {
            path: path.clone(),
            ttl_secs: 60,
            entries: HashMap::new(),
        };
        cache.insert(
            PublicationCache::key(PackageSource::Repo, "foo", "1.0-1", ""),
            &Publication::known(12345, PublicationBasis::RepoBuildDate),
            999,
        );
        cache.save().unwrap();

        let (loaded, warning) = PublicationCache::load(&path, 60);
        assert!(warning.is_none());
        let pub_ = loaded
            .get(
                &PublicationCache::key(PackageSource::Repo, "foo", "1.0-1", ""),
                1000,
            )
            .unwrap();
        assert_eq!(pub_.published_at, Some(12345));
        assert_eq!(pub_.basis, PublicationBasis::RepoBuildDate);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aur_resolution_mode_changes_cache_key() {
        for (a, b) in [("", "g"), ("g", "gh"), ("", "h"), ("h", "gh")] {
            assert_ne!(
                PublicationCache::key(PackageSource::Aur, "p", "1-1", a),
                PublicationCache::key(PackageSource::Aur, "p", "1-1", b)
            );
        }
        // Repo keys are policy-independent.
        assert_eq!(
            PublicationCache::key(PackageSource::Repo, "p", "1-1", "g"),
            PublicationCache::key(PackageSource::Repo, "p", "1-1", "")
        );
    }

    #[test]
    fn corrupt_cache_is_a_warning_not_an_error() {
        let dir = std::env::temp_dir().join(format!("aag-cache-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("publications.json");
        std::fs::write(&path, "{not json").unwrap();
        let (cache, warning) = PublicationCache::load(&path, 60);
        assert!(warning.is_some());
        assert!(cache.entries.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_removes_directory_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("aag-cache-clear-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("aur-git")).unwrap();
        std::fs::write(dir.join("publications.json"), "{}").unwrap();
        assert!(clear(&dir).unwrap());
        assert!(!dir.exists());
        // A second clear on a missing directory succeeds and reports false.
        assert!(!clear(&dir).unwrap());
    }
}
