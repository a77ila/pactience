//! Accurate per-version publication dates for AUR packages from git history.
//!
//! Every AUR package is a git repository (`https://aur.archlinux.org/<pkg>.git`).
//! The RPC has no per-version timestamp, but git history does: we walk
//! `.SRCINFO` (which records `pkgver`/`pkgrel`/`epoch` on separate lines)
//! newest-to-oldest and take the oldest commit in the contiguous run whose
//! reconstructed version equals the candidate. That commit's date is when the
//! exact candidate build (including pkgrel rebuilds) first appeared.
//!
//! When `.SRCINFO` is missing or unparseable (very old history), we fall back
//! to the upstream `pkgver=` bump commit in `PKGBUILD`.
//!
//! Repositories are kept as bare clones under the cache directory and only
//! fetched afterwards, so repeat runs are cheap. A failed fetch is tolerated
//! (the existing clone is used); a failed initial clone is an error and the
//! resolver falls back to weaker AUR sources with a warning.

use std::path::{Path, PathBuf};

use crate::discovery::CommandRunner;
use crate::error::{Error, Result};
use crate::model::{Publication, PublicationBasis, UpgradeCandidate};

use super::PublicationSource;

pub const DEFAULT_BASE_URL: &str = "https://aur.archlinux.org";

pub struct AurGitPublicationSource<'a> {
    runner: &'a dyn CommandRunner,
    cache_dir: PathBuf,
    base_url: String,
}

impl<'a> AurGitPublicationSource<'a> {
    pub fn new(runner: &'a dyn CommandRunner, cache_dir: PathBuf) -> Self {
        AurGitPublicationSource {
            runner,
            cache_dir,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base_url(runner: &'a dyn CommandRunner, cache_dir: PathBuf, base_url: &str) -> Self {
        AurGitPublicationSource {
            runner,
            cache_dir,
            base_url: base_url.to_string(),
        }
    }

    /// Clone the bare repo on first use, fetch updates afterwards. A failed
    /// fetch is tolerated: the existing clone may be slightly stale but is
    /// still usable (and fully usable offline for already-known versions).
    fn ensure_repo(&self, name: &str, repo: &Path) -> Result<()> {
        if repo.join("HEAD").exists() {
            let repo_str = repo.to_string_lossy().into_owned();
            // Best-effort refresh; ignore failures.
            let _ = self
                .runner
                .run("git", &["-C", &repo_str, "fetch", "--quiet", "origin"]);
            return Ok(());
        }
        if let Some(parent) = repo.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.to_path_buf(), e))?;
        }
        let url = format!("{}/{name}.git", self.base_url);
        let repo_str = repo.to_string_lossy().into_owned();
        let out = self
            .runner
            .run("git", &["clone", "--bare", "--quiet", &url, &repo_str])?;
        if out.status != Some(0) {
            return Err(Error::CommandStatus {
                command: format!("git clone --bare {url}"),
                status: format!("{:?}", out.status),
                stderr: out.stderr.trim().to_string(),
            });
        }
        Ok(())
    }

    /// Commits touching `.SRCINFO`, newest first: `(sha, timestamp)`.
    fn srcinfo_history(&self, repo: &Path) -> Result<Vec<(String, i64)>> {
        let repo_str = repo.to_string_lossy().into_owned();
        let out = self.runner.run(
            "git",
            &["-C", &repo_str, "log", "--format=%H %ct", "--", ".SRCINFO"],
        )?;
        if out.status != Some(0) {
            return Err(Error::CommandStatus {
                command: format!("git -C {repo_str} log -- .SRCINFO"),
                status: format!("{:?}", out.status),
                stderr: out.stderr.trim().to_string(),
            });
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|line| {
                let (sha, ts) = line.split_once(' ')?;
                Some((sha.to_string(), ts.trim().parse::<i64>().ok()?))
            })
            .collect())
    }

    /// The full `[epoch:]pkgver-pkgrel` version recorded by `.SRCINFO` at a
    /// commit. `None` when the file is missing or incomplete there.
    fn version_at(&self, repo: &Path, sha: &str) -> Result<Option<String>> {
        let repo_str = repo.to_string_lossy().into_owned();
        let spec = format!("{sha}:.SRCINFO");
        let out = self.runner.run("git", &["-C", &repo_str, "show", &spec])?;
        if out.status != Some(0) {
            // File absent in this commit: treat as "no version", not fatal.
            return Ok(None);
        }
        Ok(parse_srcinfo_version(&out.stdout))
    }

    /// Earliest commit timestamp touching `pickaxe` in `path`, if any.
    fn pickaxe_date(&self, repo: &Path, pickaxe: &str, path: &str) -> Result<Option<i64>> {
        let repo_str = repo.to_string_lossy().into_owned();
        let out = self.runner.run(
            "git",
            &[
                "-C",
                &repo_str,
                "log",
                "--format=%ct",
                "-S",
                pickaxe,
                "--",
                path,
            ],
        )?;
        if out.status != Some(0) {
            return Err(Error::CommandStatus {
                command: format!("git -C {repo_str} log -S {pickaxe:?} -- {path}"),
                status: format!("{:?}", out.status),
                stderr: out.stderr.trim().to_string(),
            });
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|line| line.trim().parse::<i64>().ok())
            .min())
    }
}

impl PublicationSource for AurGitPublicationSource<'_> {
    fn publication(&self, candidate: &UpgradeCandidate) -> Result<Publication> {
        let name = &candidate.name;
        if !crate::apply::is_valid_package_name(name) {
            return Err(Error::parse(
                "package name",
                format!("refusing git lookup for invalid package name {name:?}"),
            ));
        }
        let repo = self.cache_dir.join(format!("{name}.git"));
        self.ensure_repo(name, &repo)?;

        // Walk .SRCINFO history newest-to-oldest; the candidate's publication
        // is the oldest commit in the contiguous top-run matching it.
        let mut introduced: Option<i64> = None;
        for (sha, ts) in self.srcinfo_history(&repo)? {
            match self.version_at(&repo, &sha)? {
                Some(version) if version == candidate.candidate_version => {
                    introduced = Some(ts);
                }
                _ => break,
            }
        }
        if let Some(ts) = introduced {
            return Ok(Publication::known(ts, PublicationBasis::AurGit));
        }

        // Fallback: upstream pkgver bump in PKGBUILD (covers missing .SRCINFO
        // and very old history). Epoch/pkgrel are not part of pkgver there.
        let version = candidate
            .candidate_version
            .rsplit(':')
            .next()
            .unwrap_or(&candidate.candidate_version);
        let upstream = version.rsplit_once('-').map_or(version, |(up, _)| up);
        let pkgver = format!("pkgver={upstream}");
        if let Some(ts) = self.pickaxe_date(&repo, &pkgver, "PKGBUILD")? {
            return Ok(Publication::known(ts, PublicationBasis::AurGit));
        }
        Ok(Publication::unknown())
    }
}

/// Reconstruct the full `[epoch:]pkgver[-pkgrel]` version from `.SRCINFO`
/// content (first `pkgver`/`pkgrel`/`epoch` entries win).
pub fn parse_srcinfo_version(content: &str) -> Option<String> {
    let mut pkgver = None;
    let mut pkgrel = None;
    let mut epoch = None;
    for line in content.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "pkgver" if pkgver.is_none() => pkgver = Some(value),
            "pkgrel" if pkgrel.is_none() => pkgrel = Some(value),
            "epoch" if epoch.is_none() => epoch = Some(value),
            _ => {}
        }
    }
    let pkgver = pkgver?;
    let mut version = String::new();
    if let Some(epoch) = epoch {
        version.push_str(epoch);
        version.push(':');
    }
    version.push_str(pkgver);
    if let Some(pkgrel) = pkgrel {
        version.push('-');
        version.push_str(pkgrel);
    }
    Some(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::CommandOutput;
    use crate::model::PackageSource;
    use std::cell::RefCell;

    /// Records every invocation; responds based on the joined command line.
    struct ScriptRunner {
        invocations: RefCell<Vec<String>>,
        responses: Vec<(String, CommandOutput)>,
    }

    impl ScriptRunner {
        fn new() -> Self {
            ScriptRunner {
                invocations: RefCell::new(Vec::new()),
                responses: Vec::new(),
            }
        }

        fn on(mut self, needle: &str, status: i32, stdout: &str) -> Self {
            self.responses.push((
                needle.to_string(),
                CommandOutput {
                    status: Some(status),
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
            ));
            self
        }

        fn calls_matching(&self, needle: &str) -> usize {
            self.invocations
                .borrow()
                .iter()
                .filter(|c| c.contains(needle))
                .count()
        }
    }

    impl CommandRunner for ScriptRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
            let cmd = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            self.invocations.borrow_mut().push(cmd.clone());
            for (needle, out) in &self.responses {
                if cmd.contains(needle) {
                    return Ok(CommandOutput {
                        status: out.status,
                        stdout: out.stdout.clone(),
                        stderr: out.stderr.clone(),
                    });
                }
            }
            // Default: git clone/fetch succeed silently, git log finds nothing.
            Ok(CommandOutput {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    fn candidate(name: &str, version: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: "1.0-1".to_string(),
            candidate_version: version.to_string(),
            source: PackageSource::Aur,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aag-aurgit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn srcinfo(pkgver: &str, pkgrel: &str) -> String {
        format!("pkgbase = test\n\tpkgver = {pkgver}\n\tpkgrel = {pkgrel}\n\npkgname = test\n")
    }

    /// Runner preloaded with a `.SRCINFO` history: newest-first
    /// `(sha, ts, pkgver, pkgrel)` rows, served via `git log`/`git show`.
    fn history_runner(rows: &[(&str, i64, &str, &str)]) -> ScriptRunner {
        let log = rows
            .iter()
            .map(|(sha, ts, _, _)| format!("{sha} {ts}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut runner = ScriptRunner::new().on("-- .SRCINFO", 0, &log);
        for (sha, _, pkgver, pkgrel) in rows {
            runner = runner.on(&format!("show {sha}:.SRCINFO"), 0, &srcinfo(pkgver, pkgrel));
        }
        runner
    }

    #[test]
    fn parses_srcinfo_versions() {
        assert_eq!(
            parse_srcinfo_version(&srcinfo("2.1.0", "2")),
            Some("2.1.0-2".to_string())
        );
        assert_eq!(
            parse_srcinfo_version("\tepoch = 1\n\tpkgver = 2.0\n\tpkgrel = 1\n"),
            Some("1:2.0-1".to_string())
        );
        assert_eq!(
            parse_srcinfo_version("\tpkgver = 2.0\n"),
            Some("2.0".to_string())
        );
        assert_eq!(parse_srcinfo_version("garbage"), None);
    }

    #[test]
    fn clones_then_dates_the_exact_candidate_run() {
        let dir = temp_dir("clone");
        // Newest first: candidate 2.1.0-2 spans two commits; its publication
        // is the OLDEST of the run (200), when the pkgrel-2 build appeared.
        let runner = history_runner(&[
            ("aaa", 300, "2.1.0", "2"),
            ("bbb", 200, "2.1.0", "2"),
            ("ccc", 100, "2.1.0", "1"),
        ]);
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("paru", "2.1.0-2")).unwrap();
        assert_eq!(p.published_at, Some(200));
        assert_eq!(p.basis, PublicationBasis::AurGit);
        assert_eq!(runner.calls_matching("clone --bare"), 1);
        assert_eq!(runner.calls_matching("https://aur.test/paru.git"), 1);
        // The PKGBUILD fallback must not run when .SRCINFO resolves.
        assert_eq!(runner.calls_matching("-- PKGBUILD"), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pkgrel_bump_redates_the_build() {
        let dir = temp_dir("pkgrel");
        // Candidate 2.1.0-2 only exists in the newest commit: the rebuild is
        // treated as a fresh publication even though pkgver is older.
        let runner = history_runner(&[("aaa", 300, "2.1.0", "2"), ("bbb", 100, "2.1.0", "1")]);
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("paru", "2.1.0-2")).unwrap();
        assert_eq!(p.published_at, Some(300));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn epoch_versions_match_exactly() {
        let dir = temp_dir("epoch");
        let runner = ScriptRunner::new().on("-- .SRCINFO", 0, "aaa 300").on(
            "show aaa:.SRCINFO",
            0,
            "\tepoch = 1\n\tpkgver = 2.0\n\tpkgrel = 1\n",
        );
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("foo", "1:2.0-1")).unwrap();
        assert_eq!(p.published_at, Some(300));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn existing_repo_is_fetched_not_cloned() {
        let dir = temp_dir("existing");
        let repo = dir.join("paru.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("HEAD"), "ref: refs/heads/master").unwrap();
        let runner = history_runner(&[("aaa", 100, "2.1.0", "2")]);
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("paru", "2.1.0-2")).unwrap();
        assert_eq!(p.published_at, Some(100));
        assert_eq!(runner.calls_matching("clone --bare"), 0);
        assert_eq!(runner.calls_matching("fetch --quiet"), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_pkgbuild_pkgver() {
        let dir = temp_dir("pkgbuild");
        // No .SRCINFO history (empty log); PKGBUILD has the pkgver bump.
        let runner = ScriptRunner::new().on("-- PKGBUILD", 0, "1700000000\n");
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("foo", "2.0-3")).unwrap();
        assert_eq!(p.published_at, Some(1700000000));
        assert!(runner.calls_matching("-S pkgver=2.0") >= 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_version_is_unknown_not_error() {
        let dir = temp_dir("unknown");
        // History exists but never contains the candidate version.
        let runner = history_runner(&[("aaa", 100, "9.9", "1")]);
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        let p = source.publication(&candidate("foo", "1.0-1")).unwrap();
        assert_eq!(p.published_at, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clone_failure_is_an_error() {
        let dir = temp_dir("fail");
        let runner = ScriptRunner::new().on("clone --bare", 128, "");
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        assert!(source.publication(&candidate("foo", "1.0-1")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_names_are_rejected() {
        let dir = temp_dir("invalid");
        let runner = ScriptRunner::new();
        let source =
            AurGitPublicationSource::with_base_url(&runner, dir.clone(), "https://aur.test");
        assert!(source.publication(&candidate("bad;name", "1.0-1")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
