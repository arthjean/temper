#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Fixture, stderr, stdout};

#[test]
fn builds_and_records_a_single_clean_baseline_without_source_mutation() {
    let fixture = Fixture::single("single-fixture", true, true);
    let output = fixture.optimize(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Temper 0.0.1 schema 2 (experimental)"));

    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(manifest["experimental"], true);
    assert_ne!(manifest["final_decision"], "pending");
    assert_eq!(manifest["source_reproducibility"], "clean");
    assert_eq!(manifest["target"]["package_name"], "single-fixture");
    assert_eq!(manifest["target"]["binary_name"], "single-fixture");
    assert_eq!(
        manifest["preflight"]["host_triple"],
        "x86_64-unknown-linux-gnu"
    );
    for key in [
        "kernel",
        "cpu_model",
        "logical_core_count",
        "cargo_version",
        "rustc_version",
        "manifest_path",
        "lockfile_sha256",
    ] {
        assert!(
            manifest["preflight"].get(key).is_some(),
            "missing preflight field: {key}"
        );
    }

    let executable = manifest["baseline"]["executable_path"]
        .as_str()
        .map(std::path::Path::new)
        .expect("baseline executable path");
    let bytes = fs::read(executable).expect("read baseline executable");
    assert_eq!(
        manifest["baseline"]["sha256"],
        format!("{:x}", Sha256::digest(bytes))
    );
    assert_eq!(
        manifest["baseline"]["size_bytes"],
        fs::metadata(executable).expect("baseline metadata").len()
    );
    assert!(executable.starts_with(fixture.root.join(".temper/runs")));
    assert!(manifest["baseline"]["bounded_diagnostics"].is_string());
    assert_eq!(
        manifest["baseline_measurement"]["sample_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(7)
    );

    let status = Command::new("git")
        .current_dir(&fixture.root)
        .args(["status", "--porcelain=v1"])
        .output()
        .expect("inspect fixture source");
    assert!(
        status.stdout.is_empty(),
        "Temper changed tracked fixture files: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn rejects_dirty_sources_by_default_and_records_an_allowed_dirty_run() {
    let fixture = Fixture::single("dirty-fixture", true, true);
    fs::write(
        fixture.root.join("src/main.rs"),
        "fn main() { println!(\"dirty fixture\"); }\n",
    )
    .expect("dirty fixture source");

    let rejected = fixture.optimize(&[]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(stderr(&rejected).contains("Commit or stash changes, or pass --allow-dirty."));
    assert!(!fixture.root.join(".temper").exists());

    let allowed = fixture.optimize(&["--allow-dirty"]);
    assert!(allowed.status.success(), "{}", stderr(&allowed));
    assert_eq!(
        fixture.manifest()["source_reproducibility"],
        Value::String("dirty".to_owned())
    );
}

#[test]
fn resolves_explicit_workspace_identity_and_rejects_ambiguity_before_build() {
    let fixture = Fixture::workspace();
    let ambiguous = fixture.optimize(&[]);
    assert_eq!(ambiguous.status.code(), Some(1));
    let diagnostic = stderr(&ambiguous);
    assert!(diagnostic.contains("Select one target with --package and --bin."));
    assert!(diagnostic.contains("alpha/alpha"));
    assert!(diagnostic.contains("beta/beta-tool"));
    assert!(!fixture.root.join(".temper").exists());

    let selected = fixture.optimize(&["--package", "beta", "--bin", "beta-tool"]);
    assert!(selected.status.success(), "{}", stderr(&selected));
    let manifest = fixture.manifest();
    assert_eq!(manifest["target"]["package_name"], "beta");
    assert_eq!(manifest["target"]["binary_name"], "beta-tool");
    assert!(
        manifest["target"]["package_id"]
            .as_str()
            .is_some_and(|identity| identity.contains("beta"))
    );
}

#[test]
fn rejects_cross_targets_and_ignores_nested_workspace_run_output() {
    let cross_target = Fixture::single("cross-target", true, true);
    let rejected = cross_target.optimize(&["--target", "aarch64-unknown-linux-gnu"]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(
        stderr(&rejected)
            .contains("Temper v0.0.1 supports only x86_64-unknown-linux-gnu host binaries")
    );
    assert!(!cross_target.root.join(".temper").exists());

    let nested = Fixture::nested_single("nested-fixture");
    let first = nested.optimize(&[]);
    assert!(first.status.success(), "{}", stderr(&first));
    let second = nested.optimize(&[]);
    assert!(second.status.success(), "{}", stderr(&second));
}

#[test]
fn prerequisite_and_baseline_failures_stop_before_later_phases() {
    let missing_lock = Fixture::single("missing-lock", true, false);
    let missing_lock_output = missing_lock.optimize(&[]);
    assert_eq!(missing_lock_output.status.code(), Some(1));
    assert!(
        stderr(&missing_lock_output).contains("Temper v0.0.1 requires an existing Cargo.lock.")
    );
    assert!(!missing_lock.root.join(".temper").exists());

    let missing_cargo = Fixture::single("missing-cargo", true, true);
    let missing_cargo_output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&missing_cargo.root)
        .env("CARGO", "/definitely/not/a/cargo")
        .args([
            "temper",
            "optimize",
            "--manifest-path",
            missing_cargo
                .root
                .join("Cargo.toml")
                .to_str()
                .expect("UTF-8 fixture path"),
            "--",
            "/bin/true",
        ])
        .output()
        .expect("run missing Cargo case");
    assert_eq!(missing_cargo_output.status.code(), Some(1));
    assert!(stderr(&missing_cargo_output).contains("Cargo executable is unavailable"));
    assert!(!missing_cargo.root.join(".temper").exists());

    let broken = Fixture::single("broken-fixture", false, true);
    let broken_output = broken.optimize(&[]);
    assert_eq!(broken_output.status.code(), Some(1));
    assert!(stderr(&broken_output).contains("Cargo build failed for baseline"));
    let manifest = broken.manifest();
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["failure"]["phase"], "baseline_build");
    assert!(manifest["baseline"].is_null());
}
