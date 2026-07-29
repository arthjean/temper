#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test assertions and matrix case labels"
)]

//! v0.0.3 EP-004 US-013: the retained include-based PGO evidence bundle.
//!
//! This collector is correctness evidence only. Its fixture is synthetic and
//! purpose-built to expose compiler inputs, so nothing it retains supports an
//! optimization-gain or production-representativeness claim.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{Fixture, assert_interposed_stage, stderr};

const SENTINEL: &str = "temper_included_sentinel";

/// The four interposed stages one confirmed include-based PGO run must record.
const INTERPOSED_STAGES: [(&str, &[&str], &str); 4] = [
    ("pgo_reference", &[], "reference_build"),
    (
        "pgo_generate",
        &["profile-generate"],
        "instrumentation_build",
    ),
    (
        "pgo_use",
        &["profile-use", "pgo-warn-missing-function"],
        "optimized_build",
    ),
    (
        "pgo_confirmation",
        &["profile-use", "pgo-warn-missing-function"],
        "confirmation_build",
    ),
];

#[test]
#[ignore = "runs one full include-based Temper search; writes retained v0.0.3 evidence when TEMPER_EVIDENCE_DIR is set"]
fn collect_v003_include_pgo_evidence() {
    let output_directory = PathBuf::from(
        std::env::var_os("TEMPER_EVIDENCE_DIR")
            .expect("TEMPER_EVIDENCE_DIR must name the new dated evidence directory"),
    );
    assert!(
        fs::symlink_metadata(&output_directory).is_err(),
        "refusing to overwrite retained evidence"
    );
    let evidence_date =
        std::env::var("TEMPER_EVIDENCE_DATE").expect("TEMPER_EVIDENCE_DATE must be set");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));

    let fixture = include_fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(fixture.root.join("run-candidate"))
        .output()
        .expect("run the include-based evidence case");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 3);
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["compiler_parity"]["matched"], true);
    assert_eq!(manifest["confirmation"]["strategy"], "pgo");
    assert_eq!(manifest["confirmation"]["compiler_parity"]["matched"], true);
    let training = &manifest["pgo_training"];
    assert_interposed_stage(&training["reference_build"], "pgo_reference", &[]);
    assert_interposed_stage(
        &training["instrumentation_build"],
        "pgo_generate",
        &["profile-generate"],
    );
    let optimized = manifest["strategies"]
        .as_array()
        .expect("strategies")
        .iter()
        .find(|record| record["identity"] == "pgo")
        .expect("PGO strategy record");
    assert_interposed_stage(
        &optimized["build"],
        "pgo_use",
        &["profile-use", "pgo-warn-missing-function"],
    );
    assert_interposed_stage(
        &manifest["confirmation"]["candidate_build"],
        "pgo_confirmation",
        &["profile-use", "pgo-warn-missing-function"],
    );

    fs::create_dir_all(&output_directory).expect("create evidence directory");
    fs::copy(
        fixture.run_directory().join("run.json"),
        output_directory.join("run.json"),
    )
    .expect("retain the raw run manifest");
    fs::write(output_directory.join("temper.stdout"), &output.stdout).expect("retain stdout");
    fs::write(output_directory.join("temper.stderr"), &output.stderr).expect("retain stderr");

    let fixture_directory = output_directory.join("fixture");
    fs::create_dir_all(&fixture_directory).expect("create fixture directory");
    let mut fixture_files = Vec::new();
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "src/main.rs",
        ".cargo/config.toml",
        ".cargo/included.toml",
        "run-candidate",
    ] {
        let source = fixture.root.join(relative);
        let destination = fixture_directory.join(relative);
        fs::create_dir_all(destination.parent().expect("fixture parent"))
            .expect("fixture parent directory");
        fs::copy(&source, &destination).expect("retain fixture source");
        fixture_files.push(json!({"path": relative, "sha256": sha256_file(&source)}));
    }

    let graph = &manifest["pgo_training"]["prerequisites"]["config_graph"];
    let stages: Vec<Value> = INTERPOSED_STAGES
        .iter()
        .map(|(stage, injected, label)| {
            let build = match *label {
                "reference_build" => &training["reference_build"],
                "instrumentation_build" => &training["instrumentation_build"],
                "optimized_build" => &optimized["build"],
                _ => &manifest["confirmation"]["candidate_build"],
            };
            json!({
                "stage": stage,
                "expected_injected_flags": injected,
                "target_compilations": build["compiler_evidence"]["target_compilations"],
                "host_compilations": build["compiler_evidence"]["host_compilations"],
                "probes": build["compiler_evidence"]["probes"],
                "injected_invocations": build["compiler_evidence"]["injected_invocations"],
                "capture_digest": build["compiler_evidence"]["capture_digest"],
                "artifact_sha256": build["sha256"],
            })
        })
        .collect();

    let summary = json!({
        "evidence_date": evidence_date,
        "classification": "correctness_only",
        "performance_claim": Value::Null,
        "workload": "synthetic sentinel-guarded fixture; the workload only executes the built binary",
        "workload_class": "synthetic",
        "representativeness_claim": Value::Null,
        "tested_commit": command_text(repository, "git", &["rev-parse", "HEAD"]),
        "worktree_dirty": !command_text(repository, "git", &["status", "--porcelain"]).is_empty(),
        "cargo": command_text(repository, "cargo", &["--version"]),
        "rustc": command_text(repository, "rustc", &["--version"]),
        "kernel": command_text(repository, "uname", &["-srm"]),
        "schema_version": manifest["schema_version"],
        "status": manifest["status"],
        "selected_candidate": manifest["selected_candidate"],
        "final_decision": manifest["final_decision"],
        "sentinel_cfg": SENTINEL,
        "fixture_files": fixture_files,
        "config_graph": {
            "cargo_minor": graph["cargo_minor"],
            "include_supported": graph["include_supported"],
            "declares_include": graph["declares_include"],
            "sources": graph["sources"],
        },
        "interposition": {
            "protocol": training["reference_build"]["interposition"]["protocol"],
            "normalization": training["reference_build"]["interposition"]["normalization"],
            "shim_sha256": training["reference_build"]["interposition"]["shim_sha256"],
            "real_rustc_version": training["reference_build"]["interposition"]["real_rustc_version"],
        },
        "stages": stages,
        "compiler_parity": manifest["compiler_parity"],
        "confirmation_parity": manifest["confirmation"]["compiler_parity"],
        "promoted_sha256": manifest["promotion"]["promoted_sha256"],
    });
    fs::write(
        output_directory.join("summary.json"),
        serde_json::to_vec_pretty(&summary).expect("serialize evidence summary"),
    )
    .expect("write evidence summary");
}

/// A fixture whose only source of a required target rustflag is an included
/// Cargo configuration file, and whose source fails to compile without it.
fn include_fixture() -> Fixture {
    let fixture = Fixture::single("v003-include-evidence", true, true);
    fs::write(
        fixture.root.join("src/main.rs"),
        format!(
            "#[cfg(not({SENTINEL}))]\ncompile_error!(\"the included target rustflag did not reach this compilation\");\n\nfn main() {{\n    println!(\"included sentinel\");\n}}\n"
        ),
    )
    .expect("write sentinel source");
    let cargo = fixture.root.join(".cargo");
    fs::create_dir(&cargo).expect("Cargo config directory");
    fs::write(
        cargo.join("config.toml"),
        "include = [\"included.toml\", { path = \"absent.toml\", optional = true }]\n",
    )
    .expect("write including config");
    fs::write(
        cargo.join("included.toml"),
        format!(
            "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", \"{SENTINEL}\", \"--check-cfg=cfg({SENTINEL})\"]\n"
        ),
    )
    .expect("write included config");
    // The path-dependent sleep manufactures the decision boundary so the run
    // reaches PGO confirmation. It is coverage evidence, never a measured gain.
    let workload = fixture.root.join("run-candidate");
    fs::write(
        &workload,
        "#!/bin/sh\ncase \"$TEMPER_BINARY\" in\n  */pgo_use/*|*/confirmation/pgo/*) sleep 0.03 ;;\n  *) sleep 0.15 ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
    )
    .expect("write workload");
    let mut permissions = fs::metadata(&workload)
        .expect("workload metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&workload, permissions).expect("make the workload executable");
    fixture
}

fn command_text(directory: &Path, program: &str, arguments: &[&str]) -> String {
    let output = Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("run {program}: {error}"));
    assert!(output.status.success(), "{program} failed");
    String::from_utf8(output.stdout)
        .expect("command output is UTF-8")
        .trim()
        .to_owned()
}

fn sha256_file(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).expect("read evidence input"));
    format!("{:x}", hasher.finalize())
}
