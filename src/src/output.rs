//! Human-readable table and machine-readable JSON rendering of decisions.

use serde::Serialize;

use crate::config::Config;
use crate::date::format_date;
use crate::model::{Decision, PackageSource, Verdict};
use std::path::Path;

/// One decision as serialized to JSON.
#[derive(Debug, Serialize)]
struct JsonDecision {
    name: String,
    source: String,
    installed_version: String,
    candidate_version: String,
    published_at: Option<i64>,
    publication_basis: String,
    age_days: Option<i64>,
    verdict: Verdict,
    reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JsonSummary {
    total: usize,
    allowed: usize,
    promoted: usize,
    blocked: usize,
}

#[derive(Debug, Serialize)]
struct JsonReport {
    generated_at: i64,
    min_age_days: u32,
    dependency_policy: String,
    sources: Vec<String>,
    summary: JsonSummary,
    decisions: Vec<JsonDecision>,
}

fn counts(decisions: &[Decision]) -> (usize, usize, usize) {
    let mut allowed = 0;
    let mut promoted = 0;
    let mut blocked = 0;
    for d in decisions {
        match d.verdict {
            Verdict::Allow => allowed += 1,
            Verdict::Promote => promoted += 1,
            Verdict::Block => blocked += 1,
        }
    }
    (allowed, promoted, blocked)
}

/// Render the full report as a JSON string.
pub fn render_json(decisions: &[Decision], config: &Config, now: i64) -> String {
    let (allowed, promoted, blocked) = counts(decisions);
    let report = JsonReport {
        generated_at: now,
        min_age_days: config.min_age_days,
        dependency_policy: config.dependency_policy.to_string(),
        sources: config.sources.iter().map(|s| s.to_string()).collect(),
        summary: JsonSummary {
            total: decisions.len(),
            allowed,
            promoted,
            blocked,
        },
        decisions: decisions
            .iter()
            .map(|d| JsonDecision {
                name: d.candidate.name.clone(),
                source: d.candidate.source.to_string(),
                installed_version: d.candidate.installed_version.clone(),
                candidate_version: d.candidate.candidate_version.clone(),
                published_at: d.publication.published_at,
                publication_basis: d.publication.basis.to_string(),
                age_days: d.age_days(now),
                verdict: d.verdict,
                reasons: d.reasons.clone(),
            })
            .collect(),
    };
    // Serialization of this closed schema cannot fail.
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Wrap `text` in an ANSI color when `color` is on. Applied *after* padding
/// so escape sequences never disturb column widths.
fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Column indices that receive color.
const AGE_COL: usize = 5;
const VERDICT_COL: usize = 6;

/// Render the summary table for interactive use. The AGE and VERDICT columns
/// are colored relative to `min_age_days` when `color` is enabled: green for
/// upgrades that pass the age gate, red for blocked ones, yellow for
/// promoted dependencies.
pub fn render_table(decisions: &[Decision], now: i64, min_age_days: u32, color: bool) -> String {
    let headers = [
        "PACKAGE",
        "SRC",
        "INSTALLED",
        "CANDIDATE",
        "PUBLISHED",
        "AGE",
        "VERDICT",
        "REASON",
    ];
    // (cells, age color, verdict color) per decision.
    let rows: Vec<([String; 8], &'static str, &'static str)> = decisions
        .iter()
        .map(|d| {
            let published = d
                .publication
                .published_at
                .map(format_date)
                .unwrap_or_else(|| "unknown".to_string());
            let (age_text, age_color) = match d.age_days(now) {
                Some(a) if a >= i64::from(min_age_days) => (format!("{a}d"), GREEN),
                Some(a) => (format!("{a}d"), RED),
                None => ("-".to_string(), ""),
            };
            let verdict_color = match d.verdict {
                Verdict::Allow => GREEN,
                Verdict::Promote => YELLOW,
                Verdict::Block => RED,
            };
            (
                [
                    d.candidate.name.clone(),
                    d.candidate.source.to_string(),
                    d.candidate.installed_version.clone(),
                    d.candidate.candidate_version.clone(),
                    published,
                    age_text,
                    d.verdict.to_string(),
                    d.reasons.join("; "),
                ],
                age_color,
                verdict_color,
            )
        })
        .collect();

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for (row, _, _) in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    let render_row =
        |cells: &[String; 8], age_color: &str, verdict_color: &str, out: &mut String| {
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    out.push_str("  ");
                }
                // Do not pad the last column; reasons vary wildly in length.
                let padded = if i == cells.len() - 1 {
                    cell.clone()
                } else {
                    format!("{cell:<width$}", width = widths[i])
                };
                let code = match i {
                    AGE_COL => age_color,
                    VERDICT_COL => verdict_color,
                    _ => "",
                };
                out.push_str(&paint(&padded, code, color && !code.is_empty()));
            }
            out.push('\n');
        };

    let mut out = String::new();
    let header_cells: [String; 8] = headers.map(String::from);
    render_row(&header_cells, "", "", &mut out);
    let sep: [String; 8] = std::array::from_fn(|i| "-".repeat(widths[i]));
    render_row(&sep, "", "", &mut out);
    for (row, age_color, verdict_color) in &rows {
        render_row(row, age_color, verdict_color, &mut out);
    }
    out
}

/// One-line summary appended below the table.
pub fn render_summary(decisions: &[Decision]) -> String {
    let (allowed, promoted, blocked) = counts(decisions);
    let mut parts = vec![format!("{allowed} allowed")];
    if promoted > 0 {
        parts.push(format!("{promoted} promoted"));
    }
    parts.push(format!("{blocked} blocked"));
    format!("{} upgrade(s): {}", decisions.len(), parts.join(", "))
}

/// Actionable hint shown after the report whenever anything is blocked.
/// Points at the whitelist (`always_allow`) and, when blocked AUR packages
/// have unknown age and the heuristic is off, at `aur_heuristic`.
/// Returns `None` when nothing is blocked.
pub fn render_hint(decisions: &[Decision], config: &Config, config_path: &Path) -> Option<String> {
    let blocked = decisions
        .iter()
        .filter(|d| d.verdict == Verdict::Block)
        .count();
    if blocked == 0 {
        return None;
    }
    let mut lines = vec![format!(
        "hint: {blocked} package(s) blocked; whitelist trusted packages via always_allow in {}",
        config_path.display()
    )];
    let aur_unknown_blocked = decisions.iter().any(|d| {
        d.verdict == Verdict::Block
            && d.candidate.source == PackageSource::Aur
            && d.publication.published_at.is_none()
    });
    if aur_unknown_blocked && !config.aur_heuristic {
        lines.push(
            "hint: AUR packages have no official publication date; aur_heuristic = true \
             (or --aur-heuristic) gates them by the LastModified field"
                .to_string(),
        );
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PackageSource, Publication, PublicationBasis, UpgradeCandidate};

    const NOW: i64 = 100 * 86_400;

    fn sample_decisions() -> Vec<Decision> {
        vec![
            Decision {
                candidate: UpgradeCandidate {
                    name: "linux".to_string(),
                    installed_version: "6.8.1-1".to_string(),
                    candidate_version: "6.8.2-1".to_string(),
                    source: PackageSource::Repo,
                },
                publication: Publication::known(NOW - 10 * 86_400, PublicationBasis::Archive),
                verdict: Verdict::Allow,
                reasons: vec!["published 10d ago, meets 4d minimum (archive)".to_string()],
            },
            Decision {
                candidate: UpgradeCandidate {
                    name: "paru-bin".to_string(),
                    installed_version: "2.0.3-1".to_string(),
                    candidate_version: "2.0.4-1".to_string(),
                    source: PackageSource::Aur,
                },
                publication: Publication::unknown(),
                verdict: Verdict::Block,
                reasons: vec!["publication time unknown".to_string()],
            },
        ]
    }

    #[test]
    fn table_contains_all_columns() {
        let table = render_table(&sample_decisions(), NOW, 4, false);
        assert!(table.contains("PACKAGE"));
        assert!(table.contains("VERDICT"));
        assert!(table.contains("linux"));
        assert!(table.contains("6.8.1-1"));
        assert!(table.contains("6.8.2-1"));
        assert!(table.contains("allow"));
        assert!(table.contains("block"));
        assert!(table.contains("10d"));
        assert!(table.contains("unknown"));
        assert!(!table.contains('\x1b'));
    }

    #[test]
    fn colors_mark_verdict_and_age() {
        let table = render_table(&sample_decisions(), NOW, 4, true);
        // Allowed with 10d >= 4d: green verdict, green age. (Cells are padded
        // before painting, so the RESET follows the padding.)
        assert!(table.contains("\x1b[32mallow"));
        assert!(table.contains("\x1b[32m10d"));
        // Blocked: red verdict; unknown age stays uncolored.
        assert!(table.contains("\x1b[31mblock"));
        // Column alignment survives: escape codes never count toward width.
        let plain = render_table(&sample_decisions(), NOW, 4, false);
        let colored_lines: Vec<&str> = table.lines().collect();
        let plain_lines: Vec<&str> = plain.lines().collect();
        assert_eq!(colored_lines.len(), plain_lines.len());
    }

    #[test]
    fn age_color_respects_min_age() {
        // Same 10d-old package: green under a 4d gate, red under a 30d gate.
        assert!(render_table(&sample_decisions(), NOW, 4, true).contains("\x1b[32m10d"));
        assert!(render_table(&sample_decisions(), NOW, 30, true).contains("\x1b[31m10d"));
    }

    #[test]
    fn json_is_machine_readable() {
        let json = render_json(&sample_decisions(), &Config::default(), NOW);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["summary"]["total"], 2);
        assert_eq!(parsed["summary"]["allowed"], 1);
        assert_eq!(parsed["summary"]["blocked"], 1);
        assert_eq!(parsed["decisions"][0]["name"], "linux");
        assert_eq!(parsed["decisions"][0]["age_days"], 10);
        assert_eq!(parsed["decisions"][0]["verdict"], "allow");
        assert!(parsed["decisions"][1]["published_at"].is_null());
    }

    #[test]
    fn summary_counts() {
        assert_eq!(
            render_summary(&sample_decisions()),
            "2 upgrade(s): 1 allowed, 1 blocked"
        );
    }

    #[test]
    fn hint_points_at_whitelist_and_aur_heuristic() {
        let hint = render_hint(
            &sample_decisions(),
            &Config::default(),
            Path::new("/home/u/.config/pactience/config.toml"),
        )
        .expect("blocked packages must produce a hint");
        assert!(hint.contains("1 package(s) blocked"));
        assert!(hint.contains("always_allow"));
        assert!(hint.contains("/home/u/.config/pactience/config.toml"));
        // A blocked AUR package with unknown age triggers the second line.
        assert!(hint.contains("aur_heuristic"));
    }

    #[test]
    fn hint_omits_aur_line_when_heuristic_enabled() {
        let config = Config {
            aur_heuristic: true,
            ..Config::default()
        };
        let hint = render_hint(&sample_decisions(), &config, Path::new("/x/config.toml")).unwrap();
        assert!(hint.contains("always_allow"));
        assert!(!hint.contains("--aur-heuristic"));
    }

    #[test]
    fn no_hint_when_nothing_blocked() {
        let allowed = vec![Decision {
            verdict: Verdict::Allow,
            ..sample_decisions().into_iter().next().unwrap()
        }];
        assert!(render_hint(&allowed, &Config::default(), Path::new("/x")).is_none());
    }
}
