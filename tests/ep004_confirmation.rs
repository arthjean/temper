#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::Value;
use support::{Fixture, command, stderr, stdout};

#[test]
fn checked_in_workspace_resolves_real_cargo_artifacts_without_source_mutation() {
    let fixture = Fixture::checked_in_workspace();
    let output = fixture.optimize(&["--package", "beta", "--bin", "beta-tool"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["target"]["package_name"], "beta");
    assert_eq!(manifest["target"]["binary_name"], "beta-tool");
    assert!(
        manifest["confirmation"]["baseline_build"]["executable_path"]
            .as_str()
            .is_some_and(|path| path.contains("/target/confirmation/baseline/"))
    );
    let status = Command::new("git")
        .current_dir(&fixture.root)
        .args(["status", "--porcelain=v1"])
        .output()
        .expect("inspect fixture source");
    assert!(
        status.stdout.is_empty(),
        "Temper changed the checked-in fixture copy: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn aa_confirmation_is_a_successful_no_improvement_json_run() {
    let fixture = Fixture::single("aa-confirmation", true, true);
    let output = fixture.optimize_workload(
        &["--json"],
        &[
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("sleep 0.02"),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let json = stdout(&output);
    assert!(!json.contains('\u{1b}'));
    let report: Value = serde_json::from_str(&json).expect("one final JSON object");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["status"], "no_improvement");
    assert_eq!(report["final_decision"], "no_improvement");
    assert_eq!(
        report["confirmation"]["measurement"]["baseline_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert_eq!(
        report["confirmation"]["measurement"]["candidate_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert!(report["promotion"].is_null());
    assert!(fixture.latest().is_none());
    assert!(!fixture.run_directory().join("best/artifact").exists());
}

#[test]
fn confirmed_candidate_is_promoted_with_matching_identity_and_latest_pointer() {
    let fixture = Fixture::single("confirmed-promotion", true, true);
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*|*/target/baseline/*) sleep 0.05 ;; \
         *) sleep 0.01 ;; \
         esac",
    );
    let output =
        fixture.optimize_workload(&[], &[OsStr::new("/bin/sh"), OsStr::new("-c"), workload]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).lines().count() <= 25);
    assert!(!stdout(&output).contains('\u{1b}'));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "confirmed");
    assert_eq!(manifest["final_decision"], "confirmed");
    assert_eq!(manifest["confirmation"]["outcome"], "accepted");
    assert!(
        manifest["confirmation"]["measurement"]["confidence_interval_95"]["upper"]
            .as_f64()
            .zip(manifest["confirmation"]["measurement"]["threshold_ratio"].as_f64())
            .is_some_and(|(upper, threshold)| upper <= threshold)
    );
    assert_eq!(
        manifest["promotion"]["source_strategy"],
        manifest["confirmation"]["strategy"]
    );
    assert_eq!(
        manifest["promotion"]["source_sha256"],
        manifest["confirmation"]["candidate_build"]["sha256"]
    );
    assert_eq!(
        manifest["promotion"]["promoted_sha256"],
        manifest["confirmation"]["candidate_build"]["sha256"]
    );
    assert_eq!(
        manifest["strategies"].as_array().map(Vec::len),
        Some(3),
        "screening records remain after promotion"
    );

    let promoted = manifest["promotion"]["promoted_path"]
        .as_str()
        .map(std::path::Path::new)
        .expect("promoted path");
    assert!(promoted.starts_with(fixture.run_directory().join("best")));
    assert!(promoted.is_file());
    let source = manifest["promotion"]["source_path"]
        .as_str()
        .map(std::path::Path::new)
        .expect("source path");
    assert_eq!(
        fs::metadata(promoted)
            .expect("promoted metadata")
            .permissions()
            .mode()
            & 0o7777,
        fs::metadata(source)
            .expect("source metadata")
            .permissions()
            .mode()
            & 0o7777
    );

    let latest = fixture.latest().expect("latest pointer");
    assert_eq!(latest["run_id"], manifest["run_id"]);
    assert_eq!(
        latest["artifact_path"],
        manifest["promotion"]["promoted_path"]
    );
    assert_eq!(
        latest["artifact_sha256"],
        manifest["promotion"]["promoted_sha256"]
    );
}

#[test]
fn candidate_confirmation_failure_rejects_without_promotion() {
    let fixture = Fixture::single("confirmation-failure", true, true);
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*) sleep 0.01 ;; \
         */confirmation/*) exit 7 ;; \
         *) sleep 0.01 ;; \
         esac",
    );
    let output =
        fixture.optimize_workload(&[], &[OsStr::new("/bin/sh"), OsStr::new("-c"), workload]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "no_improvement");
    assert_eq!(
        manifest["confirmation"]["workload_failure"]["outcome"],
        "nonzero_exit"
    );
    assert_eq!(
        manifest["confirmation"]["rejection_reason"],
        "confirmation_workload_failed"
    );
    let selected = manifest["confirmation"]["strategy"]
        .as_str()
        .expect("selected strategy");
    assert!(
        manifest["strategies"]
            .as_array()
            .expect("strategies")
            .iter()
            .find(|strategy| strategy["identity"] == selected)
            .is_some_and(|strategy| strategy["rejection_reason"] == "confirmation_rejected")
    );
    assert!(manifest["promotion"].is_null());
    assert!(fixture.latest().is_none());
}

#[test]
fn failed_runs_still_emit_schema_v2_and_preflight_json_is_single_object() {
    let fixture = Fixture::single("nonzero-workload", true, true);
    let output = fixture.optimize_workload(&[], &[OsStr::new("/bin/false")]);
    assert_eq!(output.status.code(), Some(1));
    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["final_decision"], "failed");
    assert_eq!(manifest["failure"]["outcome"], "nonzero_exit");

    let missing_lock = Fixture::single("json-preflight", true, false);
    let output = command()
        .current_dir(&missing_lock.root)
        .args(["temper", "optimize", "--json", "--manifest-path"])
        .arg(missing_lock.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run JSON preflight failure");
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).expect("one preflight JSON object");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["status"], "failed");
    assert!(!stdout(&output).contains('\u{1b}'));
}
