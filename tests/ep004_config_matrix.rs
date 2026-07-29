#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test assertions and matrix case labels"
)]

//! v0.0.3 EP-004 US-011: the Cargo rustflags and `include` provenance matrix.
//!
//! Every supported case guards its own source with `compile_error!`, so a lost
//! compiler input is an executed build failure rather than an inspected field.
//! Every rejection case asserts the exact schema-3 reason code.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use support::{Fixture, argument_digest, assert_interposed_stage, capture_records, stderr};

const HOST: &str = "x86_64-unknown-linux-gnu";
const SENTINEL: &str = "temper_matrix_sentinel";
/// A flag a lower-precedence Cargo source declares. Observing it proves Cargo
/// did not select the source the case expects to win.
const OVERRIDDEN: &str = "temper_matrix_overridden";

const SENTINEL_ARGUMENTS: [&str; 3] =
    ["--cfg", SENTINEL, "--check-cfg=cfg(temper_matrix_sentinel)"];

/// The three interposed PGO training stages, in the order parity compares them.
const TRAINING_STAGES: [(&str, &[&str]); 3] = [
    ("pgo_reference", &[]),
    ("pgo_generate", &["profile-generate"]),
    ("pgo_use", &["profile-use", "pgo-warn-missing-function"]),
];

// Supported provenance: every documented rustflags source survives every phase.

#[test]
fn absent_rustflags_complete_pgo_with_no_declared_include() {
    let fixture = guarded_fixture("matrix-absent", &[], &[]);
    let manifest = assert_trained(&fixture, run(&fixture, &[]));

    let graph = &manifest["pgo_training"]["prerequisites"]["config_graph"];
    assert_eq!(graph["include_supported"], true);
    assert_eq!(graph["declares_include"], false);
    assert!(fixture_sources(graph, &fixture.root).is_empty());
}

#[test]
fn direct_string_and_array_target_rustflags_survive_every_phase() {
    for (label, table) in [
        (
            "string",
            format!(
                "[target.{HOST}]\nrustflags = \"--cfg {SENTINEL} --check-cfg=cfg({SENTINEL})\"\n"
            ),
        ),
        ("array", target_table(&SENTINEL_ARGUMENTS)),
    ] {
        let fixture = guarded_fixture(&format!("matrix-direct-{label}"), &[SENTINEL], &[]);
        write_configs(&fixture, &[("config.toml", &table)]);
        assert_trained(&fixture, run(&fixture, &[]));
        assert_ordered_across_training_stages(&fixture, &SENTINEL_ARGUMENTS);
    }
}

#[test]
fn required_and_nested_includes_survive_every_phase() {
    let required = guarded_fixture("matrix-required-include", &[SENTINEL], &[]);
    write_configs(
        &required,
        &[
            ("config.toml", "include = [\"flags.toml\"]\n"),
            ("flags.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    assert_trained(&required, run(&required, &[]));
    assert_ordered_across_training_stages(&required, &SENTINEL_ARGUMENTS);

    let nested = guarded_fixture("matrix-nested-include", &[SENTINEL], &[]);
    write_configs(
        &nested,
        &[
            ("config.toml", "include = [\"layer1.toml\"]\n"),
            ("layer1.toml", "include = [\"layer2.toml\"]\n"),
            ("layer2.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    let manifest = assert_trained(&nested, run(&nested, &[]));
    assert_ordered_across_training_stages(&nested, &SENTINEL_ARGUMENTS);

    // Included files load before the file that includes them, deepest first.
    let graph = &manifest["pgo_training"]["prerequisites"]["config_graph"];
    assert_eq!(
        source_names(graph, &nested.root),
        ["layer2.toml", "layer1.toml", "config.toml"]
    );
}

#[test]
fn a_missing_optional_include_is_recorded_and_the_required_flag_survives() {
    let fixture = guarded_fixture("matrix-optional-include", &[SENTINEL], &[]);
    write_configs(
        &fixture,
        &[
            (
                "config.toml",
                "include = [{ path = \"absent.toml\", optional = true }, \"flags.toml\"]\n",
            ),
            ("flags.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    let manifest = assert_trained(&fixture, run(&fixture, &[]));
    assert_ordered_across_training_stages(&fixture, &SENTINEL_ARGUMENTS);

    let graph = &manifest["pgo_training"]["prerequisites"]["config_graph"];
    let including = fixture_sources(graph, &fixture.root)
        .into_iter()
        .find(|source| file_name(&source["path"]) == "config.toml")
        .expect("the including config source");
    let edges = including["includes"].as_array().expect("include edges");
    assert_eq!(edges[0]["declared"], "absent.toml");
    assert_eq!(edges[0]["present"], false);
    assert_eq!(edges[0]["optional"], true);
    assert_eq!(edges[1]["declared"], "flags.toml");
    assert_eq!(edges[1]["present"], true);
    assert_eq!(edges[1]["optional"], false);
}

#[test]
fn target_cfg_and_build_rustflags_survive_every_phase() {
    for (label, table) in [
        (
            "cfg",
            format!(
                "[target.'cfg(target_os = \"linux\")']\nrustflags = [\"--cfg\", \"{SENTINEL}\", \"--check-cfg=cfg({SENTINEL})\"]\n"
            ),
        ),
        (
            "build",
            format!(
                "[build]\nrustflags = [\"--cfg\", \"{SENTINEL}\", \"--check-cfg=cfg({SENTINEL})\"]\n"
            ),
        ),
    ] {
        let fixture = guarded_fixture(&format!("matrix-{label}-rustflags"), &[SENTINEL], &[]);
        write_configs(&fixture, &[("config.toml", &table)]);
        assert_trained(&fixture, run(&fixture, &[]));
        assert_ordered_across_training_stages(&fixture, &SENTINEL_ARGUMENTS);
    }
}

#[test]
fn ambient_and_encoded_rustflags_survive_every_phase() {
    let ambient = guarded_fixture("matrix-ambient-rustflags", &[SENTINEL], &[]);
    assert_trained(
        &ambient,
        run(
            &ambient,
            &[(
                "RUSTFLAGS",
                &format!("--cfg {SENTINEL} --check-cfg=cfg({SENTINEL})"),
            )],
        ),
    );
    assert_ordered_across_training_stages(&ambient, &SENTINEL_ARGUMENTS);

    let encoded = guarded_fixture("matrix-encoded-rustflags", &[SENTINEL], &[]);
    assert_trained(
        &encoded,
        run(
            &encoded,
            &[(
                "CARGO_ENCODED_RUSTFLAGS",
                &SENTINEL_ARGUMENTS.join("\u{1f}"),
            )],
        ),
    );
    assert_ordered_across_training_stages(&encoded, &SENTINEL_ARGUMENTS);
}

// Precedence: Cargo selects exactly one source and Temper preserves its order.

#[test]
fn cargo_precedence_selects_one_source_and_preserves_its_order() {
    let overridden_table = format!(
        "[build]\nrustflags = [\"--cfg\", \"{OVERRIDDEN}\", \"--check-cfg=cfg({OVERRIDDEN})\"]\n"
    );

    // A matching target table wins over `build.rustflags`.
    let target_wins = guarded_fixture("matrix-precedence-target", &[SENTINEL], &[OVERRIDDEN]);
    write_configs(
        &target_wins,
        &[(
            "config.toml",
            &format!("{}{overridden_table}", target_table(&SENTINEL_ARGUMENTS)),
        )],
    );
    assert_trained(&target_wins, run(&target_wins, &[]));
    assert_ordered_across_training_stages(&target_wins, &SENTINEL_ARGUMENTS);

    // `RUSTFLAGS` wins over every configured source, including an included one.
    let ambient_wins = guarded_fixture("matrix-precedence-ambient", &[SENTINEL], &[OVERRIDDEN]);
    write_configs(
        &ambient_wins,
        &[
            ("config.toml", "include = [\"flags.toml\"]\n"),
            (
                "flags.toml",
                &format!(
                    "[target.{HOST}]\nrustflags = [\"--cfg\", \"{OVERRIDDEN}\", \"--check-cfg=cfg({OVERRIDDEN})\"]\n"
                ),
            ),
        ],
    );
    assert_trained(
        &ambient_wins,
        run(
            &ambient_wins,
            &[(
                "RUSTFLAGS",
                &format!("--cfg {SENTINEL} --check-cfg=cfg({SENTINEL})"),
            )],
        ),
    );
    assert_ordered_across_training_stages(&ambient_wins, &SENTINEL_ARGUMENTS);

    // `CARGO_ENCODED_RUSTFLAGS` wins over `RUSTFLAGS`.
    let encoded_wins = guarded_fixture("matrix-precedence-encoded", &[SENTINEL], &[OVERRIDDEN]);
    assert_trained(
        &encoded_wins,
        run(
            &encoded_wins,
            &[
                (
                    "CARGO_ENCODED_RUSTFLAGS",
                    &SENTINEL_ARGUMENTS.join("\u{1f}"),
                ),
                (
                    "RUSTFLAGS",
                    &format!("--cfg {OVERRIDDEN} --check-cfg=cfg({OVERRIDDEN})"),
                ),
            ],
        ),
    );
    assert_ordered_across_training_stages(&encoded_wins, &SENTINEL_ARGUMENTS);
}

// Host units never receive a phase control.

#[test]
fn build_scripts_and_proc_macros_receive_no_phase_control() {
    let fixture = Fixture::checked_in_fixture("pgo-workspace");
    write_configs(
        &fixture,
        &[(
            "config.toml",
            "[target.x86_64-unknown-linux-gnu]\nrustflags = \"--cfg temper_target --check-cfg=cfg(temper_target)\"\n",
        )],
    );
    let output = run_selected(
        &fixture,
        &[
            "--package",
            "pgo-workspace-app",
            "--bin",
            "pgo-workspace-app",
        ],
        &[],
    );
    assert_trained(&fixture, output);

    let run_directory = fixture.run_directory();
    for (stage, injected) in TRAINING_STAGES {
        let records = capture_records(&run_directory, stage);
        let mut target = 0;
        let mut host = 0;
        for record in &records {
            let observed = record["injected_flags"]
                .as_array()
                .expect("injected flag list")
                .iter()
                .map(|flag| flag.as_str().expect("injected flag"))
                .collect::<Vec<_>>();
            if record["class"] == "target_compile" {
                target += 1;
                assert_eq!(observed, injected, "{stage} target compilation");
            } else {
                host += usize::from(record["class"] == "host_compile");
                assert!(
                    observed.is_empty(),
                    "{stage} injected a phase control into a {} invocation",
                    record["class"]
                );
            }
        }
        // The selected binary plus its target dependency, and the build script
        // plus the proc macro on the host side.
        assert!(target >= 2, "{stage} observed {target} target compilations");
        assert!(host >= 2, "{stage} observed {host} host compilations");
    }
}

// Rejections: every failure names a stable reason before screening.

#[test]
fn an_include_shape_cargo_accepts_but_temper_cannot_prove_rejects_only_pgo() {
    // Cargo 1.97 silently accepts an unknown key inside an include table, so
    // this is Temper's own provenance boundary rather than a Cargo error.
    let fixture = guarded_fixture("matrix-malformed-include", &[SENTINEL], &[]);
    write_configs(
        &fixture,
        &[
            (
                "config.toml",
                "include = [{ path = \"flags.toml\", unsupported = 1 }]\n",
            ),
            ("flags.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    let output = run(&fixture, &[]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "cargo_config_include_malformed"
    );
    assert_eq!(manifest["pgo_training"]["failure_stage"], "prerequisites");
    assert!(manifest["pgo_training"]["reference_build"].is_null());
    assert!(manifest["compiler_parity"].is_null());
    assert_pgo_only_rejection(&manifest);
    assert_compiler_decision(&manifest, "cargo_config_include_malformed");
}

#[test]
fn cargo_rejected_include_graphs_fail_before_any_screening() {
    for (label, files) in [
        (
            "cycle",
            vec![
                ("config.toml", "include = [\"cycle-a.toml\"]\n".to_owned()),
                ("cycle-a.toml", "include = [\"cycle-b.toml\"]\n".to_owned()),
                ("cycle-b.toml", "include = [\"cycle-a.toml\"]\n".to_owned()),
            ],
        ),
        (
            "missing-required",
            vec![("config.toml", "include = [\"absent.toml\"]\n".to_owned())],
        ),
        (
            "not-a-list",
            vec![("config.toml", "include = \"flags.toml\"\n".to_owned())],
        ),
    ] {
        let fixture = guarded_fixture(&format!("matrix-broken-{label}"), &[], &[]);
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, contents)| (*name, contents.as_str()))
            .collect();
        write_configs(&fixture, &files);

        let output = run(&fixture, &[]);
        assert!(
            !output.status.success(),
            "{label} must fail closed: {}",
            stderr(&output)
        );
        assert!(
            fixture.latest().is_none(),
            "{label} published a success pointer"
        );
        assert!(
            !fixture.root.join(".temper/best").exists(),
            "{label} promoted an artifact"
        );
    }
}

#[test]
fn an_included_config_change_between_phases_rejects_before_screening() {
    let fixture = guarded_fixture("matrix-include-drift", &[SENTINEL], &[]);
    write_configs(
        &fixture,
        &[
            ("config.toml", "include = [\"flags.toml\"]\n"),
            ("flags.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    let drifted = target_table(&[
        "--cfg",
        SENTINEL,
        "--check-cfg=cfg(temper_matrix_sentinel)",
        "--cfg",
        "temper_matrix_drift",
        "--check-cfg=cfg(temper_matrix_drift)",
    ]);
    let wrapper = config_drift_wrapper(&fixture.root, &drifted);

    let output = run_selected(
        &fixture,
        &[],
        &[("CARGO", wrapper.to_str().expect("wrapper"))],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    // The optimized stage re-proves its prerequisites, so a config source that
    // changed during training is rejected before the optimized build exists.
    let manifest = fixture.manifest();
    let training = &manifest["pgo_training"];
    assert_eq!(training["outcome"], "rejected");
    assert_eq!(training["rejection_reason"], "pgo_phase_parity_mismatch");
    assert_eq!(training["failure_stage"], "prerequisites");
    assert_eq!(training["phase_parity"]["matched"], false);
    assert_eq!(
        training["phase_parity"]["unexpected_differences"],
        serde_json::json!(["config_graph"]),
        "the drifted input class must be named"
    );
    // No observed parity object may claim a match without complete evidence.
    assert!(manifest["compiler_parity"].is_null());
    assert_pgo_only_rejection(&manifest);
}

#[test]
fn direct_split_form_and_included_profiling_controls_reject_only_pgo() {
    for label in ["direct", "split-form", "included"] {
        let fixture = guarded_fixture(&format!("matrix-conflict-{label}"), &[], &[]);
        // The conflicting control must stay valid for the uninterposed baseline
        // and static builds, so only the PGO boundary decides the outcome.
        let profiles = fixture.root.join("conflict-profiles");
        let generate = format!("-Cprofile-generate={}", profiles.display());
        let files: Vec<(&str, String)> = match label {
            "direct" => vec![("config.toml", target_table(&[&generate]))],
            "split-form" => vec![(
                "config.toml",
                target_table(&["-C", &format!("profile-generate={}", profiles.display())]),
            )],
            _ => vec![
                ("config.toml", "include = [\"flags.toml\"]\n".to_owned()),
                ("flags.toml", target_table(&["-Cinstrument-coverage"])),
            ],
        };
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, contents)| (*name, contents.as_str()))
            .collect();
        write_configs(&fixture, &files);

        let output = run(&fixture, &[]);
        assert!(output.status.success(), "{label}: {}", stderr(&output));

        let manifest = fixture.manifest();
        assert_eq!(
            manifest["pgo_training"]["rejection_reason"], "pgo_compiler_input_conflict",
            "{label} did not reject the conflicting compiler input"
        );
        assert_eq!(manifest["pgo_training"]["failure_stage"], "reference");
        assert_pgo_only_rejection(&manifest);
        assert_compiler_decision(&manifest, "pgo_compiler_input_conflict");

        // The shim refused before replacing itself with the real compiler, so
        // no conflicting target compilation ever executed.
        let rejected: Vec<Value> = capture_records(&fixture.run_directory(), "pgo_reference")
            .into_iter()
            .filter(|record| record["rejection"] == "pgo_compiler_input_conflict")
            .collect();
        assert!(
            !rejected.is_empty(),
            "{label} persisted no rejected capture record"
        );
        for record in &rejected {
            assert_eq!(record["class"], "target_compile");
            assert_eq!(record["injected_flags"].as_array().map(Vec::len), Some(0));
        }
    }
}

// Confirmation reuses the same observed compiler-input contract.

#[test]
fn an_included_rustflag_survives_through_confirmation() {
    let fixture = guarded_fixture("matrix-confirmation-include", &[SENTINEL], &[]);
    write_configs(
        &fixture,
        &[
            ("config.toml", "include = [\"flags.toml\"]\n"),
            ("flags.toml", &target_table(&SENTINEL_ARGUMENTS)),
        ],
    );
    // The workload is deliberately slower for every non-PGO artifact so the
    // bounded search reaches PGO confirmation. It manufactures the decision
    // boundary and is evidence of interposition coverage only, never of a gain.
    let workload = fixture.root.join("run-candidate");
    fs::write(
        &workload,
        "#!/bin/sh\ncase \"$TEMPER_BINARY\" in\n  */pgo_use/*|*/confirmation/pgo/*) sleep 0.03 ;;\n  *) sleep 0.15 ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
    )
    .expect("write confirmation workload");
    make_executable(&workload);

    let output = run(&fixture, &[]);
    let manifest = assert_trained(&fixture, output);
    assert_eq!(manifest["confirmation"]["strategy"], "pgo");

    let candidate = &manifest["confirmation"]["candidate_build"];
    assert_interposed_stage(
        candidate,
        "pgo_confirmation",
        &["profile-use", "pgo-warn-missing-function"],
    );
    assert_eq!(manifest["confirmation"]["compiler_parity"]["matched"], true);
    assert_eq!(
        manifest["confirmation"]["compiler_parity"]["scope"],
        "pgo_confirmation"
    );

    // The included sentinel keeps its resolved order in the confirmation build.
    let record = sole_target_record(&fixture.run_directory(), "confirmation/pgo");
    assert_consecutive(&record, &SENTINEL_ARGUMENTS, "confirmation");
}

/// A single-binary fixture whose source only compiles when every required cfg
/// reached the target compilation and no overridden cfg did.
fn guarded_fixture(name: &str, required: &[&str], overridden: &[&str]) -> Fixture {
    let fixture = Fixture::single(name, true, true);
    let mut source = String::new();
    for cfg in required {
        source.push_str(&format!(
            "#[cfg(not({cfg}))]\ncompile_error!(\"a resolved target rustflag did not reach this compilation\");\n"
        ));
    }
    for cfg in overridden {
        source.push_str(&format!(
            "#[cfg({cfg})]\ncompile_error!(\"Cargo did not select the expected rustflags source\");\n"
        ));
    }
    source.push_str("fn main() {\n    println!(\"matrix\");\n}\n");
    fs::write(fixture.root.join("src/main.rs"), source).expect("write guarded source");
    let workload = fixture.root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("write workload");
    make_executable(&workload);
    fixture
}

fn target_table(flags: &[&str]) -> String {
    let list = flags
        .iter()
        .map(|flag| format!("\"{flag}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[target.{HOST}]\nrustflags = [{list}]\n")
}

fn write_configs(fixture: &Fixture, files: &[(&str, &str)]) {
    let directory = fixture.root.join(".cargo");
    fs::create_dir_all(&directory).expect("Cargo config directory");
    for (name, contents) in files {
        fs::write(directory.join(name), contents).expect("write Cargo config source");
    }
}

fn run(fixture: &Fixture, environment: &[(&str, &str)]) -> Output {
    run_selected(fixture, &[], environment)
}

fn run_selected(fixture: &Fixture, selection: &[&str], environment: &[(&str, &str)]) -> Output {
    let workload = fixture.root.join("run-candidate");
    if !workload.exists() {
        fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("write workload");
        make_executable(&workload);
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(selection)
        .arg("--")
        .arg(&workload);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("run the configuration matrix case")
}

/// Asserts one supported case trained PGO with complete, matched, observed
/// evidence on every interposed stage.
fn assert_trained(fixture: &Fixture, output: Output) -> Value {
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 3);
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["pgo_training"]["phase_parity"]["matched"], true);

    let parity = &manifest["compiler_parity"];
    assert_eq!(parity["matched"], true);
    assert_eq!(parity["reason"], Value::Null);
    assert_eq!(parity["differences"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        parity["stages"]
            .as_array()
            .expect("parity stages")
            .iter()
            .map(|stage| stage["stage"].as_str().expect("stage"))
            .collect::<Vec<_>>(),
        TRAINING_STAGES.map(|(stage, _)| stage)
    );
    assert_eq!(
        manifest["compiler_decisions"].as_array().map(Vec::len),
        Some(0),
        "a supported case recorded a compiler decision"
    );

    let builds = interposed_builds(&manifest);
    for (stage, injected) in TRAINING_STAGES {
        let build = builds
            .get(stage)
            .unwrap_or_else(|| panic!("{stage} was not executed"));
        assert_interposed_stage(build, stage, injected);
        assert!(
            build["environment_overrides"].is_null(),
            "{stage} synthesized compiler environment overrides"
        );
    }
    manifest
}

/// Asserts a PGO-only rejection left the static candidates eligible and gave
/// PGO no screening sample.
fn assert_pgo_only_rejection(manifest: &Value) {
    let strategies = manifest["strategies"].as_array().expect("strategies");
    for identity in ["thin-lto", "fat-lto-cgu1"] {
        let record = strategies
            .iter()
            .find(|record| record["identity"] == identity)
            .unwrap_or_else(|| panic!("{identity} strategy record"));
        assert_eq!(
            record["build"]["outcome"], "built",
            "a PGO-only rejection rejected the {identity} candidate"
        );
    }
    let pgo = strategies
        .iter()
        .find(|record| record["identity"] == "pgo")
        .expect("PGO strategy record");
    assert!(
        pgo["screening"].is_null(),
        "a rejected PGO candidate reached screening"
    );
    assert_ne!(manifest["selected_candidate"], "pgo");
}

/// Asserts one actionable compiler decision was published for `reason`.
fn assert_compiler_decision(manifest: &Value, reason: &str) {
    let decisions = manifest["compiler_decisions"]
        .as_array()
        .expect("compiler decisions");
    let decision = decisions
        .iter()
        .find(|decision| decision["reason"] == reason)
        .unwrap_or_else(|| panic!("no compiler decision named {reason}: {decisions:?}"));
    assert_eq!(decision["scope"], "pgo_only");
    assert!(
        decision["remediation"]
            .as_str()
            .is_some_and(|remediation| !remediation.is_empty()),
        "{reason} carries no remediation"
    );
    assert!(
        decision["message"]
            .as_str()
            .is_some_and(|message| message.len() <= 4096),
        "{reason} published an unbounded message"
    );
}

fn interposed_builds(manifest: &Value) -> BTreeMap<String, Value> {
    let mut builds = BTreeMap::new();
    let mut collect = |build: &Value| {
        if let Some(stage) = build["interposition"]["stage"].as_str() {
            builds.insert(stage.to_owned(), build.clone());
        }
    };
    collect(&manifest["pgo_training"]["reference_build"]);
    collect(&manifest["pgo_training"]["instrumentation_build"]);
    for strategy in manifest["strategies"].as_array().expect("strategies") {
        collect(&strategy["build"]);
    }
    builds
}

/// Asserts the winning source's arguments keep one consecutive resolved order
/// in every training stage's observed target compilation.
fn assert_ordered_across_training_stages(fixture: &Fixture, arguments: &[&str]) {
    let run_directory = fixture.run_directory();
    for (stage, _) in TRAINING_STAGES {
        let record = sole_target_record(&run_directory, stage);
        assert_consecutive(&record, arguments, stage);
    }
}

fn assert_consecutive(record: &Value, arguments: &[&str], stage: &str) {
    let digests: Vec<&str> = record["argument_digests"]
        .as_array()
        .expect("argument digests")
        .iter()
        .map(|digest| digest.as_str().expect("argument digest"))
        .collect();
    let positions: Vec<usize> = arguments
        .iter()
        .map(|argument| {
            let digest = argument_digest(argument);
            digests
                .iter()
                .position(|observed| *observed == digest)
                .unwrap_or_else(|| {
                    panic!("{stage} did not observe the resolved argument {argument}")
                })
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{stage} did not preserve the resolved argument order: {positions:?}"
    );
}

fn sole_target_record(run_directory: &Path, stage: &str) -> Value {
    let mut records: Vec<Value> = capture_records(run_directory, stage)
        .into_iter()
        .filter(|record| record["class"] == "target_compile")
        .collect();
    assert_eq!(
        records.len(),
        1,
        "{stage} must observe exactly one target compilation"
    );
    records.remove(0)
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

fn source_names(graph: &Value, root: &Path) -> Vec<String> {
    fixture_sources(graph, root)
        .into_iter()
        .map(|source| file_name(&source["path"]))
        .collect()
}

fn file_name(path: &Value) -> String {
    Path::new(path.as_str().expect("source path"))
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned()
}

/// A Cargo wrapper that rewrites the included configuration once the
/// instrumentation stage has been built, so the optimized stage resolves a
/// different compiler input than the stages it is compared against.
fn config_drift_wrapper(root: &Path, drifted: &str) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let included = root.join(".cargo/flags.toml");
    assert!(!included.to_string_lossy().contains('\''));
    let wrapper = root.join("config-drift");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nstage=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */target/pgo_generate) stage=generate ;;\n  esac\ndone\n'{real_cargo}' \"$@\"\nstatus=$?\nif [ \"$stage\" = generate ] && [ \"$status\" -eq 0 ]; then\n  cat > '{included}' <<'TEMPER_DRIFT'\n{drifted}TEMPER_DRIFT\nfi\nexit \"$status\"\n",
            included = included.display()
        ),
    )
    .expect("write config drift wrapper");
    make_executable(&wrapper);
    wrapper
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}
