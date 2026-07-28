#![allow(clippy::unwrap_used, clippy::expect_used)]

//! v0.0.3 EP-002: the private compiler shim, target-scoped PGO injection,
//! bounded capture evidence and per-stage isolation.

mod support;

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use support::{Fixture, stderr, stdout};

const PROTOCOL: &str = "temper-rustc-shim-1";
const HOST_TARGET: &str = "x86_64-unknown-linux-gnu";
const PROTOCOL_FAILURE_EXIT: i32 = 97;
const REJECTION_EXIT: i32 = 98;

// US-004: the shim is private, dispatched before Clap, and transparent.

#[test]
fn the_public_cli_exposes_no_shim_surface() {
    for arguments in [
        vec!["temper", "--help"],
        vec!["temper", "optimize", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
            .args(&arguments)
            .output()
            .expect("run help");
        let text = format!("{}{}", stdout(&output), stderr(&output));
        for secret in ["TEMPER_SHIM", "shim", PROTOCOL] {
            assert!(
                !text.contains(secret),
                "{arguments:?} exposed {secret}:\n{text}"
            );
        }
    }
}

#[test]
fn shim_dispatch_precedes_command_line_parsing() {
    let shim = ShimHarness::new("dispatch");
    // `temper optimize` without a workload is a CLI error, yet in shim mode it
    // is opaque argv that must reach the real compiler unchanged.
    let output = shim.invoke(&["temper", "optimize"], &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(shim.forwarded_arguments(), ["temper", "optimize"]);
    assert_eq!(shim.records().len(), 1);
}

#[test]
fn process_replacement_preserves_streams_status_and_signals() {
    let shim = ShimHarness::new("transparency");
    let success = shim.invoke_with_stdin(&["--emit=link"], &[], b"workload stdin");
    assert!(success.status.success());
    assert_eq!(stdout(&success), "compiler stdout\n");
    assert!(stderr(&success).contains("compiler stderr"));
    assert_eq!(shim.forwarded_stdin(), b"workload stdin");
    // The private protocol never reaches the real compiler.
    assert_eq!(shim.forwarded_shim_environment(), "");

    let failed = shim.invoke(&["--emit=link"], &[("TEMPER_FAKE_EXIT", "42")]);
    assert_eq!(failed.status.code(), Some(42));

    let signalled = shim.invoke(&["--emit=link"], &[("TEMPER_FAKE_SIGNAL", "1")]);
    assert_eq!(signalled.status.code(), None);
    assert_eq!(signalled.status.signal(), Some(libc::SIGABRT));
}

#[test]
fn argument_bytes_survive_process_replacement_without_conversion() {
    let shim = ShimHarness::new("bytes");
    let invalid = OsString::from_vec(vec![0x2d, 0x2d, 0xff, 0xfe, 0x80]);
    let quoted = OsString::from("--cfg=temper_label=\"spaced value\"");
    let output = shim.invoke_os(
        &[
            OsString::from("--crate-name"),
            OsString::from("app"),
            invalid.clone(),
            quoted.clone(),
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let forwarded = shim.forwarded_argument_bytes();
    assert_eq!(forwarded[2], invalid.as_bytes());
    assert_eq!(forwarded[3], quoted.as_bytes());
}

#[test]
fn protocol_failures_fail_closed_without_any_fallback_compiler() {
    let shim = ShimHarness::new("protocol");
    let cases: Vec<(&str, Vec<(&str, OsString)>)> = vec![
        (
            "missing value",
            vec![("TEMPER_SHIM_STAGE", OsString::new())],
        ),
        (
            "unsupported version",
            vec![(
                "TEMPER_SHIM_PROTOCOL",
                OsString::from("temper-rustc-shim-0"),
            )],
        ),
        (
            "unknown stage",
            vec![("TEMPER_SHIM_STAGE", OsString::from("baseline"))],
        ),
        (
            "unsupported target",
            vec![(
                "TEMPER_SHIM_TARGET",
                OsString::from("aarch64-unknown-linux-gnu"),
            )],
        ),
        (
            "out-of-root capture directory",
            vec![(
                "TEMPER_SHIM_CAPTURE_DIR",
                shim.outside.as_os_str().to_owned(),
            )],
        ),
        (
            "out-of-root profile path",
            vec![(
                "TEMPER_SHIM_INJECTION",
                OsString::from(format!("generate={}", shim.outside.display())),
            )],
        ),
        (
            "relative profile path",
            vec![(
                "TEMPER_SHIM_INJECTION",
                OsString::from("generate=relative/profiles"),
            )],
        ),
        (
            "relative real rustc",
            vec![("TEMPER_SHIM_REAL_RUSTC", OsString::from("rustc"))],
        ),
    ];
    for (label, overrides) in cases {
        let output = shim.invoke_overridden(&["--emit=link"], &overrides);
        assert_eq!(
            output.status.code(),
            Some(PROTOCOL_FAILURE_EXIT),
            "{label} must fail the private protocol: {}",
            stderr(&output)
        );
        let diagnostic = stderr(&output);
        assert_eq!(
            diagnostic.lines().count(),
            1,
            "{label} must emit exactly one bounded diagnostic"
        );
        assert!(diagnostic.contains(PROTOCOL), "{label}: {diagnostic}");
        assert!(diagnostic.len() < 1024, "{label} diagnostic is unbounded");
        assert!(
            !shim.compiler_ran(),
            "{label} executed a compiler after a protocol failure"
        );
        assert!(
            !shim.canary_ran(),
            "{label} fell back to a PATH compiler after a protocol failure"
        );
        assert!(shim.records().is_empty(), "{label} persisted a record");
    }
}

#[test]
fn a_duplicated_private_value_fails_closed() {
    let shim = ShimHarness::new("duplicate");
    let output = shim.invoke_with_duplicate_stage("pgo_use");
    assert_eq!(
        output.status.code(),
        Some(PROTOCOL_FAILURE_EXIT),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("duplicate private protocol value"));
    assert!(!shim.compiler_ran());
}

// US-005: phase controls reach resolved target compilations only.

#[test]
fn phase_controls_reach_target_compilations_only() {
    let shim = ShimHarness::new("injection");
    let profile = shim.run_root.join("pgo/raw");
    fs::create_dir_all(&profile).expect("profile directory");
    let generate = format!("generate={}", profile.display());

    let target = shim.invoke(
        &[
            "--crate-name",
            "app",
            "--target",
            HOST_TARGET,
            "--emit=link",
        ],
        &[("TEMPER_SHIM_INJECTION", &generate)],
    );
    assert!(target.status.success(), "{}", stderr(&target));
    let forwarded = shim.forwarded_arguments();
    assert_eq!(
        forwarded.last().map(String::as_str),
        Some(format!("-Cprofile-generate={}", profile.display()).as_str())
    );
    assert_eq!(forwarded.len(), 6, "every resolved argument is preserved");
    let record = shim.single_record();
    assert_eq!(record["class"], "target_compile");
    assert_eq!(
        record["injected_flags"],
        serde_json::json!(["profile-generate"])
    );
    assert_ne!(record["pre_digest"], record["post_digest"]);

    for (label, arguments) in [
        (
            "target-info probe",
            vec!["--print=file-names", "--target", HOST_TARGET, "--emit=link"],
        ),
        (
            "host build script",
            vec!["--crate-name", "build_script_build", "--emit=link"],
        ),
        (
            "host path containing the triple",
            vec![
                "--crate-name",
                "temper_macro",
                "--out-dir",
                "/tmp/x86_64-unknown-linux-gnu/release",
                "--emit=link",
            ],
        ),
        ("version probe", vec!["-vV"]),
    ] {
        shim.reset();
        let output = shim.invoke(&arguments, &[("TEMPER_SHIM_INJECTION", &generate)]);
        assert!(output.status.success(), "{label}: {}", stderr(&output));
        let forwarded = shim.forwarded_arguments();
        assert_eq!(forwarded, arguments, "{label} argv was modified");
        let record = shim.single_record();
        assert_ne!(record["class"], "target_compile", "{label} misclassified");
        assert_eq!(record["injected_flags"], serde_json::json!([]));
        assert_eq!(record["pre_digest"], record["post_digest"]);
    }
}

#[test]
fn pre_existing_profiling_controls_reject_before_rustc_runs() {
    for control in [
        "-Cprofile-generate=/tmp/other",
        "-Cprofile-use=/tmp/other.profdata",
        "-Cinstrument-coverage",
    ] {
        let shim = ShimHarness::new("conflict");
        let output = shim.invoke(
            &[
                "--crate-name",
                "app",
                "--target",
                HOST_TARGET,
                "--emit=link",
                control,
            ],
            &[("TEMPER_SHIM_INJECTION", "none")],
        );
        assert_eq!(
            output.status.code(),
            Some(REJECTION_EXIT),
            "{control} must reject PGO: {}",
            stderr(&output)
        );
        assert!(
            !shim.compiler_ran(),
            "{control} reached the real compiler before rejection"
        );
        let record = shim.single_record();
        assert_eq!(record["rejection"], "pgo_compiler_input_conflict");
        assert_eq!(record["injected_flags"], serde_json::json!([]));
    }
}

#[test]
fn an_ambiguous_argument_shape_rejects_instead_of_guessing() {
    let shim = ShimHarness::new("ambiguity");
    let output = shim.invoke(
        &[
            "--crate-name",
            "app",
            "--target",
            HOST_TARGET,
            "--target",
            "aarch64-unknown-linux-gnu",
            "--emit=link",
        ],
        &[("TEMPER_SHIM_INJECTION", "none")],
    );
    assert_eq!(output.status.code(), Some(REJECTION_EXIT));
    assert!(!shim.compiler_ran());
    assert_eq!(
        shim.single_record()["rejection"],
        "pgo_compiler_input_ambiguous"
    );
}

// US-006: bounded, concurrent-safe evidence.

#[test]
fn every_record_is_bounded_and_publishes_no_raw_argv() {
    let shim = ShimHarness::new("bounded");
    let secret = "--cfg=temper_secret_value_that_must_not_be_published";
    let output = shim.invoke(
        &[
            "--crate-name",
            "app",
            "--target",
            HOST_TARGET,
            "--emit=link",
            secret,
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let path = shim.record_paths()[0].clone();
    let raw = fs::read_to_string(&path).expect("record contents");
    assert!(
        !raw.contains("temper_secret_value_that_must_not_be_published"),
        "the record published raw argv: {raw}"
    );
    assert!(raw.len() <= 16 * 1024);
    let record = shim.single_record();
    assert_eq!(record["protocol"], PROTOCOL);
    assert_eq!(record["normalization"], 1);
    assert_eq!(record["stage"], "pgo_generate");
    assert_eq!(record["crate_name"], "app");
    assert_eq!(record["argument_count"], 6);
    assert!(
        record["pre_digest"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
    assert_eq!(
        path.file_name().and_then(OsStr::to_str),
        Some(format!("{}.json", record["record_id"].as_str().expect("id")).as_str())
    );
}

#[test]
fn concurrent_shim_processes_never_overwrite_a_record() {
    let shim = ShimHarness::new("concurrency");
    let invocations = 100;
    let children: Vec<_> = (0..invocations)
        .map(|index| {
            let mut command = shim.command(&[
                OsString::from("--crate-name"),
                OsString::from(format!("crate_{index}")),
                OsString::from("--target"),
                OsString::from(HOST_TARGET),
                OsString::from("--emit=link"),
            ]);
            command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn shim")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("await shim").success());
    }
    let records = shim.records();
    assert_eq!(records.len(), invocations);
    let identities: std::collections::BTreeSet<&str> = records
        .iter()
        .filter_map(|record| record["record_id"].as_str())
        .collect();
    assert_eq!(identities.len(), invocations, "record identities collided");
    let crates: std::collections::BTreeSet<&str> = records
        .iter()
        .filter_map(|record| record["crate_name"].as_str())
        .collect();
    assert_eq!(crates.len(), invocations, "a record was overwritten");
}

// US-005 and US-007 end to end.

#[test]
fn an_included_config_rustflag_survives_every_pgo_phase() {
    let fixture = include_fixture();
    let workload = fixture.root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("workload");
    make_executable(&workload);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload)
        .output()
        .expect("run included-config PGO");
    // The fixture fails to compile unless the sentinel rustflag reaches rustc,
    // so a trained PGO outcome is the executed proof that v0.0.2's include
    // defect is fixed.
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(manifest["pgo_training"]["phase_parity"]["matched"], true);

    let stages = interposed_stages(&manifest);
    assert_eq!(
        stages.keys().collect::<Vec<_>>(),
        ["pgo_generate", "pgo_reference", "pgo_use"]
    );
    let mut target_directories = Vec::new();
    let mut capture_directories = Vec::new();
    for (stage, build) in &stages {
        assert_eq!(build["outcome"], "built", "{stage} was not built");
        assert!(build["environment_overrides"].is_null());
        target_directories.push(
            build["target_directory"]
                .as_str()
                .expect("target directory"),
        );
        capture_directories.push(
            build["interposition"]["capture_directory"]
                .as_str()
                .expect("capture directory"),
        );
        assert_eq!(build["compiler_evidence"]["host_compilations"], 0);
    }
    target_directories.sort_unstable();
    target_directories.dedup();
    capture_directories.sort_unstable();
    capture_directories.dedup();
    assert_eq!(target_directories.len(), 3, "stage target roots aliased");
    assert_eq!(capture_directories.len(), 3, "stage capture roots aliased");
}

// US-007: wrapper rejection and evidence failures reject only PGO.

#[test]
fn an_ambient_wrapper_rejects_pgo_before_the_reference_build() {
    let fixture = Fixture::single("ambient-wrapper", true, true);
    let wrapper = fixture.root.join("passthrough-wrapper");
    fs::write(&wrapper, "#!/bin/sh\nexec \"$@\"\n").expect("wrapper");
    make_executable(&wrapper);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("RUSTC_WRAPPER", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run ambient wrapper case");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "ambient_compiler_override"
    );
    assert_eq!(manifest["pgo_training"]["failure_stage"], "prerequisites");
    assert!(manifest["pgo_training"]["reference_build"].is_null());
    assert!(manifest["pgo_training"]["prerequisites"].is_null());
    let strategies = manifest["strategies"].as_array().expect("strategies");
    assert_eq!(strategies[0]["build"]["outcome"], "built");
    assert_eq!(strategies[1]["build"]["outcome"], "built");
    assert_eq!(strategies[2]["rejection_reason"], "pgo_training_rejected");
    assert!(!fixture.run_directory().join("captures").exists());
}

#[test]
fn a_configured_compiler_override_rejects_pgo_before_the_reference_build() {
    let fixture = Fixture::single("configured-wrapper", true, true);
    fs::create_dir(fixture.root.join(".cargo")).expect("Cargo config directory");
    fs::write(
        fixture.root.join(".cargo/config.toml"),
        "[build]\nrustc-workspace-wrapper = \"/usr/bin/env\"\n",
    )
    .expect("configured wrapper");

    let output = fixture.optimize(&["--allow-dirty"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "unproven_compiler_override"
    );
    assert!(manifest["pgo_training"]["reference_build"].is_null());
    assert!(
        manifest["pgo_training"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("build.rustc-workspace-wrapper"))
    );
    let strategies = manifest["strategies"].as_array().expect("strategies");
    assert_eq!(strategies[0]["build"]["outcome"], "built");
    assert_eq!(strategies[1]["build"]["outcome"], "built");
}

#[test]
fn tampered_capture_evidence_rejects_only_pgo() {
    for mode in ["truncated", "symlink", "duplicate-identity"] {
        let fixture = Fixture::single(&format!("capture-{mode}"), true, true);
        let workload = fixture.root.join("run-candidate");
        fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("workload");
        make_executable(&workload);
        let wrapper = capture_tampering_wrapper(&fixture.root, mode);

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
            .current_dir(&fixture.root)
            .env("CARGO", &wrapper)
            .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
            .arg(fixture.root.join("Cargo.toml"))
            .arg("--")
            .arg(&workload)
            .output()
            .expect("run capture tampering case");
        assert!(output.status.success(), "{mode}: {}", stderr(&output));

        let manifest = fixture.manifest();
        let pgo = manifest["strategies"]
            .as_array()
            .and_then(|strategies| strategies.iter().find(|record| record["identity"] == "pgo"))
            .expect("PGO strategy record");
        assert_eq!(
            pgo["build_failure"]["reason"], "compiler_capture_corrupt",
            "{mode} did not fail closed"
        );
        assert_eq!(pgo["rejection_reason"], "compiler_capture_corrupt");
        assert!(pgo["screening"].is_null(), "{mode} reached screening");
        assert_ne!(manifest["selected_candidate"], "pgo");
        let strategies = manifest["strategies"].as_array().expect("strategies");
        assert!(
            strategies[..2]
                .iter()
                .all(|strategy| strategy["build"]["outcome"] == "built"),
            "{mode} rejected a static candidate"
        );
    }
}

/// Drives the installed executable directly through its private protocol.
struct ShimHarness {
    _root: tempfile::TempDir,
    run_root: PathBuf,
    capture_directory: PathBuf,
    outside: PathBuf,
    real_rustc: PathBuf,
    argv_path: PathBuf,
    environment_path: PathBuf,
    stdin_path: PathBuf,
    canary_path: PathBuf,
    path: OsString,
}

impl ShimHarness {
    fn new(label: &str) -> Self {
        let root = tempfile::tempdir().expect("shim root");
        let base = fs::canonicalize(root.path()).expect("canonical shim root");
        let run_root = base.join("run");
        let capture_directory = run_root.join("captures/pgo_generate");
        let target_directory = run_root.join("target/pgo_generate");
        let outside = base.join("outside");
        for directory in [&capture_directory, &target_directory, &outside] {
            fs::create_dir_all(directory).expect("shim directory");
        }
        let observations = base.join("observations");
        fs::create_dir(&observations).expect("observation directory");
        let argv_path = observations.join(format!("{label}-argv"));
        let environment_path = observations.join(format!("{label}-environment"));
        let stdin_path = observations.join(format!("{label}-stdin"));
        let canary_path = observations.join(format!("{label}-canary"));

        let real_rustc = base.join("fake-rustc");
        fs::write(
            &real_rustc,
            "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\"; done > \"$TEMPER_FAKE_ARGV\"\nenv | grep '^TEMPER_SHIM' > \"$TEMPER_FAKE_ENV\" || printf '' > \"$TEMPER_FAKE_ENV\"\ncat > \"$TEMPER_FAKE_STDIN\"\nprintf 'compiler stdout\\n'\nprintf 'compiler stderr\\n' >&2\nif [ -n \"${TEMPER_FAKE_SIGNAL:-}\" ]; then kill -ABRT $$; fi\nexit \"${TEMPER_FAKE_EXIT:-0}\"\n",
        )
        .expect("write fake rustc");
        make_executable(&real_rustc);

        let path_directory = base.join("path-bin");
        fs::create_dir(&path_directory).expect("PATH directory");
        let canary = path_directory.join("rustc");
        fs::write(
            &canary,
            format!(
                "#!/bin/sh\nprintf 'fallback\\n' > '{}'\nexit 0\n",
                canary_path.display()
            ),
        )
        .expect("write PATH canary");
        make_executable(&canary);
        let mut entries = vec![path_directory];
        entries.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(entries).expect("PATH");

        Self {
            _root: root,
            run_root,
            capture_directory,
            outside,
            real_rustc,
            argv_path,
            environment_path,
            stdin_path,
            canary_path,
            path,
        }
    }

    fn protocol_environment(&self) -> BTreeMap<&'static str, OsString> {
        BTreeMap::from([
            ("TEMPER_SHIM_PROTOCOL", OsString::from(PROTOCOL)),
            ("TEMPER_SHIM_RUN_ROOT", self.run_root.as_os_str().to_owned()),
            (
                "TEMPER_SHIM_REAL_RUSTC",
                self.real_rustc.as_os_str().to_owned(),
            ),
            ("TEMPER_SHIM_STAGE", OsString::from("pgo_generate")),
            ("TEMPER_SHIM_TARGET", OsString::from(HOST_TARGET)),
            (
                "TEMPER_SHIM_TARGET_DIR",
                self.run_root.join("target/pgo_generate").into_os_string(),
            ),
            (
                "TEMPER_SHIM_CAPTURE_DIR",
                self.capture_directory.as_os_str().to_owned(),
            ),
            ("TEMPER_SHIM_INJECTION", OsString::from("none")),
        ])
    }

    fn command(&self, arguments: &[OsString]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
        // A spawned child inherits the harness stdin by default, which would
        // block the compiler stand-in forever; only the stdin case pipes.
        command
            .args(arguments)
            .stdin(Stdio::null())
            .env("PATH", &self.path);
        command.env("TEMPER_FAKE_ARGV", &self.argv_path);
        command.env("TEMPER_FAKE_ENV", &self.environment_path);
        command.env("TEMPER_FAKE_STDIN", &self.stdin_path);
        for (name, value) in self.protocol_environment() {
            command.env(name, value);
        }
        command
    }

    fn invoke(&self, arguments: &[&str], overrides: &[(&str, &str)]) -> Output {
        let overrides: Vec<(&str, OsString)> = overrides
            .iter()
            .map(|(name, value)| (*name, OsString::from(*value)))
            .collect();
        self.invoke_overridden(arguments, &overrides)
    }

    fn invoke_overridden(&self, arguments: &[&str], overrides: &[(&str, OsString)]) -> Output {
        let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        let mut command = self.command(&arguments);
        for (name, value) in overrides {
            if value.is_empty() {
                command.env_remove(name);
            } else {
                command.env(name, value);
            }
        }
        command.output().expect("run shim")
    }

    fn invoke_os(&self, arguments: &[OsString], overrides: &[(&str, &str)]) -> Output {
        let mut command = self.command(arguments);
        for (name, value) in overrides {
            command.env(name, value);
        }
        command.output().expect("run shim")
    }

    fn invoke_with_stdin(
        &self,
        arguments: &[&str],
        overrides: &[(&str, &str)],
        stdin: &[u8],
    ) -> Output {
        use std::io::Write;
        let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        let mut command = self.command(&arguments);
        for (name, value) in overrides {
            command.env(name, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn shim");
        child
            .stdin
            .take()
            .expect("shim stdin")
            .write_all(stdin)
            .expect("write shim stdin");
        child.wait_with_output().expect("await shim")
    }

    /// Executes the shim with a genuinely duplicated protocol entry, which the
    /// standard process API cannot express.
    fn invoke_with_duplicate_stage(&self, duplicate: &str) -> Output {
        let program = CString::new(env!("CARGO_BIN_EXE_cargo-temper")).expect("program");
        let argv: Vec<CString> = ["cargo-temper", "--emit=link"]
            .iter()
            .map(|argument| CString::new(*argument).expect("argument"))
            .collect();
        let mut environment: Vec<CString> = self
            .protocol_environment()
            .into_iter()
            .map(|(name, value)| {
                let mut entry = OsString::from(name);
                entry.push("=");
                entry.push(value);
                CString::new(entry.into_vec()).expect("environment entry")
            })
            .collect();
        environment
            .push(CString::new(format!("TEMPER_SHIM_STAGE={duplicate}")).expect("duplicate"));
        environment.push(
            CString::new(format!("TEMPER_FAKE_ARGV={}", self.argv_path.display()))
                .expect("argv path"),
        );
        environment.push(
            CString::new(format!(
                "TEMPER_FAKE_ENV={}",
                self.environment_path.display()
            ))
            .expect("environment path"),
        );
        environment.push(
            CString::new(format!("TEMPER_FAKE_STDIN={}", self.stdin_path.display()))
                .expect("stdin path"),
        );
        let argv_pointers: Vec<*const libc::c_char> = argv
            .iter()
            .map(|argument| argument.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let environment_pointers: Vec<*const libc::c_char> = environment
            .iter()
            .map(|entry| entry.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let program_address = program.as_ptr() as usize;
        let argv_address = argv_pointers.as_ptr() as usize;
        let environment_address = environment_pointers.as_ptr() as usize;

        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
        // SAFETY: every pointer is built before the fork and stays valid in the
        // child's copied address space; the closure only calls execve.
        unsafe {
            command.pre_exec(move || {
                libc::execve(
                    program_address as *const libc::c_char,
                    argv_address as *const *const libc::c_char,
                    environment_address as *const *const libc::c_char,
                );
                Err(std::io::Error::last_os_error())
            });
        }
        command.output().expect("run duplicated protocol shim")
    }

    fn reset(&self) {
        for path in self.record_paths() {
            fs::remove_file(path).expect("remove record");
        }
        let _ = fs::remove_file(&self.argv_path);
    }

    fn record_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = fs::read_dir(&self.capture_directory)
            .expect("read capture directory")
            .map(|entry| entry.expect("capture entry").path())
            .collect();
        paths.sort();
        paths
    }

    fn records(&self) -> Vec<Value> {
        self.record_paths()
            .iter()
            .map(|path| {
                serde_json::from_slice(&fs::read(path).expect("read record")).expect("parse record")
            })
            .collect()
    }

    fn single_record(&self) -> Value {
        let mut records = self.records();
        assert_eq!(records.len(), 1, "exactly one record per invocation");
        records.remove(0)
    }

    fn forwarded_argument_bytes(&self) -> Vec<Vec<u8>> {
        let raw = fs::read(&self.argv_path).expect("forwarded argv");
        raw.split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    }

    fn forwarded_arguments(&self) -> Vec<String> {
        self.forwarded_argument_bytes()
            .into_iter()
            .map(|argument| String::from_utf8(argument).expect("UTF-8 argument"))
            .collect()
    }

    fn forwarded_shim_environment(&self) -> String {
        fs::read_to_string(&self.environment_path).expect("forwarded environment")
    }

    fn forwarded_stdin(&self) -> Vec<u8> {
        fs::read(&self.stdin_path).expect("forwarded stdin")
    }

    fn compiler_ran(&self) -> bool {
        self.argv_path.exists()
    }

    fn canary_ran(&self) -> bool {
        self.canary_path.exists()
    }
}

/// A workspace whose only source of a required rustflag is an included config.
fn include_fixture() -> Fixture {
    let fixture = Fixture::single("include-sentinel", true, true);
    fs::write(
        fixture.root.join("src/main.rs"),
        "#[cfg(not(temper_included_sentinel))]\ncompile_error!(\"the included target rustflag did not reach this compilation\");\n\nfn main() {\n    println!(\"included sentinel\");\n}\n",
    )
    .expect("write sentinel source");
    let cargo = fixture.root.join(".cargo");
    fs::create_dir(&cargo).expect("Cargo config directory");
    fs::write(cargo.join("config.toml"), "include = [\"included.toml\"]\n")
        .expect("write including config");
    fs::write(
        cargo.join("included.toml"),
        "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", \"temper_included_sentinel\", \"--check-cfg=cfg(temper_included_sentinel)\"]\n",
    )
    .expect("write included config");
    fixture
}

/// A Cargo wrapper that corrupts the optimized stage's capture directory after
/// Cargo has produced a valid artifact.
fn capture_tampering_wrapper(root: &Path, mode: &str) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let tampering = match mode {
        "truncated" => "head -c 12 \"$first\" > \"$captures/truncated.json\"",
        "symlink" => "ln -s /etc/hostname \"$captures/linked.json\"",
        _ => "cp \"$first\" \"$captures/duplicated.json\"",
    };
    let wrapper = root.join(format!("capture-tampering-{mode}"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntarget=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */target/pgo_use) target=$argument ;;\n  esac\ndone\n'{real_cargo}' \"$@\"\nstatus=$?\nif [ -n \"$target\" ] && [ \"$status\" -eq 0 ]; then\n  captures=\"${{target%/target/pgo_use}}/captures/pgo_use\"\n  first=$(ls \"$captures\"/*.json | head -n 1)\n  {tampering}\nfi\nexit \"$status\"\n"
        ),
    )
    .expect("write capture tampering wrapper");
    make_executable(&wrapper);
    wrapper
}

fn interposed_stages(manifest: &Value) -> BTreeMap<String, Value> {
    let mut stages = BTreeMap::new();
    let mut collect = |build: &Value| {
        if let Some(stage) = build["interposition"]["stage"].as_str() {
            stages.insert(stage.to_owned(), build.clone());
        }
    };
    collect(&manifest["pgo_training"]["reference_build"]);
    collect(&manifest["pgo_training"]["instrumentation_build"]);
    for strategy in manifest["strategies"].as_array().expect("strategies") {
        collect(&strategy["build"]);
    }
    stages
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}
