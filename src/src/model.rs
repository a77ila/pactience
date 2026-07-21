//! Shared domain types used across discovery, publication, policy and output.

use serde::{Deserialize, Serialize};

/// Where an upgrade candidate originates from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    /// Official Arch repositories (discovered via `pacman -Qu`).
    Repo,
    /// Arch User Repository (discovered via the configured AUR helper's `-Qua`).
    Aur,
}

impl std::fmt::Display for PackageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageSource::Repo => write!(f, "repo"),
            PackageSource::Aur => write!(f, "aur"),
        }
    }
}

/// A single package that could be upgraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeCandidate {
    pub name: String,
    pub installed_version: String,
    pub candidate_version: String,
    pub source: PackageSource,
}

/// How a publication timestamp was determined. This is surfaced in the output
/// so users can audit why a decision was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationBasis {
    /// First appearance in the Arch Linux Archive package pool.
    Archive,
    /// `%BUILDDATE%` from the repository sync database (fallback).
    RepoBuildDate,
    /// AUR `LastModified` field. Heuristic only: the AUR RPC exposes no
    /// per-version publication timestamp.
    AurLastModified,
    /// Commit date of the commit that introduced the candidate version in the
    /// AUR package's git repository. Accurate per-version timestamp.
    AurGit,
    /// No timestamp could be determined.
    Unknown,
}

impl std::fmt::Display for PublicationBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublicationBasis::Archive => write!(f, "archive"),
            PublicationBasis::RepoBuildDate => write!(f, "repo-builddate"),
            PublicationBasis::AurLastModified => write!(f, "aur-lastmodified (heuristic)"),
            PublicationBasis::AurGit => write!(f, "aur-git"),
            PublicationBasis::Unknown => write!(f, "unknown"),
        }
    }
}

/// Publication information for one candidate version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    /// Unix timestamp of first publication, if known.
    pub published_at: Option<i64>,
    pub basis: PublicationBasis,
}

impl Publication {
    pub fn unknown() -> Self {
        Publication {
            published_at: None,
            basis: PublicationBasis::Unknown,
        }
    }

    pub fn known(published_at: i64, basis: PublicationBasis) -> Self {
        Publication {
            published_at: Some(published_at),
            basis,
        }
    }
}

/// The final verdict for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Old enough (or explicitly allowed): safe to upgrade.
    Allow,
    /// Too young, unknown age, explicitly blocked, or blocked for dependency
    /// safety.
    Block,
    /// Younger than the threshold but required as a dependency of an allowed
    /// upgrade (`dependency-respecting` policy only).
    Promote,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Allow => write!(f, "allow"),
            Verdict::Block => write!(f, "block"),
            Verdict::Promote => write!(f, "promote"),
        }
    }
}

/// A candidate together with its publication info and the policy verdict.
#[derive(Debug, Clone)]
pub struct Decision {
    pub candidate: UpgradeCandidate,
    pub publication: Publication,
    pub verdict: Verdict,
    /// Human-readable audit trail explaining the verdict.
    pub reasons: Vec<String>,
}

impl Decision {
    /// Age of the candidate version in whole days, if publication is known.
    pub fn age_days(&self, now: i64) -> Option<i64> {
        self.publication
            .published_at
            .map(|ts| (now - ts).div_euclid(86_400))
    }
}
