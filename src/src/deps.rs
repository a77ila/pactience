//! Dependency requirement analysis.
//!
//! For every upgrade candidate we look at the dependency declarations of its
//! *candidate* version (sync DB for repo packages, AUR RPC metadata for AUR
//! packages) and classify each one: already satisfied by the installed
//! system, satisfiable only by another package in the upgrade set, coupled to
//! a co-pending dependency (satisfied by the installed version, but the
//! dependency is itself pending — verdicts must agree), satisfied only by an
//! installed provider whose pending upgrade drops the capability (a soname
//! bump — the provider must not move), or unsatisfiable.
//! The policy engine turns these into promote/block verdicts.
//!
//! [`find_installed_breaks`] adds the reverse direction: pacman also refuses
//! a transaction when an *installed* package that stays behind loses a
//! capability its installed version requires (a provider's candidate drops a
//! soname). Those edges couple the verdicts of pending dependents with their
//! provider, or block the provider outright when the dependent is not
//! pending and can never join the transaction.

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
    /// The installed version satisfies the dep, but the dep itself is also a
    /// pending upgrade. Upgrading the dependency while holding this dependent
    /// back can break the *installed* dependent (unversioned soname coupling,
    /// the classic partial-upgrade hazard), so their verdicts must agree:
    /// promote the dependent or block the dependency.
    CoupledWithCandidate { name: String },
    /// The installed version of a pending provider satisfies the dep, but the
    /// provider's *candidate* version drops the capability (a soname bump,
    /// e.g. `libavcodec.so=62-64` -> `=63-64`). Since the dep is declared by
    /// the dependent's candidate version, upgrading the provider breaks the
    /// dependent whether it is held back (installed version) or upgraded
    /// along (candidate version): the provider must not be upgraded.
    BreaksInstalled { name: String },
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

/// `installed_version` marker used for synthetic candidates that were never
/// installed (pulled in as brand-new dependencies).
pub const NEW_PACKAGE_INSTALLED: &str = "-";

/// Collect brand-new repo dependencies that could satisfy currently
/// `Unsatisfied` requirements, as synthetic upgrade candidates.
///
/// A requirement qualifies when its dependency (a) is not already a
/// candidate, (b) exists in the sync DB under its own name, and (c) the sync
/// DB version satisfies the (possibly versioned) constraint. Results are
/// deduplicated by name. Only direct name matches are resolved — virtual
/// capabilities provided solely by not-installed packages stay unsatisfied,
/// as do AUR-only packages.
///
/// The returned candidates use [`NEW_PACKAGE_INSTALLED`] as their installed
/// version; once appended to the candidate list and re-analyzed, the
/// corresponding requirements classify as `RequiresCandidate` and flow
/// through the normal policy engine (age gate, promotion, blocking).
pub fn find_installable_new_deps(
    requirements: &[Requirement],
    syncdb: &SyncDb,
    candidates: &[UpgradeCandidate],
) -> Vec<UpgradeCandidate> {
    let known: std::collections::HashSet<&str> =
        candidates.iter().map(|c| c.name.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut new_candidates = Vec::new();
    for req in requirements {
        if req.status != RequirementStatus::Unsatisfied {
            continue;
        }
        let name = req.dep.name.as_str();
        if known.contains(name) || !seen.insert(name.to_string()) {
            continue;
        }
        let Some(meta) = syncdb.get(name) else {
            continue;
        };
        if !req.dep.satisfied_by(&meta.version) {
            continue;
        }
        new_candidates.push(UpgradeCandidate {
            name: meta.name.clone(),
            installed_version: NEW_PACKAGE_INSTALLED.to_string(),
            candidate_version: meta.version.clone(),
            source: crate::model::PackageSource::Repo,
        });
    }
    new_candidates
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
        // The installed version satisfies the constraint, but the dependency
        // is itself pending: upgrading it underneath a held-back dependent is
        // exactly the partial-upgrade hazard (Arch rarely versions deps, so a
        // soname bump shows up as a plain name edge).
        return RequirementStatus::CoupledWithCandidate {
            name: provider.name.clone(),
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
    //    A provider that is itself a pending upgrade only counts when its
    //    *candidate* version still satisfies the dependency: a soname bump
    //    drops the old provide, and upgrading the provider underneath this
    //    dependency would break the dependent (installed or candidate).
    let mut dropped_by: Option<String> = None;
    for provider in localdb.providers_of(&dep.name) {
        if !provide_satisfies(dep, localdb.provided_version(provider, &dep.name).flatten()) {
            continue;
        }
        if candidates.contains_key(provider.as_str())
            && !candidate_satisfies(dep, candidates[provider.as_str()], syncdb)
        {
            if dropped_by.is_none() {
                dropped_by = Some(provider.clone());
            }
            continue;
        }
        return RequirementStatus::SatisfiedByInstalled {
            version: localdb.version_of(provider).unwrap_or("?").to_string(),
        };
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

    // 5. Only a pending provider whose candidate drops the capability can
    //    satisfy the dep today: upgrading it breaks the dependent either way.
    if let Some(name) = dropped_by {
        return RequirementStatus::BreaksInstalled { name };
    }

    RequirementStatus::Unsatisfied
}

/// Does a candidate's *candidate* version satisfy `dep`? Compares the dep
/// against the candidate's own name + version or its provides from the sync
/// DB. When the provides are unknown (e.g. AUR candidates), assume the
/// capability survives the upgrade rather than blocking the provider on a
/// guess.
fn candidate_satisfies(dep: &DepSpec, candidate: &UpgradeCandidate, syncdb: &SyncDb) -> bool {
    if dep.name == candidate.name {
        return dep.satisfied_by(&candidate.candidate_version);
    }
    let Some(meta) = syncdb.get(&candidate.name) else {
        return true;
    };
    meta.provides
        .iter()
        .any(|p| p.name == dep.name && provide_satisfies(dep, p.version.as_deref()))
}

/// Does the installed version of package `name` satisfy `dep`, either by its
/// own name + version or through one of its provides?
fn installed_satisfies(dep: &DepSpec, name: &str, localdb: &LocalDb) -> bool {
    if dep.name == name {
        return localdb
            .version_of(name)
            .map(|v| dep.satisfied_by(v))
            .unwrap_or(false);
    }
    localdb
        .provided_version(name, &dep.name)
        .map(|provided| provide_satisfies(dep, provided))
        .unwrap_or(false)
}

/// Is `dep` satisfied by some installed package other than `excluding` that
/// survives the upgrade (not pending, or its candidate still satisfies it)?
fn has_surviving_provider(
    dep: &DepSpec,
    excluding: &str,
    candidates: &HashMap<&str, &UpgradeCandidate>,
    syncdb: &SyncDb,
    localdb: &LocalDb,
) -> bool {
    let keeps = |name: &str| match candidates.get(name) {
        None => true,
        Some(c) => candidate_satisfies(dep, c, syncdb),
    };
    if dep.name != excluding && installed_satisfies(dep, &dep.name, localdb) && keeps(&dep.name) {
        return true;
    }
    localdb.providers_of(&dep.name).iter().any(|provider| {
        provider != excluding && installed_satisfies(dep, provider, localdb) && keeps(provider)
    })
}

/// Reverse dependency safety: pacman refuses a transaction when an installed
/// package that stays behind loses a dependency. For every installed package
/// we check its *installed* dependency declarations against every pending
/// upgrade providing them today: if the provider's candidate version drops
/// the capability and no other surviving installed package offers it, the
/// provider cannot move on its own.
///
/// When the dependent is itself a candidate the edge couples their verdicts
/// (the dependent's candidate comes from the same consistent repo as the
/// provider's candidate, so upgrading both works: promote the dependent, or
/// block the provider when it cannot be promoted). When the dependent is
/// not pending — a foreign/AUR package or simply not in the upgrade set —
/// it can never join the transaction, so the provider is blocked outright
/// (`BreaksInstalled`).
///
/// Candidate-derived coupling or `BreaksInstalled` edges for the same
/// dependent/provider pair (from [`analyze`]) make the reverse edge
/// redundant and suppress it; a `RequiresCandidate` edge does not — it is
/// inert while the dependent is held back, which is exactly the case the
/// reverse edge covers.
pub fn find_installed_breaks(
    candidates: &[UpgradeCandidate],
    existing: &[Requirement],
    syncdb: &SyncDb,
    localdb: &LocalDb,
) -> Vec<Requirement> {
    let candidate_by_name: HashMap<&str, &UpgradeCandidate> =
        candidates.iter().map(|c| (c.name.as_str(), c)).collect();

    // capability -> candidates whose *installed* version provides it
    // (a package always provides its own name)
    let mut installed_caps: HashMap<&str, Vec<&str>> = HashMap::new();
    for candidate in candidates {
        if candidate.installed_version == NEW_PACKAGE_INSTALLED {
            continue;
        }
        installed_caps
            .entry(candidate.name.as_str())
            .or_default()
            .push(candidate.name.as_str());
        if let Some(pkg) = localdb.installed.get(&candidate.name) {
            for provide in &pkg.provides {
                installed_caps
                    .entry(provide.name.as_str())
                    .or_default()
                    .push(candidate.name.as_str());
            }
        }
    }

    let mut seen: std::collections::HashSet<(&str, &str)> = existing
        .iter()
        .filter_map(|r| match &r.status {
            RequirementStatus::CoupledWithCandidate { name }
            | RequirementStatus::BreaksInstalled { name } => {
                Some((r.dependent.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect();

    let mut requirements = Vec::new();
    for (rname, rpkg) in &localdb.installed {
        for dep in &rpkg.depends {
            let Some(providers) = installed_caps.get(dep.name.as_str()) else {
                continue;
            };
            for pname in providers {
                if pname == rname {
                    continue;
                }
                let candidate = candidate_by_name[pname];
                // The provider's installed version must satisfy the dep
                // today — otherwise its upgrade changes nothing for the
                // dependent.
                if !installed_satisfies(dep, pname, localdb) {
                    continue;
                }
                if candidate_satisfies(dep, candidate, syncdb) {
                    continue;
                }
                if has_surviving_provider(dep, pname, &candidate_by_name, syncdb, localdb) {
                    continue;
                }
                if !seen.insert((rname.as_str(), pname)) {
                    continue;
                }
                let status = if candidate_by_name.contains_key(rname.as_str()) {
                    RequirementStatus::CoupledWithCandidate {
                        name: pname.to_string(),
                    }
                } else {
                    RequirementStatus::BreaksInstalled {
                        name: pname.to_string(),
                    }
                };
                requirements.push(Requirement {
                    dependent: rname.clone(),
                    dep: dep.clone(),
                    status,
                });
            }
        }
    }
    requirements
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
                    depends: vec![],
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

    fn syncdb_with(pkgs: &[(&str, &str)]) -> SyncDb {
        let mut db = SyncDb::default();
        for (name, version) in pkgs {
            db.packages.insert(
                name.to_string(),
                RepoPackageMeta {
                    name: name.to_string(),
                    version: version.to_string(),
                    ..Default::default()
                },
            );
        }
        db
    }

    #[test]
    fn unsatisfied_dep_in_syncdb_is_installable() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("newlib")])]);
        let localdb = localdb_with(&[("foo", "1.0-1")]);
        let syncdb = syncdb_with(&[("newlib", "3.1-2")]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert_eq!(reqs[0].status, RequirementStatus::Unsatisfied);
        let additions = find_installable_new_deps(&reqs, &syncdb, &candidates);
        assert_eq!(
            additions,
            vec![UpgradeCandidate {
                name: "newlib".to_string(),
                installed_version: NEW_PACKAGE_INSTALLED.to_string(),
                candidate_version: "3.1-2".to_string(),
                source: PackageSource::Repo,
            }]
        );
    }

    #[test]
    fn versioned_constraint_is_checked_against_syncdb() {
        let candidates = vec![candidate("foo", "1.0-1", "2.0-1")];
        let deps = HashMap::from([("foo".to_string(), vec![dep("newlib>=4.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1")]);
        let syncdb = syncdb_with(&[("newlib", "3.1-2")]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        // The repo version is too old for the constraint: not installable.
        assert!(find_installable_new_deps(&reqs, &syncdb, &candidates).is_empty());

        let deps = HashMap::from([("foo".to_string(), vec![dep("newlib>=3.0")])]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert_eq!(
            find_installable_new_deps(&reqs, &syncdb, &candidates).len(),
            1
        );
    }

    #[test]
    fn installable_new_deps_dedupe_and_skip_known() {
        let unsatisfied = |dependent: &str, name: &str| Requirement {
            dependent: dependent.to_string(),
            dep: DepSpec::parse(name).unwrap(),
            status: RequirementStatus::Unsatisfied,
        };
        let syncdb = syncdb_with(&[("newlib", "3.1-2"), ("other", "1.0-1")]);

        // Two dependents needing the same new dep: one synthetic candidate.
        let reqs = vec![unsatisfied("foo", "newlib"), unsatisfied("bar", "newlib")];
        assert_eq!(find_installable_new_deps(&reqs, &syncdb, &[]).len(), 1);

        // Already a candidate: never duplicated.
        let candidates = vec![candidate("newlib", "1.0-1", "3.1-2")];
        let reqs = vec![unsatisfied("foo", "newlib")];
        assert!(find_installable_new_deps(&reqs, &syncdb, &candidates).is_empty());

        // Non-unsatisfied requirements are ignored.
        let reqs = vec![Requirement {
            dependent: "foo".to_string(),
            dep: DepSpec::parse("newlib").unwrap(),
            status: RequirementStatus::SatisfiedByInstalled {
                version: "3.1-2".to_string(),
            },
        }];
        assert!(find_installable_new_deps(&reqs, &syncdb, &[]).is_empty());

        // Not in the sync DB (e.g. AUR-only): stays unsatisfied.
        let reqs = vec![unsatisfied("foo", "aur-only-lib")];
        assert!(find_installable_new_deps(&reqs, &syncdb, &[]).is_empty());
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
                depends: vec![],
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
    fn pending_dependency_satisfied_by_installed_is_coupled() {
        // Unversioned edge between two co-pending packages: verdicts must
        // agree even though the installed version satisfies the dep.
        let candidates = vec![
            candidate("foo", "1.0-1", "2.0-1"),
            candidate("bar", "1.0-1", "2.0-1"),
        ];
        let deps = HashMap::from([("foo".to_string(), vec![dep("bar")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.0-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::CoupledWithCandidate {
                name: "bar".to_string()
            }
        );
    }

    #[test]
    fn versioned_dep_satisfied_by_installed_is_still_coupled() {
        let candidates = vec![
            candidate("foo", "1.0-1", "2.0-1"),
            candidate("bar", "1.5-1", "2.0-1"),
        ];
        let deps = HashMap::from([("foo".to_string(), vec![dep("bar>=1.0")])]);
        let localdb = localdb_with(&[("foo", "1.0-1"), ("bar", "1.5-1")]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::CoupledWithCandidate {
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

    /// Local DB with one provider of a versioned capability (soname style).
    fn localdb_with_provider(
        dependent: &str,
        provider: &str,
        provide: &str,
    ) -> (Vec<UpgradeCandidate>, LocalDb) {
        let candidates = vec![
            candidate(dependent, "1.0-1", "2.0-1"),
            candidate(provider, "1.0-1", "2.0-1"),
        ];
        let mut localdb = localdb_with(&[(dependent, "1.0-1"), (provider, "1.0-1")]);
        localdb.insert(
            provider.to_string(),
            InstalledPackage {
                version: "1.0-1".to_string(),
                provides: vec![Provide::parse(provide).unwrap()],
                depends: vec![],
            },
        );
        (candidates, localdb)
    }

    fn syncdb_with_provides(name: &str, version: &str, provides: &[&str]) -> SyncDb {
        let mut db = SyncDb::default();
        db.packages.insert(
            name.to_string(),
            RepoPackageMeta {
                name: name.to_string(),
                version: version.to_string(),
                provides: provides
                    .iter()
                    .map(|p| Provide::parse(p).unwrap())
                    .collect(),
                ..Default::default()
            },
        );
        db
    }

    #[test]
    fn pending_provider_dropping_provide_breaks_installed() {
        // The dependent's candidate still needs libavcodec.so=62-64; the
        // provider's candidate ships =63-64 (soname bump). Upgrading the
        // provider breaks the dependent either way.
        let (candidates, localdb) =
            localdb_with_provider("recorder", "avlib", "libavcodec.so=62-64");
        let syncdb = syncdb_with_provides("avlib", "2.0-1", &["libavcodec.so=63-64"]);
        let deps = HashMap::from([("recorder".to_string(), vec![dep("libavcodec.so=62-64")])]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::BreaksInstalled {
                name: "avlib".to_string()
            }
        );
    }

    #[test]
    fn pending_provider_keeping_provide_is_satisfied() {
        let (candidates, localdb) =
            localdb_with_provider("recorder", "avlib", "libavcodec.so=62-64");
        let syncdb = syncdb_with_provides("avlib", "2.0-1", &["libavcodec.so=62-64"]);
        let deps = HashMap::from([("recorder".to_string(), vec![dep("libavcodec.so=62-64")])]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert!(matches!(
            reqs[0].status,
            RequirementStatus::SatisfiedByInstalled { .. }
        ));
    }

    #[test]
    fn pending_provider_with_unknown_candidate_provides_is_satisfied() {
        // No sync DB metadata for the provider (e.g. AUR): assume the
        // capability survives rather than blocking on a guess.
        let (candidates, localdb) =
            localdb_with_provider("recorder", "avlib", "libavcodec.so=62-64");
        let deps = HashMap::from([("recorder".to_string(), vec![dep("libavcodec.so=62-64")])]);
        let reqs = analyze(&candidates, &deps, &SyncDb::default(), &localdb);
        assert!(matches!(
            reqs[0].status,
            RequirementStatus::SatisfiedByInstalled { .. }
        ));
    }

    #[test]
    fn other_candidate_provider_wins_over_dropped_provide() {
        // A second candidate (a compat package) provides the old soname:
        // the dependency routes to it instead of breaking.
        let (mut candidates, localdb) =
            localdb_with_provider("recorder", "avlib", "libavcodec.so=62-64");
        candidates.push(candidate("avlib-compat", "1.0-1", "1.0-2"));
        let mut syncdb = syncdb_with_provides("avlib", "2.0-1", &["libavcodec.so=63-64"]);
        syncdb.packages.insert(
            "avlib-compat".to_string(),
            RepoPackageMeta {
                name: "avlib-compat".to_string(),
                version: "1.0-2".to_string(),
                provides: vec![Provide::parse("libavcodec.so=62-64").unwrap()],
                ..Default::default()
            },
        );
        let deps = HashMap::from([("recorder".to_string(), vec![dep("libavcodec.so=62-64")])]);
        let reqs = analyze(&candidates, &deps, &syncdb, &localdb);
        assert_eq!(
            reqs[0].status,
            RequirementStatus::RequiresCandidate {
                name: "avlib-compat".to_string()
            }
        );
    }

    #[test]
    fn non_pending_provider_dropping_nothing_is_unaffected() {
        // The provider is not pending: step 3 behaves exactly as before.
        let (candidates, localdb) =
            localdb_with_provider("recorder", "avlib", "libavcodec.so=62-64");
        let candidates = &candidates[..1]; // only the dependent is pending
        let deps = HashMap::from([("recorder".to_string(), vec![dep("libavcodec.so=62-64")])]);
        let reqs = analyze(candidates, &deps, &SyncDb::default(), &localdb);
        assert!(matches!(
            reqs[0].status,
            RequirementStatus::SatisfiedByInstalled { .. }
        ));
    }

    #[test]
    fn depspec_ops_end_to_end() {
        // Guards against DepOp wiring regressions in satisfied_by.
        assert!(dep("x<=2.0").satisfied_by("1.9"));
        assert!(!dep("x<=2.0").satisfied_by("2.1"));
        assert!(dep("x>2.0").satisfied_by("2.1"));
        assert!(!dep("x>2.0").satisfied_by("2.0"));
    }

    /// Installed provider `avlib` (pending 1.0-1 -> 2.0-1, drops the soname
    /// in its candidate) plus an installed `recorder` depending on the old
    /// soname. `recorder_pending` controls whether recorder is a candidate.
    fn reverse_break_setup(recorder_pending: bool) -> (Vec<UpgradeCandidate>, SyncDb, LocalDb) {
        let mut candidates = vec![candidate("avlib", "1.0-1", "2.0-1")];
        if recorder_pending {
            candidates.push(candidate("recorder", "0.6-1", "0.7-1"));
        }
        let mut localdb = LocalDb::default();
        localdb.insert(
            "avlib".to_string(),
            InstalledPackage {
                version: "1.0-1".to_string(),
                provides: vec![Provide::parse("libavcodec.so=62-64").unwrap()],
                depends: vec![],
            },
        );
        localdb.insert(
            "recorder".to_string(),
            InstalledPackage {
                version: "0.6-1".to_string(),
                provides: vec![],
                depends: vec![dep("libavcodec.so=62-64")],
            },
        );
        let syncdb = syncdb_with_provides("avlib", "2.0-1", &["libavcodec.so=63-64"]);
        (candidates, syncdb, localdb)
    }

    #[test]
    fn reverse_break_with_non_pending_dependent_blocks_provider() {
        // A foreign/AUR package can never join the transaction, so the
        // provider dropping its soname is blocked outright.
        let (candidates, syncdb, localdb) = reverse_break_setup(false);
        let reqs = find_installed_breaks(&candidates, &[], &syncdb, &localdb);
        assert_eq!(
            reqs,
            vec![Requirement {
                dependent: "recorder".to_string(),
                dep: dep("libavcodec.so=62-64"),
                status: RequirementStatus::BreaksInstalled {
                    name: "avlib".to_string()
                },
            }]
        );
    }

    #[test]
    fn reverse_break_with_pending_dependent_couples_verdicts() {
        // The dependent is pending: its candidate comes from the same
        // consistent repo as the provider's candidate, so upgrading both
        // works — the edge couples their verdicts.
        let (candidates, syncdb, localdb) = reverse_break_setup(true);
        let reqs = find_installed_breaks(&candidates, &[], &syncdb, &localdb);
        assert_eq!(
            reqs,
            vec![Requirement {
                dependent: "recorder".to_string(),
                dep: dep("libavcodec.so=62-64"),
                status: RequirementStatus::CoupledWithCandidate {
                    name: "avlib".to_string()
                },
            }]
        );
    }

    #[test]
    fn reverse_break_skipped_when_candidate_keeps_capability() {
        let (candidates, _, localdb) = reverse_break_setup(false);
        let syncdb = syncdb_with_provides("avlib", "2.0-1", &["libavcodec.so=62-64"]);
        assert!(find_installed_breaks(&candidates, &[], &syncdb, &localdb).is_empty());
    }

    #[test]
    fn reverse_break_skipped_for_provider_with_unknown_provides() {
        // No sync DB metadata (e.g. AUR provider): assume the capability
        // survives rather than blocking on a guess.
        let (candidates, _, localdb) = reverse_break_setup(false);
        assert!(find_installed_breaks(&candidates, &[], &SyncDb::default(), &localdb).is_empty());
    }

    #[test]
    fn reverse_break_skipped_when_other_installed_provider_survives() {
        let (candidates, syncdb, mut localdb) = reverse_break_setup(false);
        localdb.insert(
            "avlib-old".to_string(),
            InstalledPackage {
                version: "1.0-1".to_string(),
                provides: vec![Provide::parse("libavcodec.so=62-64").unwrap()],
                depends: vec![],
            },
        );
        assert!(find_installed_breaks(&candidates, &[], &syncdb, &localdb).is_empty());
    }

    #[test]
    fn reverse_break_dedupes_existing_coupling_but_not_requires_candidate() {
        let (candidates, syncdb, localdb) = reverse_break_setup(true);
        let coupled = Requirement {
            dependent: "recorder".to_string(),
            dep: dep("avlib"),
            status: RequirementStatus::CoupledWithCandidate {
                name: "avlib".to_string(),
            },
        };
        assert!(find_installed_breaks(&candidates, &[coupled], &syncdb, &localdb).is_empty());

        // A RequiresCandidate edge is inert while the dependent is held
        // back — exactly the case the reverse edge covers — so it must not
        // suppress the coupling edge.
        let requires = Requirement {
            dependent: "recorder".to_string(),
            dep: dep("libavcodec.so=63-64"),
            status: RequirementStatus::RequiresCandidate {
                name: "avlib".to_string(),
            },
        };
        assert_eq!(
            find_installed_breaks(&candidates, &[requires], &syncdb, &localdb).len(),
            1
        );
    }
}
