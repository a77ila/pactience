//! Discovery of upgradable packages via `pacman -Qu` and the configured AUR
//! helper (`-Qua`).

use crate::config::AurHelper;
use crate::error::{Error, Result};
use crate::model::{PackageSource, UpgradeCandidate};

/// Raw result of running an external command.
#[derive(Debug)]
pub struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Abstraction over process execution so discovery can be tested without a
/// real pacman/AUR-helper installation.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput>;
}

/// Executes real system commands, capturing output. Never goes through a
/// shell, so arguments cannot be reinterpreted.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let display = format_command(program, args);
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::command(
                        display,
                        format!("executable {program:?} not found; is this an Arch Linux system?"),
                    )
                } else {
                    Error::command(display, e.to_string())
                }
            })?;
        Ok(CommandOutput {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

pub fn format_command(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Discover all upgrade candidates from the enabled sources. The AUR helper
/// is optional: when it is missing, only repository upgrades are reported and
/// a warning is recorded. `AurHelper::None` disables AUR discovery entirely
/// (no warning — repo-only is the explicit configuration).
pub fn discover(
    runner: &dyn CommandRunner,
    helper: AurHelper,
    sources: &[PackageSource],
) -> Result<(Vec<UpgradeCandidate>, Vec<String>)> {
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();

    if sources.contains(&PackageSource::Repo) {
        let repo_out = runner.run("pacman", &["-Qu"])?;
        check_listing_status("pacman -Qu", &repo_out, &mut warnings);
        candidates.extend(parse_qu_output(&repo_out.stdout, PackageSource::Repo));
    }

    if sources.contains(&PackageSource::Aur) {
        if let Some(program) = helper.program() {
            match runner.run(program, &["-Qua"]) {
                Ok(aur_out) => {
                    check_listing_status(&format!("{program} -Qua"), &aur_out, &mut warnings);
                    candidates.extend(parse_qu_output(&aur_out.stdout, PackageSource::Aur));
                }
                Err(e) => warnings.push(format!(
                    "AUR discovery unavailable ({e}); continuing with repository packages only"
                )),
            }
        } else if !sources.contains(&PackageSource::Repo) {
            warnings.push(
                "sources = [\"aur\"] but aur_helper = \"none\"; no AUR discovery possible"
                    .to_string(),
            );
        }
    }

    // Defensive dedupe: if a name appears from both sources, prefer repo.
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(c.name.clone()));

    Ok((candidates, warnings))
}

/// `pacman -Qu` exits non-zero in benign situations (e.g. no updates on some
/// versions); anything above 1 with empty stdout is treated as a failure.
fn check_listing_status(command: &str, out: &CommandOutput, warnings: &mut Vec<String>) {
    match out.status {
        Some(0) | Some(1) => {}
        other => warnings.push(format!(
            "{command} exited with status {other:?}: {}",
            out.stderr.trim()
        )),
    }
}

/// Parse `pacman -Qu` / `<helper> -Qua` output.
///
/// Lines look like `name 1.0-1 -> 1.1-1` and may carry annotations such as
/// `[ignored]`. Ignored lines are skipped: the user pinned those packages.
/// Malformed lines are skipped defensively.
pub fn parse_qu_output(output: &str, source: PackageSource) -> Vec<UpgradeCandidate> {
    let mut candidates = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.contains("[ignored]") {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let (Some(name), Some(old), Some(arrow), Some(new)) =
            (tokens.next(), tokens.next(), tokens.next(), tokens.next())
        else {
            continue;
        };
        if arrow != "->" || !crate::apply::is_valid_package_name(name) {
            continue;
        }
        candidates.push(UpgradeCandidate {
            name: name.to_string(),
            installed_version: old.to_string(),
            candidate_version: new.to_string(),
            source,
        });
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRunner {
        responses: Vec<(String, Result<CommandOutput>)>,
    }

    impl FakeRunner {
        fn ok(program: &str, stdout: &str) -> (String, Result<CommandOutput>) {
            (
                program.to_string(),
                Ok(CommandOutput {
                    status: Some(0),
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                }),
            )
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, _args: &[&str]) -> Result<CommandOutput> {
            for (name, out) in &self.responses {
                if name == program {
                    return match out {
                        Ok(o) => Ok(CommandOutput {
                            status: o.status,
                            stdout: o.stdout.clone(),
                            stderr: o.stderr.clone(),
                        }),
                        Err(_) => Err(Error::command(program, "not found")),
                    };
                }
            }
            Err(Error::command(program, "not found"))
        }
    }

    #[test]
    fn parses_pacman_qu_lines() {
        let out = "linux 6.8.1.arch1-1 -> 6.8.2.arch1-1\nfirefox 124.0-1 -> 124.0.1-1\n";
        let c = parse_qu_output(out, PackageSource::Repo);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "linux");
        assert_eq!(c[0].installed_version, "6.8.1.arch1-1");
        assert_eq!(c[0].candidate_version, "6.8.2.arch1-1");
        assert_eq!(c[0].source, PackageSource::Repo);
    }

    #[test]
    fn skips_ignored_and_malformed_lines() {
        let out = "linux 6.8-1 -> 6.9-1 [ignored]\n\nnot enough tokens\nevil;name 1 -> 2\nvim 9.1-1 -> 9.2-1 extra-annotation\n";
        let c = parse_qu_output(out, PackageSource::Repo);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "vim");
    }

    #[test]
    fn parses_aur_lines_with_source() {
        let out = "paru-bin 2.0.3-1 -> 2.0.4-1\n";
        let c = parse_qu_output(out, PackageSource::Aur);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].source, PackageSource::Aur);
    }

    const BOTH: &[PackageSource] = &[PackageSource::Repo, PackageSource::Aur];
    const REPO: &[PackageSource] = &[PackageSource::Repo];
    const AUR: &[PackageSource] = &[PackageSource::Aur];

    #[test]
    fn discover_combines_sources_and_survives_missing_paru() {
        let runner = FakeRunner {
            responses: vec![FakeRunner::ok("pacman", "linux 1-1 -> 2-1\n")],
        };
        let (candidates, warnings) = discover(&runner, AurHelper::Paru, BOTH).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("AUR discovery unavailable"));
    }

    #[test]
    fn discover_uses_configured_helper_program() {
        let runner = FakeRunner {
            responses: vec![
                FakeRunner::ok("pacman", "linux 1-1 -> 2-1\n"),
                FakeRunner::ok("yay", "paru-bin 1-1 -> 2-1\n"),
            ],
        };
        let (candidates, warnings) = discover(&runner, AurHelper::Yay, BOTH).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].name, "paru-bin");
        assert_eq!(candidates[1].source, PackageSource::Aur);
    }

    #[test]
    fn discover_with_helper_none_skips_aur_without_warning() {
        let runner = FakeRunner {
            responses: vec![FakeRunner::ok("pacman", "linux 1-1 -> 2-1\n")],
        };
        let (candidates, warnings) = discover(&runner, AurHelper::None, BOTH).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, PackageSource::Repo);
        assert!(warnings.is_empty());
    }

    #[test]
    fn discover_dedupes_preferring_repo() {
        let runner = FakeRunner {
            responses: vec![
                FakeRunner::ok("pacman", "foo 1-1 -> 2-1\n"),
                FakeRunner::ok("paru", "foo 1-1 -> 2-1\nbar 3-1 -> 4-1\n"),
            ],
        };
        let (candidates, _) = discover(&runner, AurHelper::Paru, BOTH).unwrap();
        assert_eq!(candidates.len(), 2);
        let foo = candidates.iter().find(|c| c.name == "foo").unwrap();
        assert_eq!(foo.source, PackageSource::Repo);
    }

    #[test]
    fn sources_repo_never_touches_the_aur_helper() {
        // The runner has no paru response: an AUR call would produce the
        // "unavailable" warning, so a clean run proves it was skipped.
        let runner = FakeRunner {
            responses: vec![FakeRunner::ok("pacman", "linux 1-1 -> 2-1\n")],
        };
        let (candidates, warnings) = discover(&runner, AurHelper::Paru, REPO).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, PackageSource::Repo);
        assert!(warnings.is_empty());
    }

    #[test]
    fn sources_aur_never_touches_pacman() {
        // No pacman response: a repo call would hard-error (`?`), so success
        // proves pacman was skipped.
        let runner = FakeRunner {
            responses: vec![FakeRunner::ok("paru", "paru-bin 1-1 -> 2-1\n")],
        };
        let (candidates, warnings) = discover(&runner, AurHelper::Paru, AUR).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, PackageSource::Aur);
        assert!(warnings.is_empty());
    }

    #[test]
    fn sources_aur_with_helper_none_warns() {
        let runner = FakeRunner { responses: vec![] };
        let (candidates, warnings) = discover(&runner, AurHelper::None, AUR).unwrap();
        assert!(candidates.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("sources = [\"aur\"]"));
    }
}
