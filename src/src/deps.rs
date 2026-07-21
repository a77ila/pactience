//! Dependency requirement analysis.
//!
//! For every upgrade candidate we look at the dependency declarations of its
//! *candidate* version (sync DB for repo packages, AUR RPC metadata for AUR
//! packages) and classify each one: already satisfied by the installed
//! system, satisfiable only by another package in the upgrade set, or
//! unsatisfiable. The policy engine turns these into promote/block verdicts.

use std::collections::HashMap;

use crate::db::{DepSpec, LocalDb, SyncDb};
use crate::model::UpgradeCandidate;

/// How a single dependency declaration can be fulfilled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementStatus {
    /// An installed package (or one of its provides) already satisfies it.
    SatisfiedByInstalled { version: String },
    /// Only another package in the upgrade set satisfies it: the candidate
    /// must be upgraded together with (or instead of) the installed version.
    RequiresCandidate { name: String },
    /// Nothing installed or in the upgrade set satisfies it.
    Unsatisfied,
}

/// One dependency edge: `dependent` (an upgrade candidate) needs `dep`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub dependent: String,
    pub dep: DepSpec,
    pub status: RequirementStatus,
}

/// Classify every dependency of every candidate.
///
/// `candidate_deps` maps candidate name -> dependency declarations of its
/// candidate version. Candidates missing from the map simply contribute no
/// requirements.
pub fn analyze(
    candidates: &[UpgradeCandidate],
    candidate_deps: &HashMap<String, Vec<DepSpec>>,
    syncdb: &SyncDb,
    localdb: &LocalDb,
) -> Vec<Requirement> {
    let candidate_names: HashMap<&str, &UpgradeCandidate> =
        candidates.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut requirements = Vec::new();
    for candidate in candidates {
        let Some(deps) = candidate_deps.get(&candidate.name) else {
            continue;
        };
        for dep in deps {
            // A package never depends on itself for upgrade purposes.
            if dep.name == candidate.name {
                continue;
            }
            let status = classify(dep, &candidate_names, syncdb, localdb);
            requirements.push(Requirement {
                dependent: candidate.name.clone(),
                dep: dep.clone(),
                status,
            });
        }
    }
    requirements
}

fn classify(
    dep: &DepSpec,
    candidates: &HashMap<&str, &UpgradeCandidate>,
    syncdb: &SyncDb,
    localdb: &LocalDb,
) -> RequirementStatus {
    // 1. Direct name match in the upgrade set: the dependency will be
    //    satisfied by upgrading that package (its candidate version is by
    //    definition newer than any constraint the installed version failed).
    if let Some(provider) = candidates.get(dep.name.as_str()) {
        // Only route through the candidate if the installed version does not
        // already satisfy the constraint.
        let installed_ok = localdb
            .version_of(&dep.name)
            .map(|v| dep.satisfied_by(v))
            .unwrap_or(false);
        if !installed_ok {
            return RequirementStatus::RequiresCandidate {
                name: provider.name.clone(),
            };
        }
        return RequirementStatus::SatisfiedByInstalled {
            version: provider.installed_version.clone(),
        };
    }

    // 2. Installed package with the same name.
    if let Some(version) = localdb.version_of(&dep.name) {
        if dep.satisfied_by(version) {
            return RequirementStatus::SatisfiedByInstalled {
                version: version.to_string(),
            };
        }
        // Installed but too old, and not in the upgrade set: pacman would
        // pull it in as part of the transaction, but doing so selectively is
        // exactly the partial-upgrade hazard this tool exists to prevent.
        return RequirementStatus::Unsatisfied;
    }

    // 3. Virtual capability provided by an installed package (`sh` by bash).
    for provider in localdb.providers_of(&dep.name) {
        if provide_satisfies(dep, localdb.provided_version(provider, &dep.name).flatten()) {
            return RequirementStatus::SatisfiedByInstalled {
                version: localdb.version_of(provider).unwrap_or("?").to_string(),
            };
        }
    }

    // 4. Virtual capability provided by another candidate's *candidate*
    //    version (metadata from the sync DB).
    for provider in candidates.values() {
        let Some(meta) = syncdb.get(&provider.name) else {
            continue;
        };
        for provide in &meta.provides {
            if provide.name == dep.name && provide_satisfies(dep, provide.version.as_deref()) {
                return RequirementStatus::RequiresCandidate {
                    name: provider.name.clone(),
                };
            }
        }
    }

    RequirementStatus::Unsatisfied
}

/// Does a `provides` entry satisfy a (possibly versioned) dependency?
/// Unversioned provides only satisfy unversioned deps; versioned provides
/// are compared with the dep's operator.
fn provide_satisfies(dep: &DepSpec, provided_version: Option<&str>) -> bool {
    match (&dep.constraint, provided_version) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((op, required)), Some(provided)) => {
            let probe = DepSpec {
                name: dep.name.clone(),
                constraint: Some((*op, required.clone())),
            };
            probe.satisfied_by(provided)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{InstalledPackage, Provide, RepoPackageMeta};
    use crate::model::PackageSource;

    fn candidate(name: &str, installed: &str, candidate_version: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: installed.to_string(),
            candidate_version: candidate_version.to_string(),
            source: PackageSource::Repo,
        }
    }

    fn dep(raw: &str) -> DepSpec {
        DepSpec::parse(raw).unwrap()
    }

    fn localdb_with(pkgs: &[(&str, &str)]) -> LocalDb {
        let mut db = LocalDb::default();
        for (name, version) in pkgs {
            db.insert(
                name.to_string(),
                InstalledPackage {
                    version: version.to_string(),
                    provides: vec![],
                },
            );
        }
        db
    }

    #[test]
    fn satisfied_by_installed_version() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("bar>=1.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.5-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::SatisfiedByInstalled {
                version: "1.5-1".to_string()
            }
        );
    }

    #[test]
    fn newer_dependency_requires_candidate() {
        let candidates = vec![
            candidate("foo", "1.0-1", "2.0-1"),
            candidate("bar", "1.0-1", "2.0-1"),
        ];
        let deps = HashMap::from([("foo".to_string(), vec![dep("bar>=2.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.0-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::RequiresCandidate {
                name: "bar".to_string()
            }
        );
    }

    #[test]
    fn old_installed_and_not_in_set_is_unsatisfied() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("bar>=2.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.0-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(reqs[0].status, RequirementStatus::Unsatisfied);
    }

    #[test]
    fn new_uninstalled_dependency_is_unsatisfied() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("newlib")])]);
        let localdb = localdb_with(&[("foo", "1.0-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(reqs[0].status, RequirementStatus::Unsatisfied);
    }

    #[test]
    fn virtual_capability_satisfied_by_installed_provider() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("sh")])]);
        let mut localdb = localdb_with(&[("foo", "1.0-1")]);
        localdb.insert(
            "bash".to_string(),
            InstalledPackage {
                version: "5.2-1".to_string(),
                provides: vec![Provide {
                    name: "sh".to_string(),
                    version: None,
                }],
            },
        );
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert!(matches!(
            reqs[0].status,
            RequirementStatus::SatisfiedByInstalled { .. }
        ));
    }

    #[test]
    fn virtual_capability_can_require_candidate_provider() {
        let candidates = vec![
            candidate("foo", "1.0-1", "2.0-1"),
            candidate("bar", "1.0-1", "2.0-1"),
        ];
        let deps = HashMap::from([("foo".to_string(), vec![dep("virtualthing")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.0-1")]);
        let mut syncdb = SyncDb::default();
        syncdb.packages.insert(
            "bar".to_string(),
            RepoPackageMeta {
                name: "bar".to_string(),
                version: "2.0-1".to_string(),
                provides: vec![Provide {
                    name: "virtualthing".to_string(),
                    version: None,
                }],
                ..Default::default()
            },
        );
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::RequiresCandidate {
                name: "bar".to_string()
            }
        );
    }

    #[test]
    fn self_dependencies_are_ignored() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("foo>=2.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert!(reqs.is_empty());
    }

    #[test]
    fn versioned_provide_is_compared() {
        assert!(provide_satisfies(&dep("lib=1.0"), Some("1.0")));
        assert!(!provide_satisfies(&dep("lib=1.0"), Some("2.0")));
        assert!(provide_satisfies(&dep("lib"), None));
        assert!(!provide_satisfies(&dep("lib>=1.0"), None));
    }

    #[test]
    fn depspec_ops_end_to_end() {
        // Guards against DepOp wiring regressions in satisfied_by.
        assert!(dep("x<=2.0").satisfied_by("1.9"));
        assert!(!dep("x<=2.0").satisfied_by("2.1"));
        assert!(dep("x>2.0").satisfied_by("2.1"));
        assert!(!dep("x>2.0").satisfied_by("2.0"));
    }
}
