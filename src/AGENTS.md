# Agent Guide for `pactience`

This file is written for AI coding agents. It describes the project as it currently exists, not as it is eventually meant to become. Read `INSTRUCTIONS.md` for the full design brief.

## Project Overview

`pactience` is a Rust CLI tool for Arch Linux that enforces a "minimum package age" policy before upgrading packages. The workflow is:

1. Discover upgradable packages via `pacman -Qu` (repo) and the configured AUR helper's `-Qua` (AUR). The helper is `paru`, `yay`, or `none` (AUR handling disabled), set via `aur_helper` / `--aur-helper` and chosen interactively on first run.
2. Determine the real publication timestamp of each candidate version: the Arch Linux Archive is authoritative (first appearance in the package pool), the repo sync DB `%BUILDDATE%` is the fallback, and AUR packages are "unknown" unless the `LastModified` heuristic is enabled.
3. Compare the package age against a configurable threshold (default 4 days).
4. Enforce dependency safety: required younger dependencies are promoted (`dependency-respecting`, default) or their dependents are blocked (`strict-closure`). Co-pending packages linked by a dependency edge share a verdict: an allowed dependency is never upgraded while a dependent is held back (the dependent is promoted, or the dependency blocked) — Arch rarely versions deps, so this closes the unversioned-soname partial-upgrade hole.
5. Optionally apply the resulting safe upgrade set via `sudo pacman -S` / `<aur-helper> -S` when `--apply` is passed. Dry-run is the default.

### Age Source Strategy

The authoritative source for a package version's publication date is the **Arch Linux Archive** (`https://archive.archlinux.org`). Instead of probing daily `/repos/YYYY/MM/DD/` snapshots (one request per probe), the implementation reads the package pool index `/packages/<first-letter>/<name>/`: the per-file timestamps there record when each version file entered the archive, i.e. the same date as its first snapshot. One request per package.

- **Index format gotcha:** archive.archlinux.org serves an nginx autoindex with `DD-Mon-YYYY HH:MM` timestamps; `src/publication/archive.rs` parses both that and ISO `YYYY-MM-DD HH:MM`. hrefs are percent-encoded (`+` becomes `%2B`) and are decoded before matching.
- **Pool retention:** old versions are pruned from the pool, so very old candidates resolve as "unknown" and fall back to `%BUILDDATE%`. Real candidates (just-published versions) are always present.
- **File names omit the epoch:** candidate `1:2.0-1` matches pool file `2.0-1`.
- **AUR git history (primary):** the RPC has no per-version publication timestamp, but every AUR package is a git repo. `src/publication/aur_git.rs` keeps bare clones under `~/.cache/pactience/aur-git/`, walks `.SRCINFO` history newest-to-oldest (reconstructing `[epoch:]pkgver-pkgrel` per commit, since `pkgver`/`pkgrel` are on *separate* lines there), and dates the candidate to the oldest commit in the contiguous top-run matching it — so a pkgrel rebuild counts as a fresh publication. Falls back to the `pkgver=` bump in `PKGBUILD` for very old history. Disable with `aur_git = false` / `--no-aur-git`.
- **AUR heuristic (optional fallback):** when git lookup is disabled or fails, the RPC `LastModified` field can be used via `aur_heuristic` / `--aur-heuristic`; results are labelled `aur-lastmodified (heuristic)`.

### Current State

The tool is fully implemented per phases 1–6 of `INSTRUCTIONS.md`: CLI, discovery, publication sources with caching, dependency analysis, policy engine, table/JSON output, and `--apply`. All external interactions (process execution, HTTP) sit behind traits so tests run without pacman, an AUR helper, or network.

## Technology Stack

- **Language:** Rust (stable), edition 2024.
- **Build System:** Cargo.
- **Target OS:** Arch Linux (the tool shells out to `pacman` and the configured AUR helper — `paru` or `yay` — and reads `/var/lib/pacman/sync/*.db` + `/var/lib/pacman/local/`).
- **Data Sources:** Arch repository sync databases, Arch Archive package pool, AUR RPC v5.
- **Configuration:** `~/.config/pactience/config.toml`. Created automatically on first execution from a fully-commented template (`config::CONFIG_TEMPLATE`, kept honest by a parse-to-defaults test) plus the AUR helper chosen on first run (`config::write_config_with_helper`): interactively when stdin is a TTY and `--json` is not set, otherwise auto-detected from PATH (`config::detect_aur_helper`, paru preferred) with paru as fallback; `--config`, `--clear-cache`, and `--json` runs never prompt. Every option has a built-in default. A missing *explicitly passed* `--config` path is a warning, not auto-created (likely a typo).
- **Cache:** `~/.cache/pactience/publications.json`. Positive publication results never expire (immutable facts); negative ("unknown") results expire after `cache_ttl_secs` (default 86400).

## Build and Run Commands

```bash
cargo build
cargo build --release
cargo run                # dry-run report
cargo run -- --apply     # perform the allowed upgrades
cargo run -- --json      # machine-readable output
cargo check
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

The release binary is produced at `target/release/pactience`.

## Project Layout

The repository root contains GitHub-level files; the crate lives in `src/`.
All cargo commands run from the crate directory (`src/`).

```
<repo root> (= /workspace in the dev container)
├── .github/workflows/ci.yml      # CI (runs cargo with working-directory: src)
├── .github/workflows/release.yml # Tag (v*) -> GitHub Release binary + AUR publish
├── packaging/aur/PKGBUILD        # Template the release workflow pushes to the AUR
├── .docker/                  # Dev container (compose mounts repo root at /workspace;
│                             #   target-cache -> /workspace/src/target, dist -> /workspace/src/dist)
├── console.example           # Captured real-world run (linked from the root README)
├── README.md                 # Repo-facing readme (may reference repo-level files)
├── LICENSE-MIT, LICENSE-APACHE
└── src/                      # THE CRATE (pactience)
    ├── Cargo.toml
    ├── Cargo.lock
    ├── README.md             # crates.io-facing readme (self-contained; keep in sync with root)
    ├── INSTRUCTIONS.md       # Full product requirements and phased plan
    ├── Makefile              # build / dist (copies release binary to dist/)
    ├── src/
    │   ├── main.rs             # Wiring: CLI -> config -> discovery -> publication -> policy -> output -> apply
    │   ├── cli.rs              # clap CLI definition (--apply, --json, --config, -m/--min-age-days, --color,
    │   │                       #   -v/-q, --summary-only, --clear-cache, ...)
    │   ├── config.rs           # TOML config load/merge/validate; first-run template + AUR helper
    │   │                       #   selection (prompt/PATH detect); XDG paths; DependencyPolicy, AurHelper
    │   ├── error.rs            # Central thiserror Error type
    │   ├── logging.rs          # Leveled stderr diagnostics (error/warn/info/debug from -q/-v/-vv)
    │   ├── model.rs            # UpgradeCandidate, Publication(+Basis), Decision, Verdict
    │   ├── vercmp.rs           # Faithful alpm_pkg_vercmp port (epoch/rpmvercmp/pkgrel)
    │   ├── date.rs             # UTC epoch <-> civil date helpers (no chrono dep)
    │   ├── discovery.rs        # pacman -Qu / <aur-helper> -Qua parsing behind CommandRunner trait
    │   ├── db.rs               # Sync DB reader (tar + gzip/zstd, magic-byte sniffed), local DB reader,
    │   │                       #   DepSpec/Provide parsing, provides index
    │   ├── deps.rs             # Dependency classification: installed / requires-candidate /
    │   │                       #   coupled-with-candidate / unsatisfied
    │   ├── policy.rs           # Age verdicts + dependency-respecting / strict-closure fixpoint
    │   ├── progress.rs         # Minimal stderr progress bar for slow passes (TTY, default verbosity only)
    │   ├── publication/
    │   │   ├── mod.rs          # PublicationSource trait + resolver (cache -> Archive -> builddate)
    │   │   ├── archive.rs      # Archive package pool index parsing
    │   │   ├── aur.rs          # AUR RPC v5 multi-info client + LastModified heuristic source
    │   │   └── aur_git.rs      # AUR git-history source: bare clones + .SRCINFO version walk
    │   ├── cache.rs            # JSON publication cache (atomic write, TTL for negatives)
    │   ├── http.rs             # HttpClient trait + ureq impl (15s timeout, UA header)
    │   ├── apply.rs            # Upgrade plan building (name validation) + Executor trait
    │   └── output.rs           # Summary table (ANSI-colored AGE/VERDICT vs. the age gate) + JSON report
    └── tests/
        └── cli.rs              # Binary-level CLI tests (flags, exit codes)
```

## Code Style Guidelines

- Use `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before committing; both must be clean.
- No `unwrap()`/`expect()` in production paths (tests may use them); errors flow through `error::Error`.
- Keep external effects injectable: `CommandRunner` (processes), `HttpClient` (network), `Executor` (apply). Never call `std::process::Command` or ureq directly outside those implementations.
- Treat all external input (pacman output, DB records, AUR RPC, index HTML, config files) as untrusted: parse defensively, skip malformed records, never fail the whole run for one bad record.
- Degraded operation is a warning, not an error: a missing AUR helper, unreadable DBs, failed RPC, or corrupt cache all degrade with a stderr `warning:` line while the run continues.
- Version comparisons must go through `vercmp::vercmp` (alpm semantics: epoch dominates; extra segments are *newer*; alpha segments beat numeric ones).

## Testing Instructions

Run tests with `cargo test` (unit tests live in `#[cfg(test)]` modules; CLI tests in `tests/cli.rs`). The suite covers:

- `pacman -Qu`/`<aur-helper> -Qua` parsing (ignored lines, malformed lines, dedupe, helper program selection, `AurHelper::None` skips AUR silently).
- alpm vercmp edge cases (epoch, pkgrel, alpha segments, leading zeros).
- Sync/local DB parsing incl. a generated tar fixture; DepSpec/Provide parsing.
- Archive index parsing (both date formats, epoch stripping, prefix collisions) with a mock `HttpClient`.
- AUR RPC parsing and the heuristic source (version-match guard).
- AUR git source: `.SRCINFO` version reconstruction, contiguous-run dating (pkgrel bumps redate), clone/fetch behavior, PKGBUILD fallback, all via a scripted `CommandRunner`. Validated live against `aur.archlinux.org/paru.git`.
- Cache TTL semantics (positives immortal, negatives expire), corrupt-cache recovery, and `cache::clear` idempotency (`--clear-cache` wipes the whole cache dir incl. `aur-git/`, then exits before any config creation or analysis).
- First-run config template: parses to exact defaults, documents every option, creates parent dirs, never overwrites an existing file. AUR helper selection: `parse_helper_choice` (numbers/names/empty/invalid), `detect_aur_helper_with` (paru preferred over yay), the prompt loop (invalid → re-ask, empty/EOF → default), and `write_config_with_helper` recording the active choice.
- Policy engine: age thresholds, unknown-age rule, allow/block lists, promotion chains, `always_block` dependents, strict-closure transitivity, and co-pending coupling (held-back dependents promote alongside an allowed dependency; unpromotable dependents block the dependency, incl. no re-promotion of coupling-blocked packages).
- Apply plan construction (repo/aur split, blocked filtering, root vs. sudo, configured helper program, `AurHelper::None` + AUR upgrade is an error) with a recording `Executor`.
- Verbosity mapping (`-q`/default/`-v`/`-vv` -> error/warn/info/debug) and quiet/verbose conflict.
- Progress bar: verbosity gating (default level + TTY only), bar rendering proportions, label truncation.
- Table coloring: green/red AGE relative to `min_age_days`, verdict colors, ANSI codes never counted in column widths. Colors follow `--color auto|always|never` (auto = TTY + `NO_COLOR` honored); JSON output is never colored.
- Blocked-package hint: shown after the summary whenever anything is blocked; points at `always_allow` (the whitelist) with the live config path, and suggests `aur_heuristic` when blocked AUR packages have unknown age.
- CLI surface via the compiled binary (`tests/cli.rs`), environment-independent. The `--clear-cache` test redirects `XDG_CACHE_HOME` to a temp dir — never let binary tests touch the real user cache.

Tests must pass without pacman, an AUR helper, or network access. Do not add tests that depend on the host system state.

## Security Considerations

- `--apply` runs `sudo pacman -S <names>` / `<aur-helper> -S <names>` via `std::process::Command` with argv arrays — no shell. Package names are validated against `[a-z0-9@._+-]` in `apply::is_valid_package_name` before planning. When already running as root, the plan uses plain `pacman -S` (no sudo dependency).
- Running the tool as root triggers a warning: analysis needs no privileges and elevation happens only inside `--apply`. Root is *not* hard-rejected (containers commonly run as root).
- Dry-run is the default; the real upgrade path requires explicit `--apply`.
- All network responses and DB records are untrusted and parsed defensively (see Code Style).
- The cache stores only package names, versions, timestamps, and the publication basis — no user paths or secrets.

## Dependencies

Direct dependencies (see `Cargo.toml`): `clap` (CLI), `serde` + `serde_json` (JSON/cache/RPC), `toml` (config), `thiserror` (errors), `ureq` (blocking HTTP), `tar` + `flate2` + `zstd` (sync DB decoding). Add crates only when a requirement genuinely needs them.

## Deployment

The project is prepared for open-source publication:

- **Licensing:** dual `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE` at repo root and in the crate, kept byte-identical), declared in `Cargo.toml`; README carries the standard dual-license contribution clause.
- **Crate metadata:** `Cargo.toml` has description, rust-version (1.88, for edition-2024 let-chains), keywords/categories, and `exclude = ["/dist"]` keeping build output out of the crate. `cargo publish --dry-run` passes (~32 files). Since a git repo now exists, packaging requires committed files (or `--allow-dirty`).
- **Repository URL:** `Cargo.toml` `repository` is still a `YOUR-USERNAME` placeholder — replace it before publishing.
- **CI:** `.github/workflows/ci.yml` at the *repository root* (the only location GitHub reads) runs fmt, clippy (`-D warnings`), the full test suite, and a locked release build (binary uploaded as an artifact) on stable Rust, plus an MSRV `cargo check` on 1.88, on every push to any branch and on pull requests. All cargo steps use `working-directory: src` because the crate is not at the root. Tests run on `ubuntu-latest` by design (no pacman/paru needed).
- **Releases:** `.github/workflows/release.yml` runs on `v*` tags. It verifies the tag matches the `Cargo.toml` version, builds and strips the x86_64 Linux binary, attaches a tarball + sha256 to a GitHub Release (auto-generated notes), then publishes to the AUR: it renders `packaging/aur/PKGBUILD` with the tag version and the sha256 of the GitHub source tarball, regenerates `.SRCINFO` with `makepkg --printsrcinfo` in an `archlinux:base-devel` container, and pushes to `ssh://aur@aur.archlinux.org/pactience.git` using the `AUR_SSH_PRIVATE_KEY` repo secret. Release procedure: bump `version` in `src/Cargo.toml` and `pkgver` in `packaging/aur/PKGBUILD`, commit, then `git tag vX.Y.Z && git push --tags`.
- **READMEs:** the repo-root `README.md` is the GitHub face (may reference repo-level files like `console.example`); `src/README.md` is the crates.io face and must stay self-contained. Otherwise keep them in sync.

Not yet published to crates.io; the AUR package still needs to be registered (initial push to the AUR git repo, see "Releases" above) before the release workflow can update it.
