#![allow(clippy::unwrap_used, clippy::expect_used)]

//! v0.0.3 EP-003: schema-3 config provenance, observed compiler parity and
//! actionable fail-closed compiler decisions.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use support::{Fixture, stderr, stdout};

const SENTINEL: &str = "temper_included_sentinel";

// US-008: schema 3 versions the config graph and the compiler evidence.

#[test]
fn a_schema_three_run_publishes_observed_parity_and_the_config_graph() {
    let fixture = include_fixture();
    let output = optimize(&fixture, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 3);
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");

    // Every interposed build carries its versioned interposition identity.
    let reference = &manifest["pgo_training"]["reference_build"];
    let interposition = &reference["interposition"];
    assert_eq!(interposition["stage"], "pgo_reference");
    assert_eq!(interposition["protocol"], "temper-rustc-shim-1");
    assert_eq!(interposition["normalization"], 1);
    assert_eq!(
        interposition["shim_sha256"].as_str().map(str::len),
        Some(64)
    );
    assert!(
        interposition["real_rustc_version"]
            .as_str()
            .is_some_and(|version| version.starts_with("rustc "))
    );
    assert!(interposition["profile_path"].is_null());
    assert_eq!(
        interposition["injected_flags"].as_array().map(Vec::len),
        Some(0)
    );
    let evidence = &reference["compiler_evidence"];
    assert!(
        evidence["target_compilations"]
            .as_u64()
            .is_some_and(|n| n > 0)
    );
    assert_eq!(evidence["injected_invocations"], 0);
    assert_eq!(evidence["capture_digest"].as_str().map(str::len), Some(64));

    // The configuration graph records include provenance in load order.
    let graph = &manifest["pgo_training"]["prerequisites"]["config_graph"];
    assert_eq!(graph["include_supported"], true);
    assert_eq!(graph["declares_include"], true);
    assert!(
        graph["cargo_minor"]
            .as_u64()
            .is_some_and(|minor| minor >= 94)
    );
    let sources = fixture_sources(graph, &fixture.root);
    assert_eq!(
        sources
            .iter()
            .map(|source| file_name(&source["path"]))
            .collect::<Vec<_>>(),
        ["included.toml", "config.toml"],
        "included files must precede the file that includes them"
    );
    assert!(
        sources
            .iter()
            .all(|source| source["sha256"].as_str().map(str::len) == Some(64))
    );
    assert_eq!(sources[0]["discovery"], "include");
    let edges = sources[1]["includes"].as_array().expect("include edges");
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0]["declared"], "included.toml");
    assert_eq!(edges[0]["present"], true);
    assert_eq!(edges[0]["optional"], false);
    assert_eq!(edges[1]["declared"], "absent.toml");
    assert_eq!(edges[1]["present"], false);
    assert_eq!(
        edges[1]["optional"], true,
        "a missing optional include is recorded"
    );

    // US-009: observed parity is decided across all three executed stages.
    let parity = &manifest["compiler_parity"];
    assert_eq!(parity["matched"], true);
    assert_eq!(parity["scope"], "pgo_training");
    assert_eq!(parity["reason"], Value::Null);
    assert_eq!(parity["differences"].as_array().map(Vec::len), Some(0));
    assert_eq!(parity["differences_truncated"], false);
    let stages = parity["stages"].as_array().expect("parity stages");
    assert_eq!(
        stages
            .iter()
            .map(|stage| stage["stage"].as_str().expect("stage"))
            .collect::<Vec<_>>(),
        ["pgo_reference", "pgo_generate", "pgo_use"]
    );
    let aggregates: Vec<&str> = stages
        .iter()
        .map(|stage| {
            stage["normalized_aggregate_digest"]
                .as_str()
                .expect("aggregate digest")
        })
        .collect();
    assert!(
        aggregates.windows(2).all(|pair| pair[0] == pair[1]),
        "normalized aggregates must agree across stages: {aggregates:?}"
    );
    assert!(
        stages
            .iter()
            .all(|stage| stage["target_compilations"].as_u64().is_some_and(|n| n > 0))
    );
    assert_eq!(
        manifest["compiler_decisions"].as_array().map(Vec::len),
        Some(0)
    );
}

#[test]
fn compiler_input_environment_is_recorded_as_digests_without_values() {
    let fixture = Fixture::single("environment-digest", true, true);
    let sentinel_flags = "--cfg temper_env_sentinel --check-cfg=cfg(temper_env_sentinel)";
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("RUSTFLAGS", sentinel_flags)
        .env("CARGO_BUILD_RUSTFLAGS", "")
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/sleep", "0.02"])
        .output()
        .expect("run environment digest case");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    let inputs = manifest["pgo_training"]["prerequisites"]["config_graph"]["environment_inputs"]
        .as_array()
        .expect("environment inputs");
    let named = |name: &str| {
        inputs
            .iter()
            .find(|input| input["name"] == name)
            .expect("every compiler-input variable is recorded")
    };
    let rustflags = named("RUSTFLAGS");
    assert_eq!(rustflags["presence"], "set");
    assert_eq!(rustflags["sha256"].as_str().map(str::len), Some(64));
    // An empty value and an absent value stay distinguishable without a digest.
    let empty = named("CARGO_BUILD_RUSTFLAGS");
    assert_eq!(empty["presence"], "empty");
    assert_eq!(empty["sha256"], Value::Null);
    assert_eq!(named("RUSTC_WRAPPER")["presence"], "absent");

    let persisted = fs::read_to_string(fixture.run_directory().join("run.json")).expect("run.json");
    assert!(
        !persisted.contains("temper_env_sentinel"),
        "run.json published an unrestricted environment value"
    );
}

// US-009: observed drift rejects only PGO, after the optimized build.

#[test]
fn an_altered_use_stage_capture_rejects_only_pgo_with_a_compiler_input_mismatch() {
    let fixture = include_fixture();
    let wrapper = capture_argument_mutating_wrapper(&fixture.root);
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(fixture.root.join("run-candidate"))
        .output()
        .expect("run mutated capture case");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    let parity = &manifest["compiler_parity"];
    assert_eq!(parity["matched"], false);
    assert_eq!(parity["reason"], "compiler_input_mismatch");
    let classes: Vec<&str> = parity["differences"]
        .as_array()
        .expect("differences")
        .iter()
        .map(|difference| difference["class"].as_str().expect("class"))
        .collect();
    assert!(
        classes.contains(&"argument_added") && classes.contains(&"argument_count"),
        "unexpected difference classes: {classes:?}"
    );
    assert!(
        parity["differences"]
            .as_array()
            .expect("differences")
            .iter()
            .any(|difference| difference["crate_name"].as_str().is_some()),
        "a difference must name its affected logical crate"
    );

    // The optimized build is retained as evidence but never screened.
    let pgo = strategy(&manifest, "pgo");
    assert_eq!(pgo["rejection_reason"], "compiler_input_mismatch");
    assert_eq!(pgo["build"]["outcome"], "built");
    assert!(
        pgo["screening"].is_null(),
        "a mismatched PGO build was screened"
    );
    assert_ne!(manifest["selected_candidate"], "pgo");
    for identity in ["thin-lto", "fat-lto-cgu1"] {
        assert_eq!(
            strategy(&manifest, identity)["build"]["outcome"],
            "built",
            "{identity} was rejected by a PGO-only decision"
        );
    }

    // US-010: the decision is actionable and bounded.
    let decision = &manifest["compiler_decisions"][0];
    assert_eq!(decision["reason"], "compiler_input_mismatch");
    assert_eq!(decision["scope"], "pgo_only");
    assert_eq!(decision["stage"], "pgo_use");
    assert!(decision["crate_name"].as_str().is_some());
    assert!(
        decision["remediation"]
            .as_str()
            .is_some_and(|remediation| !remediation.is_empty())
    );
    assert!(
        decision["difference_classes"]
            .as_array()
            .is_some_and(|classes| !classes.is_empty())
    );
    assert!(
        stdout(&output).contains("compiler: reason=compiler_input_mismatch, scope=pgo_only"),
        "human report omitted the compiler decision:\n{}",
        stdout(&output)
    );
}

// US-010: a compiler override reachable only through `include` is named.

#[test]
fn a_wrapper_declared_only_in_an_included_config_rejects_only_pgo() {
    let fixture = Fixture::single("included-wrapper", true, true);
    let cargo = fixture.root.join(".cargo");
    fs::create_dir(&cargo).expect("Cargo config directory");
    fs::write(cargo.join("config.toml"), "include = [\"wrapper.toml\"]\n")
        .expect("including config");
    fs::write(
        cargo.join("wrapper.toml"),
        "[build]\nrustc-wrapper = \"/usr/bin/env\"\n",
    )
    .expect("included wrapper config");

    let output = fixture.optimize(&["--allow-dirty"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "unproven_compiler_override"
    );
    assert!(manifest["pgo_training"]["reference_build"].is_null());
    assert!(manifest["compiler_parity"].is_null());

    let decision = &manifest["compiler_decisions"][0];
    assert_eq!(decision["reason"], "unproven_compiler_override");
    assert_eq!(decision["scope"], "pgo_only");
    assert!(
        decision["source"]
            .as_str()
            .is_some_and(|source| source.ends_with("wrapper.toml")),
        "the decision must name the included source: {decision}"
    );
    assert!(
        decision["remediation"]
            .as_str()
            .is_some_and(|remediation| remediation.contains("build.rustc-wrapper"))
    );
    assert!(
        stdout(&output).contains("compiler: reason=unproven_compiler_override, scope=pgo_only"),
        "human report omitted the compiler decision:\n{}",
        stdout(&output)
    );
    for identity in ["thin-lto", "fat-lto-cgu1"] {
        assert_eq!(strategy(&manifest, identity)["build"]["outcome"], "built");
    }
}

#[test]
fn a_missing_required_include_never_reaches_a_promoted_artifact() {
    let fixture = Fixture::single("missing-include", true, true);
    let cargo = fixture.root.join(".cargo");
    fs::create_dir(&cargo).expect("Cargo config directory");
    fs::write(cargo.join("config.toml"), "include = [\"absent.toml\"]\n")
        .expect("broken including config");

    let output = fixture.optimize(&["--allow-dirty"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a broken include graph must fail closed: {}",
        stderr(&output)
    );
    assert!(
        fixture.latest().is_none(),
        "a broken run published a pointer"
    );
}

/// A workspace whose only source of a required rustflag is an included config,
/// plus a recorded missing optional include edge.
fn include_fixture() -> Fixture {
    let fixture = Fixture::single("schema-three-include", true, true);
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
    let workload = fixture.root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("workload");
    make_executable(&workload);
    fixture
}

fn optimize(fixture: &Fixture, extra_arguments: &[&str]) -> std::process::Output {
    let mut arguments = vec!["--allow-dirty"];
    arguments.extend_from_slice(extra_arguments);
    let workload = fixture.root.join("run-candidate");
    fixture.optimize_workload(&arguments, &[workload.as_os_str()])
}

/// A Cargo wrapper that appends one argument digest to a `pgo_use` target
/// capture after Cargo produced a valid artifact. The record stays structurally
/// valid, so only the observed parity comparison can reject it.
fn capture_argument_mutating_wrapper(root: &Path) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let wrapper = root.join("capture-argument-mutation");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntarget=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */target/pgo_use) target=$argument ;;\n  esac\ndone\n'{real_cargo}' \"$@\"\nstatus=$?\nif [ -n \"$target\" ] && [ \"$status\" -eq 0 ]; then\n  captures=\"${{target%/target/pgo_use}}/captures/pgo_use\"\n  record=$(grep -l '\"argument_digests\":\\[\"' \"$captures\"/*.json | head -n 1)\n  if [ -n \"$record\" ]; then\n    sed -i 's/\"argument_digests\":\\[\"/\"argument_digests\":[\"0000000000000000\",\"/' \"$record\"\n  fi\nfi\nexit \"$status\"\n"
        ),
    )
    .expect("write capture mutating wrapper");
    make_executable(&wrapper);
    wrapper
}

fn strategy<'a>(manifest: &'a Value, identity: &str) -> &'a Value {
    manifest["strategies"]
        .as_array()
        .expect("strategies")
        .iter()
        .find(|record| record["identity"] == identity)
        .expect("strategy record")
}

/// Only the fixture's own configuration sources. An ambient `CARGO_HOME`
/// configuration is legitimately discovered but is not under test here.
fn fixture_sources<'a>(graph: &'a Value, root: &Path) -> Vec<&'a Value> {
    let root = fs::canonicalize(root).expect("canonical fixture root");
    graph["sources"]
        .as_array()
        .expect("config sources")
        .iter()
        .filter(|source| {
            source["path"]
                .as_str()
                .is_some_and(|path| Path::new(path).starts_with(&root))
        })
        .collect()
}

fn file_name(path: &Value) -> String {
    Path::new(path.as_str().expect("source path"))
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned()
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}
