//! Execution of the safe upgrade set via `pacman` and the configured AUR
//! helper.
//!
//! This is the only module that can change system state. It is reached only
//! when the user passes `--apply`; the default dry-run path never touches it
//! beyond constructing the plan.

use crate::config::AurHelper;
use crate::error::{Error, Result};
use crate::model::{Decision, PackageSource, Verdict};

/// A command that would be (or was) executed, in argv form. Kept as data so
/// dry-run mode can print it and tests can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl std::fmt::Display for PlannedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

/// Side-effecting executor; injectable for tests.
pub trait Executor {
    fn run(&self, command: &PlannedCommand) -> Result<()>;
}

/// Runs commands for real, inheriting stdio so pacman can prompt the user.
pub struct SystemExecutor;

impl Executor for SystemExecutor {
    fn run(&self, command: &PlannedCommand) -> Result<()> {
        let status = std::process::Command::new(&command.program)
            .args(&command.args)
            .status()
            .map_err(|e| Error::command(command.to_string(), e.to_string()))?;
        if !status.success() {
            return Err(Error::CommandStatus {
                command: command.to_string(),
                status: format!("{status}"),
                stderr: String::new(),
            });
        }
        Ok(())
    }
}

/// Pacman package names: lowercase alnum plus `@ . _ + -`.
/// Validated defensively even though commands are spawned without a shell.
pub fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'+' | b'-'))
}

/// Effective UID from `/proc/self/status` (0 = root). Returns `None` when it
/// cannot be determined; callers treat `None` as non-root, which keeps the
/// safer behavior (warn less, elevate via sudo).
#[cfg(target_os = "linux")]
pub fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    // Format: "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
    status
        .lines()
        .find(|line| line.starts_with("Uid:"))?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
pub fn effective_uid() -> Option<u32> {
    None
}

/// Build the command plan for the allowed (and promoted) upgrades.
/// Repository packages go through `sudo pacman -S` — or plain `pacman -S`
/// when already running as root — and AUR packages through `<helper> -S`
/// (the helper runs sudo itself when needed). AUR upgrades with
/// `AurHelper::None` are a configuration error: discovery cannot produce
/// them, so this only guards against API misuse.
pub fn plan(
    decisions: &[Decision],
    as_root: bool,
    helper: AurHelper,
) -> Result<Vec<PlannedCommand>> {
    let mut repo = Vec::new();
    let mut aur = Vec::new();
    for d in decisions {
        if d.verdict == Verdict::Block {
            continue;
        }
        let name = &d.candidate.name;
        if !is_valid_package_name(name) {
            return Err(Error::parse(
                "package name",
                format!("refusing to upgrade invalid package name {name:?}"),
            ));
        }
        match d.candidate.source {
            PackageSource::Repo => repo.push(name.clone()),
            PackageSource::Aur => aur.push(name.clone()),
        }
    }

    let mut commands = Vec::new();
    if !repo.is_empty() {
        let mut args = vec!["-S".to_string()];
        args.extend(repo);
        // As root there is nothing to elevate; calling sudo would only add a
        // spurious dependency on sudo being installed.
        let program = if as_root { "pacman" } else { "sudo" };
        if !as_root {
            args.insert(0, "pacman".to_string());
        }
        commands.push(PlannedCommand {
            program: program.to_string(),
            args,
        });
    }
    if !aur.is_empty() {
        let program = helper.program().ok_or_else(|| {
            Error::parse(
                "apply plan",
                "AUR upgrades selected but aur_helper is \"none\"",
            )
        })?;
        let mut args = vec!["-S".to_string()];
        args.extend(aur);
        commands.push(PlannedCommand {
            program: program.to_string(),
            args,
        });
    }
    Ok(commands)
}

/// Execute the plan, stopping at the first failure.
pub fn execute(commands: &[PlannedCommand], executor: &dyn Executor) -> Result<()> {
    for cmd in commands {
        executor.run(cmd)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Publication, UpgradeCandidate};
    use std::cell::RefCell;

    fn decision(name: &str, source: PackageSource, verdict: Verdict) -> Decision {
        Decision {
            candidate: UpgradeCandidate {
                name: name.to_string(),
                installed_version: "1-1".to_string(),
                candidate_version: "2-1".to_string(),
                source,
            },
            publication: Publication::unknown(),
            verdict,
            reasons: vec![],
        }
    }

    #[test]
    fn valid_names() {
        assert!(is_valid_package_name("linux"));
        assert!(is_valid_package_name("lib32-gcc-libs"));
        assert!(is_valid_package_name("python3.12"));
        assert!(is_valid_package_name("ttf-font+awesome"));
        assert!(!is_valid_package_name(""));
        assert!(!is_valid_package_name("foo;rm -rf"));
        assert!(!is_valid_package_name("foo bar"));
        assert!(!is_valid_package_name("../etc"));
    }

    #[test]
    fn plan_separates_repo_and_aur_and_skips_blocked() {
        let decisions = vec![
            decision("linux", PackageSource::Repo, Verdict::Allow),
            decision("glibc", PackageSource::Repo, Verdict::Block),
            decision("paru-bin", PackageSource::Aur, Verdict::Promote),
        ];
        let cmds = plan(&decisions, false, AurHelper::Paru).unwrap();
        assert_eq!(
            cmds,
            vec![
                PlannedCommand {
                    program: "sudo".into(),
                    args: vec!["pacman".into(), "-S".into(), "linux".into()],
                },
                PlannedCommand {
                    program: "paru".into(),
                    args: vec!["-S".into(), "paru-bin".into()],
                },
            ]
        );
    }

    #[test]
    fn plan_uses_configured_helper_program() {
        let decisions = vec![decision("paru-bin", PackageSource::Aur, Verdict::Allow)];
        let cmds = plan(&decisions, false, AurHelper::Yay).unwrap();
        assert_eq!(
            cmds,
            vec![PlannedCommand {
                program: "yay".into(),
                args: vec!["-S".into(), "paru-bin".into()],
            }]
        );
    }

    #[test]
    fn plan_with_helper_none_rejects_aur_upgrades() {
        let decisions = vec![decision("paru-bin", PackageSource::Aur, Verdict::Allow)];
        assert!(plan(&decisions, false, AurHelper::None).is_err());
        // Repo-only plans are unaffected by the disabled helper.
        let decisions = vec![decision("linux", PackageSource::Repo, Verdict::Allow)];
        assert_eq!(plan(&decisions, false, AurHelper::None).unwrap().len(), 1);
    }

    #[test]
    fn plan_as_root_skips_sudo() {
        let decisions = vec![decision("linux", PackageSource::Repo, Verdict::Allow)];
        let cmds = plan(&decisions, true, AurHelper::Paru).unwrap();
        assert_eq!(
            cmds,
            vec![PlannedCommand {
                program: "pacman".into(),
                args: vec!["-S".into(), "linux".into()],
            }]
        );
    }

    #[test]
    fn empty_when_everything_blocked() {
        let decisions = vec![decision("glibc", PackageSource::Repo, Verdict::Block)];
        assert!(plan(&decisions, false, AurHelper::Paru).unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn effective_uid_is_readable() {
        // Value depends on who runs the tests; just require it to parse.
        assert!(effective_uid().is_some());
    }

    #[test]
    fn executor_is_called_in_order() {
        struct RecordingExecutor(RefCell<Vec<String>>);
        impl Executor for RecordingExecutor {
            fn run(&self, command: &PlannedCommand) -> Result<()> {
                self.0.borrow_mut().push(command.to_string());
                Ok(())
            }
        }
        let recorder = RecordingExecutor(RefCell::new(Vec::new()));
        let cmds = vec![
            PlannedCommand {
                program: "sudo".into(),
                args: vec!["pacman".into(), "-S".into(), "linux".into()],
            },
            PlannedCommand {
                program: "paru".into(),
                args: vec!["-S".into(), "paru-bin".into()],
            },
        ];
        execute(&cmds, &recorder).unwrap();
        assert_eq!(
            recorder.0.borrow().as_slice(),
            ["sudo pacman -S linux", "paru -S paru-bin"]
        );
    }
}
