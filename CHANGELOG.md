# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.3] - 2026-07-23

### Added

- `sources` configuration option: choose which package sources pactience
  manages — `["repo", "aur"]` (default), `["repo"]` (official repositories
  only), or `["aur"]` (AUR only). The skipped side's commands are never
  invoked.
- First-run prompt for source selection, and a one-time upgrade prompt for
  config files written by older versions (the choice is appended to the
  file, so it is asked exactly once).
- `--sources repo,aur` CLI flag to override the config for a single run;
  also suppresses the prompts and works with `--json` and in scripts.
- `--set-min-age DAYS`: persists `min_age_days` into the config file and
  exits. Creates the file from the template when missing; replaces an
  existing active line in place.
- The JSON report now includes the active `sources`.
- `/merge` pull-request comment command (GitHub Actions): merges the PR with
  a GitLab-style commit message (`Merge branch '<source>' into '<target>'`
  plus a `See pull request <repo>#<n>` trailer). Restricted to the repo
  owner, org members, and collaborators.

### Fixed

- Partial-upgrade hazard: an allowed dependency could be upgraded while a
  co-pending dependent was held back — invisible in the metadata because
  Arch rarely versions its dependencies (the classic unversioned soname
  breakage). Coupled candidates now share a verdict: the dependent is
  promoted alongside, or the dependency is blocked.
- Forged AUR commit dates: git histories with non-monotonic commit
  timestamps (a commit predating its own parent — impossible in the
  append-only AUR) are now rejected as tampered and fail safe to unknown,
  which blocks by default.

## [0.1.2] - 2026-07-23

### Fixed

- TTY-dependent progress test breaking interactive AUR builds: libtest
  captures output without redirecting fd 2, so `stderr().is_terminal()`
  stayed true under `cargo test` on a real terminal, failing the
  no-terminal test during `makepkg check()`. The terminal flag is now
  injected, making the test deterministic.

## [0.1.1] - 2026-07-23

### Added

- `pactience-bin` AUR package (`packaging/aur-bin/PKGBUILD`) with per-arch
  sources and checksums, published by an `aur-bin` release job.
- Release binaries for aarch64 alongside x86_64; LICENSE files bundled in
  the release assets.

### Fixed

- AUR source build: `options=('!lto')` in the PKGBUILD — makepkg's default
  `lto` option turned the C objects built by ring/zstd-sys into GCC LTO
  bytecode that rust-lld cannot link.

## [0.1.0] - 2026-07-22

### Added

- First release: minimum-age upgrade policy for Arch Linux with repo
  (Arch Linux Archive / `%BUILDDATE%`) and AUR (git history / `LastModified`
  heuristic) publication dates, dependency-safe upgrade sets
  (`dependency-respecting` / `strict-closure`), dry-run by default with
  `--apply`, table and JSON output, first-run configuration template, and
  AUR publication via the release workflow.

[0.1.3]: https://github.com/a77ila/pactience/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/a77ila/pactience/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/a77ila/pactience/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/a77ila/pactience/releases/tag/v0.1.0
