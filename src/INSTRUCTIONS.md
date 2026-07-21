# pactience Design Brief

## Overview

`pactience` is a CLI tool for Arch Linux that enforces a "minimum package age" policy before upgrading packages. It bases decisions on actual publication dates in Arch repositories and the Arch Linux Archive, not on local discovery time.

---

## Goals

Create a Rust CLI tool (`pactience`) that:
1. **Discovers Upgradable Packages:** Similar to `pacman -Qu` and `paru -Qua`.
2. **Determines Publication Time:** Fetches real publication dates from Arch repositories/Archive and AUR.
3. **Applies Age Policy:** Configurable threshold (e.g., 4 days) to allow or block upgrades.
4. **Ensures Dependency Safety:** Prevents partial upgrades that would break the system or violate `pacman`/`paru` constraints.
5. **Integrates with Pacman/Paru:** Optional `--apply` flag to perform the safe upgrade.
6. **Output:** Provides summary tables and machine-readable JSON.

---

## Constraints and Assumptions

- **Target OS:** Arch Linux.
- **Language:** Rust (stable).
- **Package Manager:** `pacman` and `paru` (for AUR).
- **Data Sources:** Real Arch resources (Archive, repo DBs, AUR RPC). No speculative data.
- **Strict Dependency Safety:** Mandatory system consistency. If age policy conflicts with dependencies, consistency wins (or depends on policy).
- **Transparency:** All decisions (age vs. dependency) must be clearly explained in the output.

---

## Functional Requirements

### 1. Discover Upgradable Packages
- Parse `pacman -Qu` and `paru -Qua`.
- Alternatively, read `/var/lib/pacman/sync/*.db` and query AUR RPC.
- Collect `(name, installed_version, candidate_version, source)`.

### 2. Determine Publication Timestamp
- Fetch first publication time for candidate versions.
- **Authoritative source:** The Arch Linux Archive (`/repos/YYYY/MM/DD/` daily snapshots). The earliest snapshot containing the candidate version is its publication date.
- **Trait-based Abstraction:** `PublicationSource` trait.
- **Implementations:**
  - `ArchivePublicationSource` — primary source; finds the first snapshot date for a version.
  - `RepoDbPublicationSource` — fallback using sync DB `%BUILDDATE%` when the Archive is unreachable or the version is too new.
  - `AurPublicationSource` — AUR RPC has no per-version build/publish timestamp; returns "unknown" by default. Optional heuristic mode may use `LastModified` with a warning.
- **Caching:** Local cache to avoid repeated network hits.

### 3. Age Evaluation and Policy
- Default threshold: 4 days.
- Behavior for unknown publication time: Block by default (configurable).

### 4. Dependency Safety (Critical)
- Build/obtain dependency graph from sync DBs and AUR metadata.
- **Logic:**
    - If A requires newer B, then:
        - Upgrade B (even if young) OR Block A.
    - If B is blocked, all packages depending on new B must also be blocked.
- **Policies:**
    - `dependency-respecting` (default): Auto-promote younger dependencies.
    - `strict-closure`: Block dependents instead of promoting dependencies.

### 5. Integration with Pacman/Paru
- `--apply`: Run `sudo pacman -S` or `paru -S` with the allowed set.
- Dry-run mode by default.
- Handle `pacman`/`paru` transaction validation.

### 6. Configuration
- `~/.config/pactience/config.toml`.
- Options: `min_age_days`, `always_allow`, `always_block`, `dependency_policy`, `cache_ttl`.

---

## Non-Functional Requirements

- **Code Quality:** Idiomatic Rust, clear separation of concerns, proper error handling (no `unwrap()`).
- **Performance:** Efficient caching and network usage.
- **Extensibility:** Support for future policies (e.g., security advisories).
- **Testability:** High coverage for parsing, age logic, and dependency resolution. Mocked sources for tests.

---

## Development Standards

This project must be developed to senior-engineer, production-grade standards. The codebase is expected to be clean, maintainable, and free of common anti-patterns.

- **No shortcuts:** Avoid hacky workarounds, placeholder logic, "temporary" code that lingers, or speculative abstractions.
- **No anti-patterns:** Avoid `unwrap()`/`expect()` in production paths, global mutable state, tight coupling to external services, and premature optimization.
- **Clean architecture:** Keep modules focused, boundaries explicit, and dependencies injectable. Favor composition over inheritance-like trait hierarchies.
- **Defensive programming:** Treat all external input (pacman output, DB records, AUR RPC responses, network payloads, config files) as untrusted and parse/validate it defensively.
- **Observability:** Errors must be actionable. Log or report the *why* behind decisions (age, dependency, policy) so users can audit them.
- **Review-ready code:** Every change should pass `cargo fmt`, `cargo clippy -- -D warnings`, and the full test suite without warnings.

---

## Development Process

1. **Phase 1: Project Setup** - `Cargo.toml`, basic CLI skeleton.
2. **Phase 2: Discovery** - Parse `pacman -Qu` and display candidates.
3. **Phase 3: Publication Info** - Integrate `PublicationSource` and caching.
4. **Phase 4: Dependency Graph** - Build graph and implement safety logic.
5. **Phase 5: Policy and Decision** - Combine age and dependency logic.
6. **Phase 6: Integration and UI** - `pacman` integration, JSON output, and final polish.
