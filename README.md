# pactience

*Teach your pacman patience.*

> This project is being built using [Kimi 3](https://www.moonshot.cn/) (Kimi Code CLI).

A Rust CLI for Arch Linux that enforces a **minimum package age** policy before
upgrading. Freshly published packages occasionally ship regressions;
`pactience` looks up when each pending upgrade was *actually published* and
only lets through versions that have aged past a configurable threshold
(default: 4 days) — while keeping dependency consistency intact.

Dry-run by default: it reports what it would do and changes nothing unless you
pass `--apply`.

## How it decides

1. **Discovery** — parses `pacman -Qu` (repo packages) and the configured AUR
   helper's `-Qua` output (paru or yay; AUR).
2. **Publication dates** — per candidate version, resolved and cached:
   - *repo packages*: the [Arch Linux Archive](https://archive.archlinux.org)
     package pool (authoritative: when the version first appeared), falling
     back to the sync database `%BUILDDATE%`;
   - *AUR packages*: the package's **git history** (the commit that introduced
     the exact `pkgver-pkgrel`, via `.SRCINFO`), optionally falling back to the
     RPC `LastModified` heuristic. Bare clones are kept under
     `~/.cache/pactience/aur-git/` and only fetched afterwards.
3. **Age policy** — versions older than `min_age_days` are allowed, younger
   ones blocked. Unknown ages block by default.
4. **Dependency safety** — if an allowed upgrade needs a younger dependency:
   - `dependency-respecting` (default): the dependency is *promoted* into the
     upgrade set (consistency beats age), unless it is `always_block`-listed;
   - `strict-closure`: the dependent is blocked instead, transitively.
5. **Apply** — with `--apply`, the allowed set is installed via
   `sudo pacman -S` / `<aur-helper> -S` (no shell, validated package names).

Every verdict carries a reason in the output, so decisions are auditable.

## Installation

Requires an Arch Linux system with `pacman`. AUR packages are handled through
an AUR helper — `paru` or `yay`, chosen on first run (or via `aur_helper` /
`--aur-helper`; `none` disables AUR handling). Build from source:

```bash
cargo install --path . --locked
```

or build a release binary:

```bash
make dist        # produces dist/pactience
```

## Usage

```bash
pactience                 # dry-run report (table)
pactience -m 7            # require a 7-day minimum age
pactience --apply         # install the allowed set
pactience --json          # machine-readable report
pactience -v              # one info line per action (-vv for debug)
pactience --color always  # force colored output (auto/always/never)
pactience --summary-only  # summary line only, no table (CI-friendly)
pactience --clear-cache   # wipe ~/.cache/pactience and exit
```

```
PACKAGE            SRC   INSTALLED  CANDIDATE   PUBLISHED   AGE  VERDICT  REASON
glibc              repo  2.42-1     2.43+r37-1  2026-06-25  23d  allow    published 23d ago, meets 4d minimum (archive)
webkit2gtk-4.1     repo  2.52.4-1   2.52.5-2    2026-07-14   3d  block    published 3d ago, below 4d minimum (archive)
paru               aur   2.0.0-1    2.1.0-2     2025-12-12  218d allow    published 218d ago, meets 4d minimum (aur-git)

3 upgrade(s): 2 allowed, 1 blocked
hint: 1 package(s) blocked; whitelist trusted packages via always_allow in ~/.config/pactience/config.toml
```

Notable flags: `--dependency-policy`, `--aur-heuristic`, `--no-aur-git`,
`--aur-helper`, `--allow-unknown`, `--config`, `-q/--quiet`, `--summary-only`,
`--clear-cache`. See `--help`.

## Configuration

On first run you are asked which AUR helper to use (`paru`, `yay`, or `none`;
non-interactive runs auto-detect from PATH, falling back to `paru`) and a
fully commented template plus your choice is written to
`~/.config/pactience/config.toml`. All options (defaults shown):

```toml
# min_age_days = 4
# always_allow = ["linux"]            # always upgrade, regardless of age
# always_block = ["glibc"]            # never upgrade, never promote
# dependency_policy = "dependency-respecting"   # or "strict-closure"
# cache_ttl_secs = 86400              # TTL for "unknown age" cache entries
# allow_unknown = false               # allow packages with unknown age
# aur_heuristic = false               # gate AUR by RPC LastModified
# aur_git = true                      # accurate AUR dates from git history
# aur_helper = "paru"                 # "paru", "yay", or "none"
```

CLI flags override the file. Publication results are cached in
`~/.cache/pactience/` (positive results never expire — a version's
publication date is a historical fact; only "unknown" results expire).
`pactience --clear-cache` removes the whole cache directory (including the
AUR git clones) and exits.

## Safety

- Dry-run is the default; `--apply` is the only path that changes the system.
- Commands are spawned as argv arrays (no shell); package names are validated.
- Running as root prints a warning — analysis needs no privileges; elevation
  happens only inside `--apply` (and `sudo` is dropped when already root).
- Blocked upgrades explain *why*: age below minimum, unknown publication,
  `always_block`, or an unsatisfiable dependency. The tool never performs
  partial upgrades: anything whose dependency requirements cannot be satisfied
  by the installed system plus the allowed set is blocked.

## Development

```bash
cargo test                              # unit + CLI integration tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

The test suite runs anywhere — no pacman, AUR helper, network, or root
required;
process execution, HTTP, and the apply step sit behind injectable traits. CI
runs the same checks on every push and pull request.

`AGENTS.md` documents the architecture for AI coding agents; `INSTRUCTIONS.md`
is the original design brief.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
