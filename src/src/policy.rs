//! Policy engine: combines publication age with dependency safety.
//!
//! Phase 1 assigns each candidate an age-based verdict (allow/block, subject
//! to `always_allow`/`always_block` and the unknown-age rule). Phase 2 then
//! iterates dependency requirements to a fixpoint:
//!
//! - `dependency-respecting` (default): a required younger dependency is
//!   *promoted* into the upgrade set; a dependency that cannot be promoted
//!   (e.g. `always_block`) blocks its dependents instead.
//! - `strict-closure`: no promotions; any candidate whose requirements are
//!   not satisfied by installed packages or age-allowed candidates is
//!   blocked, transitively.
//!
//! Co-pending packages linked by a dependency edge additionally share a
//! verdict: an allowed dependency is never upgraded underneath a held-back
//! dependent (Arch rarely versions deps, so a soname bump is invisible in
//! the metadata). The dependent is promoted, or — when it cannot be
//! promoted, or under `strict-closure` — the dependency is blocked.

use std::collections::HashMap;

use crate::config::{Config, DependencyPolicy};
use crate::deps::{Requirement, RequirementStatus};
use crate::model::{Decision, Publication, UpgradeCandidate, Verdict};

/// Evaluate all candidates into final decisions, in discovery order.
pub fn evaluate(
    candidates: &[UpgradeCandidate],
    publications: &HashMap<String, Publication>,
    requirements: &[Requirement],
    config: &Config,
    now: i64,
) -> Vec<Decision> {
    let mut decisions: Vec<Decision> = candidates
        .iter()
        .map(|c| {
            let publication = publications
                .get(&c.name)
                .cloned()
                .unwrap_or_else(Publication::unknown);
            age_verdict(c, &publication, config, now)
        })
        .collect();

    match config.dependency_policy {
        DependencyPolicy::DependencyRespecting => {
            apply_dependency_respecting(&mut decisions, requirements)
        }
        DependencyPolicy::StrictClosure => apply_strict_closure(&mut decisions, requirements),
    }
    decisions
}

/// The age-based verdict before dependency rules are applied.
fn age_verdict(
    candidate: &UpgradeCandidate,
    publication: &Publication,
    config: &Config,
    now: i64,
) -> Decision {
    let name = &candidate.name;
    let (verdict, reason) = if config.always_block.iter().any(|n| n == name) {
        (
            Verdict::Block,
            "matched always_block in configuration".to_string(),
        )
    } else if config.always_allow.iter().any(|n| n == name) {
        (
            Verdict::Allow,
            "matched always_allow in configuration".to_string(),
        )
    } else {
        match publication.published_at {
            None => {
                if config.allow_unknown {
                    (
                        Verdict::Allow,
                        "publication time unknown; allowed by allow_unknown".to_string(),
                    )
                } else {
                    (
                        Verdict::Block,
                        "publication time unknown; blocked by default (see allow_unknown)"
                            .to_string(),
                    )
                }
            }
            Some(ts) => {
                let age_days = (now - ts).div_euclid(86_400);
                if age_days >= i64::from(config.min_age_days) {
                    (
                        Verdict::Allow,
                        format!(
                            "published {age_days}d ago, meets {}d minimum ({})",
                            config.min_age_days, publication.basis
                        ),
                    )
                } else {
                    (
                        Verdict::Block,
                        format!(
                            "published {age_days}d ago, below {}d minimum ({})",
                            config.min_age_days, publication.basis
                        ),
                    )
                }
            }
        }
    };
    Decision {
        candidate: candidate.clone(),
        publication: publication.clone(),
        verdict,
        reasons: vec![reason],
    }
}

fn is_upgraded(verdict: Verdict) -> bool {
    verdict != Verdict::Block
}

fn format_dep(dep: &crate::db::DepSpec) -> String {
    match &dep.constraint {
        Some((op, version)) => {
            let op = match op {
                crate::db::DepOp::Ge => ">=",
                crate::db::DepOp::Le => "<=",
                crate::db::DepOp::Eq => "=",
                crate::db::DepOp::Gt => ">",
                crate::db::DepOp::Lt => "<",
            };
            format!("{}{}{}", dep.name, op, version)
        }
        None => dep.name.clone(),
    }
}

fn index_of(decisions: &[Decision], name: &str) -> Option<usize> {
    decisions.iter().position(|d| d.candidate.name == name)
}

/// `dependency-respecting`: promote required younger dependencies; block
/// dependents whose requirements cannot be met at all. Co-pending packages
/// linked by a dependency edge share a verdict: a held-back dependent is
/// promoted alongside its allowed dependency, and if it cannot be promoted
/// the dependency is blocked instead.
fn apply_dependency_respecting(decisions: &mut [Decision], requirements: &[Requirement]) {
    // `always_block` packages must never be promoted; track which packages
    // may not be promoted. Promotability is lost whenever a candidate is
    // blocked by a dependency rule (its block is then structural, not
    // age-based), which keeps the fixpoint monotone.
    let mut no_promote: Vec<bool> = decisions
        .iter()
        .map(|d| d.reasons.iter().any(|r| r.contains("always_block")))
        .collect();

    loop {
        let mut changed = false;
        for req in requirements {
            let Some(a) = index_of(decisions, &req.dependent) else {
                continue;
            };
            match &req.status {
                RequirementStatus::SatisfiedByInstalled { .. } => {}
                RequirementStatus::CoupledWithCandidate { name } => {
                    let Some(b) = index_of(decisions, name) else {
                        continue;
                    };
                    if !is_upgraded(decisions[b].verdict) || decisions[a].verdict != Verdict::Block
                    {
                        continue;
                    }
                    if no_promote[a] {
                        // The held-back dependent can never be upgraded, so
                        // the dependency must not move underneath its
                        // installed version.
                        decisions[b].verdict = Verdict::Block;
                        no_promote[b] = true;
                        decisions[b].reasons.push(format!(
                            "blocked: held-back dependent {} cannot be promoted, and upgrading {name} alone could break its installed version",
                            req.dependent
                        ));
                    } else {
                        decisions[a].verdict = Verdict::Promote;
                        decisions[a].reasons.push(format!(
                            "promoted despite age: allowed dependency {name} upgrades underneath the held-back installed version ({})",
                            format_dep(&req.dep)
                        ));
                    }
                    changed = true;
                }
                RequirementStatus::Unsatisfied => {
                    if is_upgraded(decisions[a].verdict) {
                        decisions[a].verdict = Verdict::Block;
                        no_promote[a] = true;
                        decisions[a].reasons.push(format!(
                            "blocked: candidate version requires {}, which no installed package or upgrade candidate provides",
                            format_dep(&req.dep)
                        ));
                        changed = true;
                    }
                }
                RequirementStatus::RequiresCandidate { name } => {
                    if !is_upgraded(decisions[a].verdict) {
                        continue;
                    }
                    let Some(b) = index_of(decisions, name) else {
                        continue;
                    };
                    if decisions[b].verdict == Verdict::Block {
                        if no_promote[b] {
                            decisions[a].verdict = Verdict::Block;
                            no_promote[a] = true;
                            decisions[a].reasons.push(format!(
                                "blocked: requires {}, but {name} cannot be promoted",
                                format_dep(&req.dep)
                            ));
                        } else {
                            decisions[b].verdict = Verdict::Promote;
                            decisions[b].reasons.push(format!(
                                "promoted despite age: required by {} ({})",
                                req.dependent,
                                format_dep(&req.dep)
                            ));
                        }
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return;
        }
    }
}

/// `strict-closure`: never promote; block candidates whose requirements are
/// not met by installed packages or age-allowed candidates, transitively.
/// A dependency is also blocked when a coupled dependent is held back.
fn apply_strict_closure(decisions: &mut [Decision], requirements: &[Requirement]) {
    loop {
        let mut changed = false;
        for req in requirements {
            let Some(a) = index_of(decisions, &req.dependent) else {
                continue;
            };
            // Coupling protects the *installed* dependent: it fires exactly
            // when the dependent is held back, so it must be handled before
            // the upgraded-dependent short-circuit below.
            if let RequirementStatus::CoupledWithCandidate { name } = &req.status {
                let Some(b) = index_of(decisions, name) else {
                    continue;
                };
                if is_upgraded(decisions[b].verdict) && decisions[a].verdict == Verdict::Block {
                    decisions[b].verdict = Verdict::Block;
                    decisions[b].reasons.push(format!(
                        "blocked: held-back dependent {} is not upgraded, and upgrading {name} alone could break its installed version ({})",
                        req.dependent,
                        format_dep(&req.dep)
                    ));
                    changed = true;
                }
                continue;
            }
            if !is_upgraded(decisions[a].verdict) {
                continue;
            }
            let block_reason = match &req.status {
                RequirementStatus::SatisfiedByInstalled { .. }
                | RequirementStatus::CoupledWithCandidate { .. } => None,
                RequirementStatus::Unsatisfied => Some(format!(
                    "blocked: candidate version requires {}, which no installed package or allowed candidate provides",
                    format_dep(&req.dep)
                )),
                RequirementStatus::RequiresCandidate { name } => {
                    let provider_allowed = index_of(decisions, name)
                        .map(|b| decisions[b].verdict == Verdict::Allow)
                        .unwrap_or(false);
                    if provider_allowed {
                        None
                    } else {
                        Some(format!(
                            "blocked: requires {}, but {name} is not allowed by the age policy",
                            format_dep(&req.dep)
                        ))
                    }
                }
            };
            if let Some(reason) = block_reason {
                decisions[a].verdict = Verdict::Block;
                decisions[a].reasons.push(reason);
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DepSpec;
    use crate::model::{PackageSource, PublicationBasis};

    const DAY: i64 = 86_400;
    const NOW: i64 = 100 * DAY;

    fn candidate(name: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: "1.0-1".to_string(),
            candidate_version: "2.0-1".to_string(),
            source: PackageSource::Repo,
        }
    }

    fn published_days_ago(days: i64) -> Publication {
        Publication::known(NOW - days * DAY, PublicationBasis::Archive)
    }

    fn config() -> Config {
        Config::default() // 4 days, dependency-respecting
    }

    fn pubs(entries: &[(&str, Publication)]) -> HashMap<String, Publication> {
        entries
            .iter()
            .map(|(n, p)| (n.to_string(), p.clone()))
            .collect()
    }

    fn verdict_of(decisions: &[Decision], name: &str) -> Verdict {
        decisions
            .iter()
            .find(|d| d.candidate.name == name)
            .unwrap()
            .verdict
    }

    #[test]
    fn old_enough_is_allowed() {
        let d = evaluate(
            &[candidate("foo")],
            &pubs(&[("foo", published_days_ago(10))]),
            &[],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "foo"), Verdict::Allow);
        assert!(d[0].reasons[0].contains("10d ago"));
    }

    #[test]
    fn too_young_is_blocked() {
        let d = evaluate(
            &[candidate("foo")],
            &pubs(&[("foo", published_days_ago(2))]),
            &[],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "foo"), Verdict::Block);
    }

    #[test]
    fn unknown_age_blocked_by_default_allowed_with_opt_in() {
        let d = evaluate(
            &[candidate("foo")],
            &pubs(&[("foo", Publication::unknown())]),
            &[],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "foo"), Verdict::Block);

        let mut cfg = config();
        cfg.allow_unknown = true;
        let d = evaluate(
            &[candidate("foo")],
            &pubs(&[("foo", Publication::unknown())]),
            &[],
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "foo"), Verdict::Allow);
    }

    #[test]
    fn allow_and_block_lists_override_age() {
        let mut cfg = config();
        cfg.always_allow = vec!["young".to_string()];
        cfg.always_block = vec!["old".to_string()];
        let d = evaluate(
            &[candidate("young"), candidate("old")],
            &pubs(&[
                ("young", published_days_ago(0)),
                ("old", published_days_ago(30)),
            ]),
            &[],
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "young"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "old"), Verdict::Block);
    }

    #[test]
    fn dependency_respecting_promotes_young_dependency() {
        let reqs = vec![Requirement {
            dependent: "app".to_string(),
            dep: DepSpec::parse("lib>=2.0").unwrap(),
            status: RequirementStatus::RequiresCandidate {
                name: "lib".to_string(),
            },
        }];
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(1)),
            ]),
            &reqs,
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Promote);
    }

    #[test]
    fn promotion_chains_transitively() {
        // app -> lib -> base; only app is old enough.
        let reqs = vec![
            Requirement {
                dependent: "app".to_string(),
                dep: DepSpec::parse("lib>=2.0").unwrap(),
                status: RequirementStatus::RequiresCandidate {
                    name: "lib".to_string(),
                },
            },
            Requirement {
                dependent: "lib".to_string(),
                dep: DepSpec::parse("base>=2.0").unwrap(),
                status: RequirementStatus::RequiresCandidate {
                    name: "base".to_string(),
                },
            },
        ];
        let d = evaluate(
            &[candidate("app"), candidate("lib"), candidate("base")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(1)),
                ("base", published_days_ago(1)),
            ]),
            &reqs,
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Promote);
        assert_eq!(verdict_of(&d, "base"), Verdict::Promote);
    }

    #[test]
    fn always_block_dependency_blocks_dependent() {
        let mut cfg = config();
        cfg.always_block = vec!["lib".to_string()];
        let reqs = vec![Requirement {
            dependent: "app".to_string(),
            dep: DepSpec::parse("lib>=2.0").unwrap(),
            status: RequirementStatus::RequiresCandidate {
                name: "lib".to_string(),
            },
        }];
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(10)),
            ]),
            &reqs,
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
        assert!(
            d.iter()
                .find(|x| x.candidate.name == "app")
                .unwrap()
                .reasons
                .iter()
                .any(|r| r.contains("cannot be promoted"))
        );
    }

    #[test]
    fn unsatisfied_requirement_blocks_dependent() {
        let reqs = vec![Requirement {
            dependent: "app".to_string(),
            dep: DepSpec::parse("ghost>=9.0").unwrap(),
            status: RequirementStatus::Unsatisfied,
        }];
        let d = evaluate(
            &[candidate("app")],
            &pubs(&[("app", published_days_ago(10))]),
            &reqs,
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
    }

    #[test]
    fn strict_closure_blocks_instead_of_promoting() {
        let mut cfg = config();
        cfg.dependency_policy = DependencyPolicy::StrictClosure;
        let reqs = vec![
            Requirement {
                dependent: "app".to_string(),
                dep: DepSpec::parse("lib>=2.0").unwrap(),
                status: RequirementStatus::RequiresCandidate {
                    name: "lib".to_string(),
                },
            },
            Requirement {
                dependent: "gui".to_string(),
                dep: DepSpec::parse("app>=2.0").unwrap(),
                status: RequirementStatus::RequiresCandidate {
                    name: "app".to_string(),
                },
            },
        ];
        let d = evaluate(
            &[candidate("app"), candidate("lib"), candidate("gui")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(1)),
                ("gui", published_days_ago(10)),
            ]),
            &reqs,
            &cfg,
            NOW,
        );
        // lib too young -> app blocked -> gui blocked transitively.
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
        assert_eq!(verdict_of(&d, "gui"), Verdict::Block);
    }

    #[test]
    fn strict_closure_allows_when_provider_age_allowed() {
        let mut cfg = config();
        cfg.dependency_policy = DependencyPolicy::StrictClosure;
        let reqs = vec![Requirement {
            dependent: "app".to_string(),
            dep: DepSpec::parse("lib>=2.0").unwrap(),
            status: RequirementStatus::RequiresCandidate {
                name: "lib".to_string(),
            },
        }];
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(10)),
            ]),
            &reqs,
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Allow);
    }

    fn coupled(dependent: &str, dep: &str) -> Requirement {
        Requirement {
            dependent: dependent.to_string(),
            dep: DepSpec::parse(dep).unwrap(),
            status: RequirementStatus::CoupledWithCandidate {
                name: dep.to_string(),
            },
        }
    }

    #[test]
    fn coupled_dependent_is_promoted_with_allowed_dependency() {
        // lib is old enough, app is too young: upgrading lib alone could
        // break the installed app, so app is promoted alongside it.
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(1)),
                ("lib", published_days_ago(10)),
            ]),
            &[coupled("app", "lib")],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "lib"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "app"), Verdict::Promote);
    }

    #[test]
    fn coupling_does_not_promote_the_dependency() {
        // The reverse direction is safe by time ordering (the allowed
        // candidate predates the blocked dependency), so nothing changes.
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(1)),
            ]),
            &[coupled("app", "lib")],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
    }

    #[test]
    fn coupling_promotes_transitively() {
        // lib allowed -> promote app -> app now upgrades underneath
        // held-back gui -> promote gui.
        let d = evaluate(
            &[candidate("gui"), candidate("app"), candidate("lib")],
            &pubs(&[
                ("gui", published_days_ago(1)),
                ("app", published_days_ago(1)),
                ("lib", published_days_ago(10)),
            ]),
            &[coupled("app", "lib"), coupled("gui", "app")],
            &config(),
            NOW,
        );
        assert_eq!(verdict_of(&d, "lib"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "app"), Verdict::Promote);
        assert_eq!(verdict_of(&d, "gui"), Verdict::Promote);
    }

    #[test]
    fn unpromotable_dependent_blocks_the_dependency() {
        // app is always_block-listed: it can never be promoted, so the
        // allowed dependency must not move underneath its installed version.
        let mut cfg = config();
        cfg.always_block = vec!["app".to_string()];
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(1)),
                ("lib", published_days_ago(10)),
            ]),
            &[coupled("app", "lib")],
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
        assert!(
            d.iter()
                .find(|x| x.candidate.name == "lib")
                .unwrap()
                .reasons
                .iter()
                .any(|r| r.contains("held-back dependent"))
        );
    }

    #[test]
    fn coupling_blocked_dependency_is_not_repromoted() {
        // app requires lib>=2.0 (promotes lib), but held-back gui couples
        // lib: lib cannot move, so app must be held back too.
        let reqs = vec![
            Requirement {
                dependent: "app".to_string(),
                dep: DepSpec::parse("lib>=2.0").unwrap(),
                status: RequirementStatus::RequiresCandidate {
                    name: "lib".to_string(),
                },
            },
            coupled("gui", "lib"),
        ];
        let mut cfg = config();
        cfg.always_block = vec!["gui".to_string()];
        let d = evaluate(
            &[candidate("app"), candidate("lib"), candidate("gui")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(1)),
                ("gui", published_days_ago(1)),
            ]),
            &reqs,
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "gui"), Verdict::Block);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
    }

    #[test]
    fn strict_closure_blocks_dependency_under_held_back_dependent() {
        let mut cfg = config();
        cfg.dependency_policy = DependencyPolicy::StrictClosure;
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(1)),
                ("lib", published_days_ago(10)),
            ]),
            &[coupled("app", "lib")],
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Block);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Block);
    }

    #[test]
    fn strict_closure_leaves_coupled_allowed_pair_alone() {
        let mut cfg = config();
        cfg.dependency_policy = DependencyPolicy::StrictClosure;
        let d = evaluate(
            &[candidate("app"), candidate("lib")],
            &pubs(&[
                ("app", published_days_ago(10)),
                ("lib", published_days_ago(10)),
            ]),
            &[coupled("app", "lib")],
            &cfg,
            NOW,
        );
        assert_eq!(verdict_of(&d, "app"), Verdict::Allow);
        assert_eq!(verdict_of(&d, "lib"), Verdict::Allow);
    }
}
