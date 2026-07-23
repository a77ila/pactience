//! Integration tests for the CLI surface: argument parsing, exit codes.
//!
//! These run the compiled binary; they avoid paths that require pacman,
//! paru, or network access so they pass on any machine.

use std::process::Command;

fn pactience(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pactience"))
        .args(args)
        .output()
        .expect("binary must run")
}

#[test]
fn help_succeeds_and_lists_flags() {
    let out = pactience(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--apply",
        "--json",
        "--config",
        "--min-age-days",
        "--set-min-age",
        "--dependency-policy",
        "--aur-heuristic",
        "--allow-unknown",
        "--aur-helper",
        "--sources",
        "--verbose",
        "--quiet",
        "--summary-only",
        "--clear-cache",
    ] {
        assert!(stdout.contains(flag), "missing {flag} in --help output");
    }
}

#[test]
fn version_succeeds() {
    let out = pactience(&["--version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("pactience"));
}

#[test]
fn invalid_min_age_days_is_rejected() {
    let out = pactience(&["--min-age-days", "four"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid value"));
    // Same via the short flag.
    let out = pactience(&["-m", "four"]);
    assert!(!out.status.success());
}

#[test]
fn invalid_color_choice_is_rejected() {
    let out = pactience(&["--color", "rainbow"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("auto"));
    assert!(stderr.contains("always"));
    assert!(stderr.contains("never"));
}

#[test]
fn invalid_dependency_policy_is_rejected() {
    let out = pactience(&["--dependency-policy", "yolo"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dependency-respecting"));
    assert!(stderr.contains("strict-closure"));
}

#[test]
fn invalid_sources_choice_is_rejected() {
    let out = pactience(&["--sources", "everything"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("repo"));
    assert!(stderr.contains("aur"));
}

#[test]
fn unknown_flag_is_rejected() {
    let out = pactience(&["--definitely-not-a-flag"]);
    assert!(!out.status.success());
}

#[test]
fn summary_only_conflicts_with_json() {
    let out = pactience(&["--summary-only", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--summary-only"));
}

#[test]
fn set_min_age_writes_the_config_and_exits() {
    // XDG_CONFIG_HOME redirects the config to a temp dir so the real user
    // configuration is never touched.
    let dir = std::env::temp_dir().join(format!("aag-cli-setmin-{}", std::process::id()));
    let config_path = dir.join("pactience/config.toml");

    // Missing file: created from the template with the active setting.
    let out = Command::new(env!("CARGO_BIN_EXE_pactience"))
        .args(["--set-min-age", "9"])
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .expect("binary must run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("min_age_days = 9"), "stdout: {stdout}");
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("min_age_days = 9"));
    assert!(contents.contains("# min_age_days = 4"));

    // Existing file: the active line is replaced, not duplicated.
    let out = Command::new(env!("CARGO_BIN_EXE_pactience"))
        .args(["--set-min-age", "12"])
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .expect("binary must run");
    assert!(out.status.success());
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("min_age_days = 12"));
    assert!(!contents.contains("min_age_days = 9\n"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn set_min_age_conflicts_with_min_age_days() {
    let out = pactience(&["--set-min-age", "9", "--min-age-days", "2"]);
    assert!(!out.status.success());
}

#[test]
fn clear_cache_removes_cache_dir_and_exits() {
    // XDG_CACHE_HOME redirects the cache to a temp dir so the real user
    // cache is never touched.
    let dir = std::env::temp_dir().join(format!("aag-cli-clear-{}", std::process::id()));
    let cache_dir = dir.join("pactience");
    std::fs::create_dir_all(cache_dir.join("aur-git")).unwrap();
    std::fs::write(cache_dir.join("publications.json"), "{}").unwrap();

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_pactience"))
            .arg("--clear-cache")
            .env("XDG_CACHE_HOME", &dir)
            .output()
            .expect("binary must run")
    };

    let out = run();
    assert!(out.status.success());
    assert!(!cache_dir.exists());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cleared cache directory"),
        "stdout: {stdout}"
    );

    // Idempotent: clearing a missing cache still succeeds.
    let out = run();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("did not exist"));
    std::fs::remove_dir_all(&dir).ok();
}
