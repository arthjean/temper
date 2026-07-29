#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test assertions and matrix case labels"
)]

//! v0.0.3 EP-004 US-012: the wrapper-rejection, process and evidence matrix.
//!
//! Every wrapper class is rejected before the PGO reference build exists, every
//! stage isolation and evidence defect fails closed with its stable schema-3
//! reason, and the pass-through boundary keeps its artifact identity and its
//! cold-build overhead budget.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Fixture, assert_interposed_stage, capture_records, stderr};

const HOST: &str = "x86_64-unknown-linux-gnu";
const PROTOCOL: &str = "temper-rustc-shim-1";
/// The distinctive component of every configured compiler override under test.
/// No report may ever publish it.
const OVERRIDE_MARKER: &str = "temper-unpublished-compiler-value";

/// The four interposed stages of one confirmed PGO run.
const INTERPOSED_STAGES: [(&str, &[&str]); 4] = [
    ("pgo_reference", &[]),
    ("pgo_generate", &["profile-generate"]),
    ("pgo_use", &["profile-use", "pgo-warn-missing-function"]),
    (
        "pgo_confirmation",
        &["profile-use", "pgo-warn-missing-function"],
    ),
];

// The control: with no wrapper anywhere, every interposed stage completes.

#[test]
fn the_no_wrapper_control_completes_every_interposed_stage() {
    let fixture = wrapper_fixture("wrapper-none");
    force_pgo_confirmation(&fixture);
    let output = optimize(&fixture, &[], &[]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 3);
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["compiler_parity"]["matched"], true);
    assert_eq!(manifest["confirmation"]["strategy"], "pgo");
    assert_eq!(manifest["confirmation"]["compiler_parity"]["matched"], true);
    assert_eq!(
        manifest["compiler_decisions"].as_array().map(Vec::len),
        Some(0)
    );

    let training = &manifest["pgo_training"];
    let builds = [
        &training["reference_build"],
        &training["instrumentation_build"],
        &strategy(&manifest, "pgo")["build"],
        &manifest["confirmation"]["candidate_build"],
    ];
    let run_directory = fixture.run_directory();
    let mut roots = Vec::new();
    for ((stage, injected), build) in INTERPOSED_STAGES.iter().zip(builds) {
        assert_interposed_stage(build, stage, injected);
        let records = capture_records(&run_directory, capture_directory(stage));
        assert!(
            records
                .iter()
                .any(|record| record["class"] == "target_compile"),
            "{stage} observed no target compilation"
        );
        assert!(
            records
                .iter()
                .all(|record| record["stage"] == *stage && record["protocol"] == PROTOCOL),
            "{stage} mixed records from another stage"
        );
        // Every stage owns a unique target root and a unique capture root, as
        // Cargo actually recorded them rather than as the plan intended them.
        roots.push(
            build["target_directory"]
                .as_str()
                .expect("stage target directory")
                .to_owned(),
        );
        roots.push(
            build["interposition"]["capture_directory"]
                .as_str()
                .expect("stage capture directory")
                .to_owned(),
        );
    }
    let unique = roots.len();
    roots.sort();
    roots.dedup();
    assert_eq!(roots.len(), unique, "stage target or capture roots aliased");
}

// Every wrapper class rejects only PGO, before the reference build exists.

#[test]
fn ambient_general_workspace_and_nested_wrappers_reject_only_pgo() {
    for (label, variables) in [
        ("general", vec!["RUSTC_WRAPPER"]),
        ("workspace", vec!["RUSTC_WORKSPACE_WRAPPER"]),
        ("nested", vec!["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]),
        ("cargo-build-general", vec!["CARGO_BUILD_RUSTC_WRAPPER"]),
    ] {
        let fixture = wrapper_fixture(&format!("wrapper-ambient-{label}"));
        let wrapper = pass_through_wrapper(&fixture.root, "sccache");
        let environment: Vec<(&str, &str)> = variables
            .iter()
            .map(|variable| (*variable, wrapper.to_str().expect("wrapper path")))
            .collect();

        let output = optimize(&fixture, &[], &environment);
        assert!(output.status.success(), "{label}: {}", stderr(&output));
        let manifest = fixture.manifest();
        assert_rejected_before_the_reference_build(
            &fixture,
            &manifest,
            "ambient_compiler_override",
        );
        let decision = compiler_decision(&manifest, "ambient_compiler_override");
        assert!(
            decision["source"].is_null(),
            "{label} named a config source for an ambient override"
        );
    }
}

#[test]
fn configured_wrappers_and_compilers_reject_only_pgo_through_nested_includes() {
    for (label, key, nested) in [
        ("general-direct", "rustc-wrapper", false),
        ("workspace-direct", "rustc-workspace-wrapper", false),
        ("general-included", "rustc-wrapper", true),
        ("workspace-included", "rustc-workspace-wrapper", true),
        ("compiler-included", "rustc", true),
    ] {
        let fixture = wrapper_fixture(&format!("wrapper-config-{label}"));
        // A wrapper execs its arguments and a replacement compiler execs the
        // real rustc, so the uninterposed baseline and static builds stay valid
        // and only the PGO boundary decides the outcome.
        let declared = if key == "rustc" {
            compiler_stand_in(&fixture.root)
        } else {
            pass_through_wrapper(&fixture.root, OVERRIDE_MARKER)
        };
        let table = format!("[build]\n{key} = \"{}\"\n", declared.display());
        let directory = fixture.root.join(".cargo");
        fs::create_dir_all(&directory).expect("Cargo config directory");
        if nested {
            // The declaration is only reachable through two include levels.
            fs::write(
                directory.join("config.toml"),
                "include = [\"layer1.toml\"]\n",
            )
            .expect("including config");
            fs::write(
                directory.join("layer1.toml"),
                "include = [\"layer2.toml\"]\n",
            )
            .expect("nested including config");
            fs::write(directory.join("layer2.toml"), &table).expect("declaring config");
        } else {
            fs::write(directory.join("config.toml"), &table).expect("declaring config");
        }

        let output = optimize(&fixture, &[], &[]);
        assert!(output.status.success(), "{label}: {}", stderr(&output));
        let manifest = fixture.manifest();
        assert_rejected_before_the_reference_build(
            &fixture,
            &manifest,
            "unproven_compiler_override",
        );

        let decision = compiler_decision(&manifest, "unproven_compiler_override");
        let expected_source = if nested { "layer2.toml" } else { "config.toml" };
        assert_eq!(
            decision["source"].as_str().map(|source| Path::new(source)
                .file_name()
                .expect("source file name")
                .to_string_lossy()
                .into_owned()),
            Some(expected_source.to_owned()),
            "{label} did not name the declaring config source"
        );
        assert!(
            decision["remediation"]
                .as_str()
                .is_some_and(|remediation| remediation.contains(&format!("build.{key}"))),
            "{label} remediation does not name the declared key"
        );
    }
}

#[test]
fn a_rejected_wrapper_is_named_without_publishing_its_value() {
    let fixture = wrapper_fixture("wrapper-unpublished-value");
    let wrapper = pass_through_wrapper(&fixture.root, OVERRIDE_MARKER);
    fs::create_dir_all(fixture.root.join(".cargo")).expect("Cargo config directory");
    fs::write(
        fixture.root.join(".cargo/config.toml"),
        format!("[build]\nrustc-wrapper = \"{}\"\n", wrapper.display()),
    )
    .expect("declaring config");

    let output = optimize(&fixture, &[], &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let persisted = fs::read_to_string(fixture.run_directory().join("run.json")).expect("run.json");
    assert!(
        !persisted.contains(OVERRIDE_MARKER),
        "run.json published the unrestricted compiler override value"
    );
    let reported = format!(
        "{}{}",
        stderr(&output),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !reported.contains(OVERRIDE_MARKER),
        "the report published the unrestricted compiler override value"
    );
}

// Stage isolation is enforced before Cargo starts.

#[test]
fn a_pre_existing_or_symlinked_stage_directory_rejects_before_cargo_starts() {
    for mode in ["directory", "symlink"] {
        let fixture = wrapper_fixture(&format!("stage-isolation-{mode}"));
        let wrapper = stage_directory_squatting_wrapper(&fixture.root, mode);

        let output = optimize(
            &fixture,
            &[],
            &[("CARGO", wrapper.to_str().expect("wrapper"))],
        );
        assert!(output.status.success(), "{mode}: {}", stderr(&output));

        let manifest = fixture.manifest();
        assert_eq!(
            manifest["pgo_training"]["rejection_reason"], "pgo_stage_isolation_failed",
            "{mode} did not reject a reused stage directory"
        );
        assert_eq!(manifest["pgo_training"]["failure_stage"], "instrumentation");
        assert!(
            manifest["pgo_training"]["instrumentation_build"].is_null(),
            "{mode} started Cargo on an aliased stage directory"
        );
        assert!(
            !fixture
                .run_directory()
                .join("captures/pgo_generate")
                .exists(),
            "{mode} captured evidence for a rejected stage"
        );
        assert_pgo_only(&manifest);
    }
}

// Evidence bounds fail closed with their own reason.

#[test]
fn an_over_budget_capture_record_rejects_only_pgo() {
    let fixture = wrapper_fixture("capture-budget");
    let wrapper = oversized_record_wrapper(&fixture.root);

    let output = optimize(
        &fixture,
        &[],
        &[("CARGO", wrapper.to_str().expect("wrapper"))],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    let pgo = strategy(&manifest, "pgo");
    assert_eq!(pgo["build_failure"]["reason"], "compiler_capture_limit");
    assert_eq!(pgo["rejection_reason"], "compiler_capture_limit");
    assert!(pgo["screening"].is_null());
    assert_pgo_only(&manifest);
    let decision = compiler_decision(&manifest, "compiler_capture_limit");
    assert_eq!(decision["stage"], "pgo_use");
}

// NFR-001 and NFR-002 on the production implementation.

#[test]
#[ignore = "paired cold-build benchmark; run explicitly for retained evidence"]
fn paired_pass_through_builds_keep_their_identity_and_overhead_budget() {
    let report = paired_overhead_report(12);
    let median = report["median_ratio"].as_f64().expect("median ratio");
    assert_eq!(
        report["identical_sha256"], true,
        "pass-through changed the selected executable: {report}"
    );
    assert!(
        median <= 1.05,
        "median pass-through overhead exceeded the 5% budget: {report}"
    );
    if let Some(path) = std::env::var_os("TEMPER_OVERHEAD_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&report).expect("report")).expect("write report");
    }
}

/// Runs `pairs` alternating direct and pass-through cold builds of the checked
/// in workspace fixture and reports the paired ratios and artifact identity.
fn paired_overhead_report(pairs: usize) -> Value {
    let fixture = Fixture::checked_in_fixture("pgo-workspace");
    let rustc = fs::canonicalize(real_rustc()).expect("canonical real rustc");
    // Users install a release binary, so the overhead budget is measured on one.
    let shim = release_shim();
    let roots = fixture.root.join("overhead");
    fs::create_dir_all(&roots).expect("overhead root");

    // One discarded warm-up pair pays the first-run page-cache and Cargo
    // metadata costs that would otherwise land on whichever arm ran first.
    cold_build(&fixture.root, &roots.join("warmup-direct"), None);
    cold_build(
        &fixture.root,
        &roots.join("warmup-shim"),
        Some((&shim, &rustc)),
    );

    let mut ratios = Vec::new();
    let mut identical = true;
    let mut artifact = None;
    for pair in 0..pairs {
        let direct_root = roots.join(format!("direct-{pair}"));
        let shim_root = roots.join(format!("shim-{pair}"));
        // Alternate which arm builds first so ordering cannot favour one arm.
        let (direct, shimmed) = if pair % 2 == 0 {
            let direct = cold_build(&fixture.root, &direct_root, None);
            (
                direct,
                cold_build(&fixture.root, &shim_root, Some((&shim, &rustc))),
            )
        } else {
            let shimmed = cold_build(&fixture.root, &shim_root, Some((&shim, &rustc)));
            (cold_build(&fixture.root, &direct_root, None), shimmed)
        };
        ratios.push(shimmed.0 as f64 / direct.0 as f64);
        identical &= direct.1 == shimmed.1;
        artifact.get_or_insert(direct.1);
    }
    ratios.sort_by(|left, right| left.partial_cmp(right).expect("finite ratio"));
    let median = ratios[ratios.len() / 2];
    serde_json::json!({
        "pairs": pairs,
        "median_ratio": median,
        "ratios": ratios,
        "identical_sha256": identical,
        "artifact_sha256": artifact,
        "real_rustc": rustc,
        "shim_executable": shim,
    })
}

/// Builds the release `cargo-temper` the benchmark interposes with, into its
/// own target directory so it never contends with the test run's profile.
fn release_shim() -> PathBuf {
    let target_directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join("overhead-shim");
    let status = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["build", "--release", "--locked", "--bin", "cargo-temper"])
        .arg("--target-dir")
        .arg(&target_directory)
        .status()
        .expect("build the release shim");
    assert!(status.success(), "the release shim could not be built");
    target_directory.join("release/cargo-temper")
}

/// One cold release build into a fresh target directory, optionally through the
/// installed executable acting as a pass-through compiler shim.
fn cold_build(
    root: &Path,
    target_directory: &Path,
    interposition: Option<(&Path, &Path)>,
) -> (u128, String) {
    // Cargo creates its target directory lazily, but the shim validates the
    // stage root on the very first probe, so both arms start from an existing
    // empty directory.
    fs::create_dir_all(target_directory).expect("cold build target directory");
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(root)
        .args(["build", "--release", "--locked", "--target", HOST])
        .args(["--package", "pgo-workspace-app"])
        .arg("--target-dir")
        .arg(target_directory)
        .env(
            "RUSTFLAGS",
            "--cfg temper_target --check-cfg=cfg(temper_target)",
        );
    if let Some((shim, rustc)) = interposition {
        let run_root = target_directory.parent().expect("overhead root");
        let captures = run_root.join(
            target_directory
                .file_name()
                .expect("stage name")
                .to_string_lossy()
                .into_owned()
                + "-captures",
        );
        fs::create_dir_all(&captures).expect("capture directory");
        command
            .env("RUSTC", shim)
            .env("TEMPER_SHIM_PROTOCOL", PROTOCOL)
            .env("TEMPER_SHIM_RUN_ROOT", run_root)
            .env("TEMPER_SHIM_REAL_RUSTC", rustc)
            .env("TEMPER_SHIM_STAGE", "pgo_reference")
            .env("TEMPER_SHIM_TARGET", HOST)
            .env("TEMPER_SHIM_TARGET_DIR", target_directory)
            .env("TEMPER_SHIM_CAPTURE_DIR", &captures)
            .env("TEMPER_SHIM_INJECTION", "none");
    }
    let started = Instant::now();
    let output = command.output().expect("cold build");
    let elapsed = started.elapsed().as_nanos();
    assert!(output.status.success(), "{}", stderr(&output));
    let executable = target_directory
        .join(HOST)
        .join("release")
        .join("pgo-workspace-app");
    (elapsed, sha256_file(&executable))
}

fn real_rustc() -> PathBuf {
    let output = Command::new("rustc")
        .arg("--print=sysroot")
        .output()
        .expect("rustc sysroot");
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).expect("sysroot").trim())
        .join("bin")
        .join("rustc")
}

fn sha256_file(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).expect("read artifact"));
    format!("{:x}", hasher.finalize())
}

/// A single-binary fixture with an executable workload.
fn wrapper_fixture(name: &str) -> Fixture {
    let fixture = Fixture::single(name, true, true);
    let workload = fixture.root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("write workload");
    make_executable(&workload);
    fixture
}

/// Makes the bounded search reach PGO confirmation. The path-dependent sleep
/// manufactures the decision boundary and is coverage evidence only, never a
/// measured optimization gain.
fn force_pgo_confirmation(fixture: &Fixture) {
    let workload = fixture.root.join("run-candidate");
    fs::write(
        &workload,
        "#!/bin/sh\ncase \"$TEMPER_BINARY\" in\n  */pgo_use/*|*/confirmation/pgo/*) sleep 0.03 ;;\n  *) sleep 0.15 ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
    )
    .expect("write confirmation workload");
    make_executable(&workload);
}

fn optimize(fixture: &Fixture, selection: &[&str], environment: &[(&str, &str)]) -> Output {
    let workload = fixture.root.join("run-candidate");
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
    command.output().expect("run the wrapper matrix case")
}

/// Asserts one wrapper class was rejected before any interposed stage ran.
fn assert_rejected_before_the_reference_build(fixture: &Fixture, manifest: &Value, reason: &str) {
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert_eq!(manifest["pgo_training"]["rejection_reason"], reason);
    assert_eq!(manifest["pgo_training"]["failure_stage"], "prerequisites");
    assert!(manifest["pgo_training"]["reference_build"].is_null());
    assert!(manifest["pgo_training"]["prerequisites"].is_null());
    assert!(manifest["compiler_parity"].is_null());
    assert!(
        !fixture.run_directory().join("captures").exists(),
        "{reason} captured compiler evidence before rejecting"
    );
    assert_pgo_only(manifest);
}

/// Asserts a PGO-only rejection left every static candidate eligible.
fn assert_pgo_only(manifest: &Value) {
    for identity in ["thin-lto", "fat-lto-cgu1"] {
        assert_eq!(
            strategy(manifest, identity)["build"]["outcome"],
            "built",
            "a PGO-only rejection rejected the {identity} candidate"
        );
    }
    assert!(strategy(manifest, "pgo")["screening"].is_null());
    assert_ne!(manifest["selected_candidate"], "pgo");
}

fn compiler_decision<'a>(manifest: &'a Value, reason: &str) -> &'a Value {
    let decisions = manifest["compiler_decisions"]
        .as_array()
        .expect("compiler decisions");
    let decision = decisions
        .iter()
        .find(|decision| decision["reason"] == reason)
        .unwrap_or_else(|| panic!("no compiler decision named {reason}: {decisions:?}"));
    assert_eq!(decision["scope"], "pgo_only");
    assert_eq!(decision["message_truncated"], false);
    decision
}

fn strategy<'a>(manifest: &'a Value, identity: &str) -> &'a Value {
    manifest["strategies"]
        .as_array()
        .expect("strategies")
        .iter()
        .find(|record| record["identity"] == identity)
        .unwrap_or_else(|| panic!("{identity} strategy record"))
}

/// The capture and target sub-path of one interposed stage.
fn capture_directory(stage: &str) -> &'static str {
    match stage {
        "pgo_reference" => "pgo_reference",
        "pgo_generate" => "pgo_generate",
        "pgo_use" => "pgo_use",
        _ => "confirmation/pgo",
    }
}

/// A wrapper that forwards its arguments unchanged, standing in for any general
/// or workspace compiler wrapper including a caching one.
fn pass_through_wrapper(root: &Path, name: &str) -> PathBuf {
    let wrapper = root.join(name);
    fs::write(&wrapper, "#!/bin/sh\nexec \"$@\"\n").expect("write pass-through wrapper");
    make_executable(&wrapper);
    wrapper
}

/// A replacement compiler that execs the real rustc, so `build.rustc` can be
/// declared without breaking the uninterposed builds.
fn compiler_stand_in(root: &Path) -> PathBuf {
    let compiler = root.join(OVERRIDE_MARKER);
    fs::write(&compiler, "#!/bin/sh\nexec rustc \"$@\"\n").expect("write compiler stand-in");
    make_executable(&compiler);
    compiler
}

/// A Cargo wrapper that squats the next stage's target directory once the
/// reference stage has been built, so the claim must fail before Cargo starts.
fn stage_directory_squatting_wrapper(root: &Path, mode: &str) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let squat = match mode {
        "symlink" => "ln -s \"$target\" \"${target%/pgo_reference}/pgo_generate\"",
        _ => "mkdir -p \"${target%/pgo_reference}/pgo_generate\"",
    };
    let wrapper = root.join(format!("stage-squat-{mode}"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntarget=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */target/pgo_reference) target=$argument ;;\n  esac\ndone\n'{real_cargo}' \"$@\"\nstatus=$?\nif [ -n \"$target\" ] && [ \"$status\" -eq 0 ]; then\n  {squat}\nfi\nexit \"$status\"\n"
        ),
    )
    .expect("write stage squatting wrapper");
    make_executable(&wrapper);
    wrapper
}

/// A Cargo wrapper that writes one capture record past the bounded record size
/// after the optimized stage produced a valid artifact.
fn oversized_record_wrapper(root: &Path) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let wrapper = root.join("capture-oversize");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntarget=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */target/pgo_use) target=$argument ;;\n  esac\ndone\n'{real_cargo}' \"$@\"\nstatus=$?\nif [ -n \"$target\" ] && [ \"$status\" -eq 0 ]; then\n  captures=\"${{target%/target/pgo_use}}/captures/pgo_use\"\n  head -c 70000 /dev/zero | tr '\\\\0' 'x' > \"$captures/oversized.json\"\nfi\nexit \"$status\"\n"
        ),
    )
    .expect("write oversized record wrapper");
    make_executable(&wrapper);
    wrapper
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}
