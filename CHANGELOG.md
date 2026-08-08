# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.2] - 2026-08-08

### Added

- AUR publish resilience: the AUR push in `release.yml` was extracted into
  a reusable composite action (`.github/actions/aur-publish`) that retries
  the AUR git clone, and a new scheduled `aur-sync` workflow (every 6
  hours, plus manual dispatch) re-checks whether the AUR packages lag the
  latest GitHub Release and republishes whatever is missing. Previously an
  AUR git interface outage (maintenance, DDoS mitigation — these can last
  for days) failed the release workflow and left the AUR packages stale
  until someone re-ran it manually; the sync workflow probes the interface
  first and skips quietly while the AUR is down.

### Fixed

- A pending upgrade whose candidate version drops a versioned soname
  provide (e.g. ffmpeg 9 dropping `libavcodec.so=62-64`) is now blocked
  whenever another candidate still requires that capability — previously
  the edge was classified as satisfied by the installed provider, so
  pactience produced an upgrade set pacman then refused (`breaks
  dependency ... required by <held-back package>`). The block cascades to
  anything requiring the provider's candidate. Repo candidates whose
  provides are unknown (AUR) are unaffected, and a second candidate still
  providing the capability (a compat package) takes over instead.
- The same refusal happened when the held-back dependent was *already
  rebuilt* against the new soname (its candidate dep then points at the
  provider's candidate, so the candidate-side check above does not apply):
  pacman checks the *installed* dependent's declarations, which pactience
  never read. A new reverse pass (`deps::find_installed_breaks`) now checks
  every installed package's installed dependencies against capabilities
  dropped by pending upgrades: a pending dependent couples with the
  provider and is promoted alongside it (its candidate comes from the same
  consistent repo), while a non-pending dependent (foreign/AUR package, or
  not in the upgrade set) blocks the provider outright, since it can never
  join the transaction.

## [0.2.1] - 2026-08-06

### Added

- `refresh` configuration option (default true) and `--no-refresh` flag:
  pactience now refreshes the pacman sync databases (`sudo pacman -Sy`, or
  plain `pacman -Sy` as root) before each run, so discovery and `--apply`
  work on current repository data. Stale databases previously hid upgrades
  and made downloads fail with 404s when mirrors no longer carried the
  recorded versions. Skipped for AUR-only runs; a failed refresh degrades
  to a warning and the run continues with possibly stale data. pacman's
  output goes to stderr so the report (and `--json`) stays clean.

## [0.2.0] - 2026-08-06

### Added

- `new_dependencies` configuration option and `--new-dependencies` flag
  (`block` default, `warn`, `allow`): when an upgrade requires a brand-new
  dependency that is not installed yet (e.g. telegram-desktop gaining a
  cmark-gfm dependency), the dependent is blocked. In `block` mode the
  missing package is listed as a blocked row (publication date resolved
  for information only) so the report shows what the upgrade is waiting
  for. In `warn`/`allow` mode, new dependencies
  resolvable from the sync DB are pulled into the set as new installs
  (shown with `-` as the installed version), gated by the same min-age
  policy — but never promoted: a new package that is too young or of
  unknown age blocks the dependent. `warn` additionally prints a warning
  for each new package to be installed (on stderr and in a line after the
  report summary). AUR-only new dependencies still block the dependent.

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

[0.2.0]: https://github.com/a77ila/pactience/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/a77ila/pactience/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/a77ila/pactience/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/a77ila/pactience/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/a77ila/pactience/releases/tag/v0.1.0
