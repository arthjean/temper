#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{Fixture, stderr};

const EVIDENCE_DATE: &str = "2026-07-28";

#[test]
#[ignore = "writes retained PGO-hardening evidence when TEMPER_EVIDENCE_DIR is set"]
fn collect_retained_pgo_hardening_evidence() {
    let output_directory = PathBuf::from(
        std::env::var_os("TEMPER_EVIDENCE_DIR")
            .expect("TEMPER_EVIDENCE_DIR must name the new dated evidence directory"),
    );
    assert!(
        !output_directory.exists(),
        "refusing to overwrite retained evidence"
    );
    let staging = tempfile::tempdir().expect("evidence staging directory");
    let staged_evidence = staging.path().join(EVIDENCE_DATE);
    fs::create_dir(&staged_evidence).expect("staged evidence directory");
    let mut cases = Vec::new();

    let single = Fixture::single("dogfood-single", true, true);
    cases.push(run_and_capture(
        "single-binary",
        single,
        None,
        false,
        &[],
        "exec \"$TEMPER_BINARY\"\n",
        ExpectedOutcome::FullPgo,
        &staged_evidence,
    ));

    let string_flags = Fixture::single("dogfood-string-flags", true, true);
    cases.push(run_and_capture(
        "string-rustflags",
        string_flags,
        Some("[target.x86_64-unknown-linux-gnu]\nrustflags = \"-Cdebuginfo=1\"\n"),
        false,
        &[],
        "exec \"$TEMPER_BINARY\"\n",
        ExpectedOutcome::FullPgo,
        &staged_evidence,
    ));

    let workspace = Fixture::checked_in_fixture("pgo-workspace");
    cases.push(run_and_capture(
        "workspace-host-tools",
        workspace,
        Some(
            "[target.x86_64-unknown-linux-gnu]\nrustflags = \"--cfg temper_target --check-cfg=cfg(temper_target)\"\n",
        ),
        false,
        &["--package", "pgo-workspace-app", "--bin", "pgo-workspace-app"],
        "exec \"$TEMPER_BINARY\"\n",
        ExpectedOutcome::FullPgo,
        &staged_evidence,
    ));

    let array_flags = Fixture::nested_single("dogfood-array-spaces");
    cases.push(run_and_capture(
        "array-rustflags-space-path",
        array_flags,
        Some(
            "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"-Cdebuginfo=0\", \"--cfg\", \"temper_label=\\\"path with spaces\\\"\"]\n",
        ),
        true,
        &[],
        "exec \"$TEMPER_BINARY\"\n",
        ExpectedOutcome::FullPgo,
        &staged_evidence,
    ));

    let incompatible = Fixture::checked_in_fixture("pgo-missing-function");
    cases.push(run_and_capture(
        "incompatible-profile-control",
        incompatible,
        None,
        false,
        &[],
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  fixture_root=${0%/*}\n  cp \"$fixture_root/src/optimized.rs\" \"$fixture_root/src/main.rs\"\nfi\n",
        ExpectedOutcome::MissingProfileRejection,
        &staged_evidence,
    ));

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tested_commit = command_text(repository, "git", &["rev-parse", "HEAD"]);
    let implementation_tree_sha256 = implementation_tree_sha256(repository);
    let evidence_harness_tree_sha256 = evidence_harness_tree_sha256(repository);
    let tested_binary_sha256 = sha256_file(Path::new(env!("CARGO_BIN_EXE_cargo-temper")));
    let toolchain = toolchain_fingerprint(repository);
    fs::write(
        staged_evidence.join("summary.json"),
        serde_json::to_vec_pretty(&json!({
            "evidence_date": EVIDENCE_DATE,
            "classification": "correctness_only",
            "performance_claim": null,
            "tested_commit": tested_commit,
            "worktree_dirty": true,
            "implementation_tree_sha256": implementation_tree_sha256,
            "evidence_harness_tree_sha256": evidence_harness_tree_sha256,
            "tested_binary_sha256": tested_binary_sha256,
            "cases": cases,
            "toolchain": toolchain,
        }))
        .expect("serialize evidence summary"),
    )
    .expect("write evidence summary");
    fs::write(
        staged_evidence.join("README.md"),
        "# Temper v0.0.2 PGO-hardening evidence\n\n\
Date: 2026-07-28.\n\n\
This directory retains correctness evidence only. It makes no optimization-gain \
or production-performance claim. The four supported cases completed \
instrumentation, training, strict merge, optimized build and screening. The \
incompatible-profile control changed fixture source only after training and \
was rejected as `pgo_missing_profile_data` with zero PGO screening samples.\n\n\
Each case retains the exact fixture source, workload and raw `run.json`. \
`summary.json` records the host/toolchain identity, baseline Git commit, the \
dirty production-input checksum, evidence-harness checksum, exact tested-binary \
checksum and independent artifact/profile checksum rechecks performed before \
temporary build directories were released.\n",
    )
    .expect("write evidence README");

    fs::create_dir_all(
        output_directory
            .parent()
            .expect("dated evidence parent directory"),
    )
    .expect("evidence parent directory");
    copy_directory(&staged_evidence, &output_directory);
}

#[derive(Clone, Copy)]
enum ExpectedOutcome {
    FullPgo,
    MissingProfileRejection,
}

#[allow(clippy::too_many_arguments)]
fn run_and_capture(
    name: &str,
    mut fixture: Fixture,
    cargo_config: Option<&str>,
    spaced_path: bool,
    selection: &[&str],
    workload_body: &str,
    expected: ExpectedOutcome,
    evidence_root: &Path,
) -> Value {
    if spaced_path {
        let spaced_root = fixture
            .root
            .parent()
            .expect("fixture parent")
            .join("dogfood workspace with spaces");
        fs::rename(&fixture.root, &spaced_root).expect("move fixture under spaced path");
        fixture.root = spaced_root;
    }
    if let Some(config) = cargo_config {
        fs::create_dir(fixture.root.join(".cargo")).expect("Cargo config directory");
        fs::write(fixture.root.join(".cargo/config.toml"), config).expect("Cargo config");
    }
    let workload = fixture.root.join("dogfood-workload");
    fs::write(&workload, format!("#!/bin/sh\nset -eu\n{workload_body}"))
        .expect("write dogfood workload");
    make_executable(&workload);

    let case_directory = evidence_root.join(name);
    let fixture_evidence = case_directory.join("fixture");
    fs::create_dir_all(&case_directory).expect("case evidence directory");
    copy_fixture_inputs(&fixture.root, &fixture_evidence);

    let output = optimize(&fixture, selection, &workload);
    assert!(output.status.success(), "{name}: {}", stderr(&output));
    let manifest = fixture.manifest();
    let pgo = manifest["strategies"]
        .as_array()
        .and_then(|strategies| strategies.iter().find(|record| record["identity"] == "pgo"))
        .expect("PGO strategy record");
    match expected {
        ExpectedOutcome::FullPgo => {
            assert_eq!(manifest["pgo_training"]["outcome"], "trained");
            assert_eq!(manifest["pgo_training"]["phase_parity"]["matched"], true);
            assert_eq!(pgo["build"]["outcome"], "built");
            assert!(
                pgo["screening"]["sample_durations_ns"]
                    .as_array()
                    .is_some_and(|samples| !samples.is_empty())
            );
        }
        ExpectedOutcome::MissingProfileRejection => {
            assert_eq!(manifest["pgo_training"]["outcome"], "trained");
            assert_eq!(pgo["rejection_reason"], "pgo_missing_profile_data");
            assert!(pgo["screening"].is_null());
            assert_ne!(manifest["selected_candidate"], "pgo");
        }
    }

    let audit = recheck_run_checksums(&manifest);
    let run_json = fixture.run_directory().join("run.json");
    fs::copy(run_json, case_directory.join("run.json")).expect("retain raw run manifest");
    fs::write(
        case_directory.join("audit.json"),
        serde_json::to_vec_pretty(&audit).expect("serialize case audit"),
    )
    .expect("write case audit");
    json!({
        "name": name,
        "outcome": match expected {
            ExpectedOutcome::FullPgo => "full_pgo",
            ExpectedOutcome::MissingProfileRejection => "pgo_missing_profile_data",
        },
        "artifact_checksums_rechecked": audit["artifact_checksums_rechecked"],
        "raw_profile_checksums_rechecked": audit["raw_profile_checksums_rechecked"],
        "merged_profile_checksums_rechecked": audit["merged_profile_checksums_rechecked"],
    })
}

fn optimize(fixture: &Fixture, selection: &[&str], workload: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(selection)
        .arg("--")
        .arg(workload);
    command.output().expect("run dogfood case")
}

fn recheck_run_checksums(manifest: &Value) -> Value {
    let mut artifact_count = 0_u64;
    let mut raw_profile_count = 0_u64;
    let mut merged_profile_count = 0_u64;
    walk_records(manifest, &mut |object| {
        if let (Some(path), Some(expected)) = (
            object.get("executable_path").and_then(Value::as_str),
            object.get("sha256").and_then(Value::as_str),
        ) {
            assert_eq!(sha256_file(Path::new(path)), expected);
            artifact_count += 1;
        }
        if let (Some(path), Some(expected)) = (
            object.get("path").and_then(Value::as_str),
            object.get("sha256").and_then(Value::as_str),
        ) && Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "profraw")
        {
            assert_eq!(sha256_file(Path::new(path)), expected);
            raw_profile_count += 1;
        }
        if let (Some(path), Some(expected)) = (
            object.get("profdata_path").and_then(Value::as_str),
            object.get("profdata_sha256").and_then(Value::as_str),
        ) {
            assert_eq!(sha256_file(Path::new(path)), expected);
            merged_profile_count += 1;
        }
    });
    assert!(
        artifact_count >= 3,
        "expected baseline and static artifacts"
    );
    assert!(
        raw_profile_count >= 1,
        "expected retained raw-profile evidence"
    );
    assert_eq!(merged_profile_count, 1, "expected one merged profile");
    json!({
        "artifact_checksums_rechecked": artifact_count,
        "raw_profile_checksums_rechecked": raw_profile_count,
        "merged_profile_checksums_rechecked": merged_profile_count,
        "all_rechecks_matched": true,
    })
}

fn walk_records(value: &Value, inspect: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match value {
        Value::Object(object) => {
            inspect(object);
            for child in object.values() {
                walk_records(child, inspect);
            }
        }
        Value::Array(array) => {
            for child in array {
                walk_records(child, inspect);
            }
        }
        _ => {}
    }
}

fn copy_fixture_inputs(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("fixture evidence directory");
    for entry in fs::read_dir(source)
        .expect("read fixture input")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("fixture input entries")
    {
        let name = entry.file_name();
        if [
            OsStr::new(".git"),
            OsStr::new(".temper"),
            OsStr::new("target"),
        ]
        .contains(&name.as_os_str())
        {
            continue;
        }
        let target = destination.join(&name);
        if entry.file_type().expect("fixture input type").is_dir() {
            copy_fixture_inputs(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture evidence");
        }
    }
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("create retained evidence directory");
    for entry in fs::read_dir(source)
        .expect("read staged evidence")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("staged evidence entries")
    {
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("staged evidence type").is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy retained evidence");
        }
    }
}

fn toolchain_fingerprint(repository: &Path) -> Value {
    let rustc = command_text(repository, "rustc", &["-Vv"]);
    let cargo = command_text(repository, "cargo", &["-Vv"]);
    let target_libdir = command_text(
        repository,
        "rustc",
        &[
            "--print",
            "target-libdir",
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
    );
    let llvm_profdata = Path::new(target_libdir.trim())
        .parent()
        .expect("target libdir parent")
        .join("bin/llvm-profdata");
    let llvm = command_text(
        repository,
        llvm_profdata.to_str().expect("llvm-profdata UTF-8 path"),
        &["--version"],
    );
    json!({
        "rustc": rustc,
        "cargo": cargo,
        "llvm_profdata_path": llvm_profdata,
        "llvm_profdata": llvm,
        "host": command_text(repository, "uname", &["-a"]),
    })
}

fn implementation_tree_sha256(repository: &Path) -> String {
    let mut paths = vec![PathBuf::from("Cargo.lock"), PathBuf::from("Cargo.toml")];
    collect_regular_paths(repository, &repository.join("src"), &mut paths);
    hash_relative_files(repository, paths)
}

fn evidence_harness_tree_sha256(repository: &Path) -> String {
    let mut paths = vec![
        PathBuf::from("tests/ep002_dogfood.rs"),
        PathBuf::from("tests/support/mod.rs"),
    ];
    collect_regular_paths(
        repository,
        &repository.join("tests/fixtures/pgo-missing-function"),
        &mut paths,
    );
    collect_regular_paths(
        repository,
        &repository.join("tests/fixtures/pgo-workspace"),
        &mut paths,
    );
    hash_relative_files(repository, paths)
}

fn collect_regular_paths(repository: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .expect("read evidence input directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("evidence input entries")
    {
        let file_type = entry.file_type().expect("evidence input type");
        if file_type.is_dir() {
            collect_regular_paths(repository, &entry.path(), paths);
        } else if file_type.is_file() {
            paths.push(
                entry
                    .path()
                    .strip_prefix(repository)
                    .expect("repository-relative evidence input")
                    .to_path_buf(),
            );
        }
    }
}

fn hash_relative_files(repository: &Path, mut paths: Vec<PathBuf>) -> String {
    paths.sort();
    let mut digest = Sha256::new();
    for path in paths {
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update(fs::read(repository.join(&path)).expect("read implementation file"));
    }
    format!("{:x}", digest.finalize())
}

fn command_text(directory: &Path, program: &str, arguments: &[&str]) -> String {
    let output = Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run evidence identity command");
    assert!(output.status.success(), "{program} identity failed");
    String::from_utf8(output.stdout)
        .expect("identity output UTF-8")
        .trim()
        .to_owned()
}

fn sha256_file(path: &Path) -> String {
    let mut file = fs::File::open(path).expect("open evidence file");
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).expect("read evidence file");
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    format!("{:x}", digest.finalize())
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("workload metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make workload executable");
}
