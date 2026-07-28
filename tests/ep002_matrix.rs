#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Fixture, stderr};

#[test]
fn supported_absent_rustflags_and_baseline_base_complete_pgo() {
    let fixture = Fixture::single("matrix-absent-baseline", true, true);
    let output = run_supported_case(&fixture, BaseStrategy::Baseline, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_supported_pgo(&fixture.manifest(), "baseline");
}

#[test]
fn supported_string_rustflags_workspace_and_host_tools_complete_pgo() {
    let fixture = Fixture::checked_in_fixture("pgo-workspace");
    fs::create_dir(fixture.root.join(".cargo")).expect("Cargo config directory");
    fs::write(
        fixture.root.join(".cargo/config.toml"),
        "[target.x86_64-unknown-linux-gnu]\nrustflags = \"--cfg temper_target --check-cfg=cfg(temper_target)\"\n",
    )
    .expect("string target rustflags");
    commit_fixture_change(&fixture.root);
    let output = run_supported_case(
        &fixture,
        BaseStrategy::Thin,
        &[
            "--package",
            "pgo-workspace-app",
            "--bin",
            "pgo-workspace-app",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_supported_pgo(&fixture.manifest(), "thin-lto");
}

#[test]
fn supported_array_rustflags_and_space_paths_complete_pgo() {
    let mut fixture = Fixture::nested_single("matrix-array-spaces");
    let spaced_root = fixture
        .root
        .parent()
        .expect("fixture parent")
        .join("workspace with spaces");
    fs::rename(&fixture.root, &spaced_root).expect("move fixture under spaced path");
    fixture.root = spaced_root;
    fs::create_dir(fixture.root.join(".cargo")).expect("Cargo config directory");
    fs::write(
        fixture.root.join(".cargo/config.toml"),
        "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", \"temper_target\", \"--cfg\", \"temper_label=\\\"path with spaces\\\"\"]\n",
    )
    .expect("array target rustflags");
    let output = run_supported_case(&fixture, BaseStrategy::Fat, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_supported_pgo(&fixture.manifest(), "fat-lto-cgu1");
}

#[test]
fn pgo_json_looking_stream_rejects_only_pgo() {
    assert_pgo_stream_rejection(PgoStreamMode::JsonLooking, "unrecognized_cargo_json");
}

#[test]
fn pgo_zero_executable_stream_rejects_only_pgo() {
    assert_pgo_stream_rejection(PgoStreamMode::Zero, "cargo_executable_missing");
}

#[test]
fn pgo_multiple_executable_stream_rejects_only_pgo() {
    assert_pgo_stream_rejection(PgoStreamMode::Multiple, "cargo_executable_ambiguous");
}

#[derive(Clone, Copy)]
enum PgoStreamMode {
    JsonLooking,
    Zero,
    Multiple,
}

impl PgoStreamMode {
    fn label(self) -> &'static str {
        match self {
            Self::JsonLooking => "json-looking",
            Self::Zero => "zero",
            Self::Multiple => "multiple",
        }
    }
}

#[derive(Clone, Copy)]
enum BaseStrategy {
    Baseline,
    Thin,
    Fat,
}

fn run_supported_case(
    fixture: &Fixture,
    base_strategy: BaseStrategy,
    selection: &[&str],
) -> Output {
    let tracked_before = tracked_input_sha256(&fixture.root);
    let workload = fixture.root.join("matrix-workload");
    let sleeps = match base_strategy {
        BaseStrategy::Baseline => ("0", "0.03", "0.06"),
        BaseStrategy::Thin => ("0.06", "0", "0.03"),
        BaseStrategy::Fat => ("0.06", "0.03", "0"),
    };
    fs::write(
        &workload,
        format!(
            "#!/bin/sh\nset -eu\ncase \"$TEMPER_BINARY\" in\n  */baseline/*) sleep {} ;;\n  */thin-lto/*) sleep {} ;;\n  */fat-lto-cgu1/*) sleep {} ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
            sleeps.0, sleeps.1, sleeps.2
        ),
    )
    .expect("write matrix workload");
    make_executable(&workload);

    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(selection)
        .arg("--")
        .arg(&workload);
    let output = command.output().expect("run supported PGO matrix case");
    assert_eq!(
        tracked_input_sha256(&fixture.root),
        tracked_before,
        "matrix case changed tracked source, manifests, or lockfile"
    );
    output
}

fn assert_supported_pgo(manifest: &Value, expected_base: &str) {
    assert_eq!(manifest["pgo_base_strategy"], expected_base);
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["pgo_training"]["phase_parity"]["matched"], true);
    assert_eq!(
        manifest["pgo_training"]["merge"]["arguments"][1],
        "--failure-mode=any"
    );
    let pgo = manifest["strategies"]
        .as_array()
        .and_then(|strategies| strategies.iter().find(|record| record["identity"] == "pgo"))
        .expect("PGO strategy record");
    assert_eq!(pgo["build"]["outcome"], "built");
    assert_eq!(
        pgo["screening"]["sample_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(7)
    );
    assert!(
        pgo["build"]["environment_overrides"][0]["arguments"]
            .as_array()
            .is_some_and(|arguments| {
                arguments
                    .iter()
                    .any(|argument| argument == "-Cllvm-args=-pgo-warn-missing-function")
            })
    );
}

fn assert_pgo_stream_rejection(mode: PgoStreamMode, expected_reason: &str) {
    let fixture = Fixture::single(&format!("matrix-pgo-stream-{}", mode.label()), true, true);
    let tracked_before = tracked_input_sha256(&fixture.root);
    let workload = fixture.root.join("matrix-stream-workload");
    fs::write(
        &workload,
        "#!/bin/sh\nset -eu\ncase \"$TEMPER_BINARY\" in\n  */target/baseline/*) sleep 0.03 ;;\n  */target/thin-lto/*) sleep 0.01 ;;\n  */target/fat-lto-cgu1/*) sleep 0.02 ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
    )
    .expect("write Cargo-stream workload");
    make_executable(&workload);
    let wrapper = pgo_stream_wrapper(&fixture.root, mode);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload)
        .output()
        .expect("run PGO Cargo-stream rejection");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        tracked_input_sha256(&fixture.root),
        tracked_before,
        "PGO Cargo-stream case changed tracked source, manifest, or lockfile"
    );

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["pgo_training"]["phase_parity"]["matched"], true);
    let strategies = manifest["strategies"].as_array().expect("strategy records");
    assert!(
        strategies[..2].iter().all(|strategy| {
            strategy["build"]["outcome"] == "built" && !strategy["screening"].is_null()
        }),
        "static candidates must remain eligible"
    );
    let pgo = strategies
        .iter()
        .find(|strategy| strategy["identity"] == "pgo")
        .expect("PGO strategy");
    assert_eq!(pgo["build_failure"]["reason"], expected_reason);
    assert_eq!(pgo["rejection_reason"], "pgo_build_failed");
    assert!(pgo["screening"].is_null());
    assert_ne!(manifest["selected_candidate"], "pgo");
}

fn pgo_stream_wrapper(root: &Path, mode: PgoStreamMode) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let injection = match mode {
        PgoStreamMode::JsonLooking => format!(
            "'{real_cargo}' \"$@\"\nstatus=$?\nprintf '%s\\n' '{{\"reason\":\"future-cargo-message\"}}'\nexit \"$status\""
        ),
        PgoStreamMode::Zero => {
            "printf '%s\\n' '{\"reason\":\"build-finished\",\"success\":true}'".to_owned()
        }
        PgoStreamMode::Multiple => format!(
            "'{real_cargo}' \"$@\" | awk '{{ print; if (index($0, \"\\\"reason\\\":\\\"compiler-artifact\\\"\") > 0) print }}'"
        ),
    };
    let wrapper = root.join(format!("pgo-cargo-stream-{}", mode.label()));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"${{CARGO_ENCODED_RUSTFLAGS:-}}\" in\n  *-Cprofile-use=*)\n    {injection}\n    ;;\n  *) exec '{real_cargo}' \"$@\" ;;\nesac\n"
        ),
    )
    .expect("write PGO Cargo-stream wrapper");
    make_executable(&wrapper);
    wrapper
}

fn commit_fixture_change(root: &Path) {
    let status = Command::new("git")
        .current_dir(root)
        .args(["add", "."])
        .status()
        .expect("stage fixture change");
    assert!(status.success());
    let status = Command::new("git")
        .current_dir(root)
        .args(["commit", "--quiet", "-m", "matrix configuration"])
        .status()
        .expect("commit fixture change");
    assert!(status.success());
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("workload metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make workload executable");
}

fn tracked_input_sha256(root: &Path) -> String {
    let mut paths = Vec::new();
    collect_input_paths(root, root, &mut paths);
    paths.sort();
    let mut digest = Sha256::new();
    for relative in paths {
        digest.update(relative.as_os_str().as_encoded_bytes());
        digest.update(fs::read(root.join(relative)).expect("read fixture input"));
    }
    format!("{:x}", digest.finalize())
}

fn collect_input_paths(root: &Path, directory: &Path, paths: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory)
        .expect("read fixture input directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("fixture input entries")
    {
        let name = entry.file_name();
        if [".git", ".temper", "target"].contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        if entry.file_type().expect("fixture input type").is_dir() {
            collect_input_paths(root, &entry.path(), paths);
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).expect("fixture-relative path");
        let include = relative.file_name().is_some_and(|name| {
            name == "Cargo.toml"
                || name == "Cargo.lock"
                || name == "build.rs"
                || name == "config.toml"
        }) || relative
            .components()
            .any(|component| component.as_os_str() == "src");
        if include {
            paths.push(relative.to_path_buf());
        }
    }
}
