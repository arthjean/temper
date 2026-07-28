#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test assertions and unreachable fixture modes"
)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_metadata::Message;
use support::{Fixture, stderr};

const HOST: &str = "x86_64-unknown-linux-gnu";

#[test]
fn cargo_preserves_owned_target_flag_order_and_excludes_host_units() {
    for (case, config, project_flags) in [
        ("absent", None, Vec::<String>::new()),
        (
            "string",
            Some("[target.x86_64-unknown-linux-gnu]\nrustflags = \"--cfg temper_string\"\n"),
            vec!["--cfg".to_owned(), "temper_string".to_owned()],
        ),
        (
            "array-space",
            Some(
                "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", 'temper_label=\"space value\"']\n",
            ),
            vec![
                "--cfg".to_owned(),
                "temper_label=\"space value\"".to_owned(),
            ],
        ),
    ] {
        let fixture = Fixture::checked_in_fixture("rustflags-workspace");
        if let Some(config) = config {
            fs::create_dir(fixture.root.join(".cargo")).expect("Cargo config directory");
            fs::write(fixture.root.join(".cargo/config.toml"), config)
                .expect("Cargo target config");
        }
        let capture_root = fixture.root.join("rustc-capture");
        let native_capture = capture_root.join("native");
        let composed_capture = capture_root.join("composed");
        fs::create_dir_all(&native_capture).expect("native capture directory");
        fs::create_dir(&composed_capture).expect("composed capture directory");
        let wrapper = rustc_capture_wrapper(&fixture.root);
        let native_output = cargo_fixture_command(
            &fixture.root,
            &wrapper,
            &native_capture,
            &format!("target-{case}-native"),
        )
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("run native Cargo rustflags fixture");
        assert!(native_output.status.success(), "{}", stderr(&native_output));
        let native_invocations = captured_invocations(&native_capture);
        let native_target = target_invocation(&native_invocations);
        assert!(
            project_flags.is_empty()
                || native_target
                    .windows(project_flags.len())
                    .any(|window| window == project_flags),
            "Cargo did not expose the configured flags in order: {native_target:?}"
        );

        let profile_directory = fixture.root.join("profiles");
        fs::create_dir(&profile_directory).expect("profile directory");
        let pgo_flag = format!("-Cprofile-generate={}", profile_directory.display());
        let mut encoded_flags = project_flags.clone();
        encoded_flags.push(pgo_flag.clone());

        let output = cargo_fixture_command(
            &fixture.root,
            &wrapper,
            &composed_capture,
            &format!("target-{case}-composed"),
        )
        .env(
            "CARGO_ENCODED_RUSTFLAGS",
            encoded_flags.join(&'\u{1f}'.to_string()),
        )
        .output()
        .expect("run Cargo rustflags fixture");
        assert!(output.status.success(), "{}", stderr(&output));

        let invocations = captured_invocations(&composed_capture);
        let target = target_invocation(&invocations);
        assert!(
            target
                .windows(encoded_flags.len())
                .any(|window| window == encoded_flags),
            "owned flags were not retained in order: {target:?}"
        );
        assert!(
            target.iter().any(|argument| argument == &pgo_flag),
            "target binary did not receive PGO"
        );
        if case == "array-space" {
            assert!(
                target
                    .iter()
                    .any(|argument| argument == "temper_label=\"space value\""),
                "embedded-space argument boundary was lost: {target:?}"
            );
        }

        let host_units: Vec<_> = invocations
            .iter()
            .filter(|arguments| {
                crate_name(arguments).is_some_and(|name| {
                    name == "build_script_build" || name == "temper_rustflags_macro"
                })
            })
            .collect();
        assert_eq!(host_units.len(), 2, "expected build script and proc macro");
        assert!(
            host_units
                .iter()
                .all(|arguments| { !arguments.iter().any(|argument| argument == &pgo_flag) })
        );
    }
}

#[test]
fn cargo_metadata_0231_exposes_incompatible_json_records_to_fail_closed_logic() {
    for fixture in ["unknown-reason.jsonl", "malformed-compiler-artifact.jsonl"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cargo-messages")
            .join(fixture);
        let contents = fs::read_to_string(path).expect("Cargo message fixture");
        let parsed: Vec<_> = Message::parse_stream(contents.as_bytes()).collect();
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            Ok(Message::TextLine(line)) => assert!(line.trim_start().starts_with('{')),
            Err(_) => {}
            other => panic!("fixture unexpectedly became a recognized Cargo message: {other:?}"),
        }
    }
}

#[test]
fn suspicious_json_and_ambiguous_artifacts_fail_before_measurement() {
    for (mode, reason) in [
        ("unknown", "unrecognized_cargo_json"),
        ("malformed", "unrecognized_cargo_json"),
        ("zero", "cargo_executable_missing"),
        ("multiple", "cargo_executable_ambiguous"),
    ] {
        let fixture = Fixture::single(&format!("stream-{mode}"), true, true);
        let wrapper = cargo_stream_wrapper(&fixture.root, mode);
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
            .current_dir(&fixture.root)
            .env("CARGO", &wrapper)
            .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
            .arg(fixture.root.join("Cargo.toml"))
            .args(["--", "/bin/true"])
            .output()
            .expect("run Cargo stream case");
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        let manifest = fixture.manifest();
        assert_eq!(
            manifest["failure"]["build_failure"]["reason"], reason,
            "unexpected failure for mode {mode}"
        );
        assert!(manifest["baseline_measurement"].is_null());
        assert!(manifest["strategies"].as_array().is_some_and(Vec::is_empty));
    }
}

#[test]
fn plain_text_and_fresh_matching_artifacts_remain_accepted() {
    let fixture = Fixture::single("fresh-artifact", true, true);
    let wrapper = cargo_stream_wrapper(&fixture.root, "fresh");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run fresh artifact case");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fixture.manifest()["baseline"]["outcome"], "built");
}

#[test]
fn new_schema_two_runs_leave_historical_schema_one_bytes_unchanged() {
    let fixture = Fixture::single("schema-boundary", true, true);
    let historical = fixture.root.join(".temper/runs/historical/run.json");
    fs::create_dir_all(historical.parent().expect("historical run parent"))
        .expect("historical run directory");
    let schema_one = b"{\"schema_version\":1,\"historical\":true}\n";
    fs::write(&historical, schema_one).expect("historical schema-1 record");

    let output = fixture.optimize(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fs::read(&historical).expect("historical schema-1 bytes"),
        schema_one
    );
    let new_run = fs::read_dir(fixture.root.join(".temper/runs"))
        .expect("runs directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("run entries")
        .into_iter()
        .find(|entry| entry.file_name() != "historical")
        .expect("new schema-2 run");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(new_run.path().join("run.json")).expect("schema-2 run"))
            .expect("parse schema-2 run");
    assert_eq!(manifest["schema_version"], 2);
}

fn rustc_capture_wrapper(root: &Path) -> PathBuf {
    let wrapper = root.join("capture-rustc");
    fs::write(
        &wrapper,
        "#!/bin/sh\nrecord=\"$TEMPER_RUSTC_CAPTURE_DIR/$$\"\n: > \"$record\"\nfor argument in \"$@\"; do\n  printf '%s\\n' \"$argument\" >> \"$record\"\ndone\nexec \"$@\"\n",
    )
    .expect("write rustc capture wrapper");
    make_executable(&wrapper);
    wrapper
}

fn cargo_stream_wrapper(root: &Path, mode: &str) -> PathBuf {
    let real_cargo = env!("CARGO");
    assert!(!real_cargo.contains('\''));
    let wrapper = root.join(format!("cargo-{mode}"));
    let build = match mode {
        "unknown" => format!(
            "'{real_cargo}' \"$@\"\nstatus=$?\nprintf '%s\\n' '{{\"reason\":\"future-cargo-message\"}}'\nexit \"$status\""
        ),
        "malformed" => format!(
            "'{real_cargo}' \"$@\"\nstatus=$?\nprintf '%s\\n' '{{\"reason\":\"compiler-artifact\"}}'\nexit \"$status\""
        ),
        "zero" => "printf '%s\\n' '{\"reason\":\"build-finished\",\"success\":true}'".to_owned(),
        "multiple" => format!(
            "'{real_cargo}' \"$@\" | awk '{{ print; if (index($0, \"\\\"reason\\\":\\\"compiler-artifact\\\"\") > 0) print }}'"
        ),
        "fresh" => format!(
            "'{real_cargo}' \"$@\" | sed 's/\"fresh\":false/\"fresh\":true/g'\nstatus=$?\nprintf '%s\\n' 'ordinary Cargo text diagnostic'\nexit \"$status\""
        ),
        _ => panic!("unknown Cargo stream mode"),
    };
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" != 'build' ]; then\n  exec '{real_cargo}' \"$@\"\nfi\n{build}\n"
        ),
    )
    .expect("write Cargo stream wrapper");
    make_executable(&wrapper);
    wrapper
}

fn captured_invocations(directory: &Path) -> Vec<Vec<String>> {
    let mut paths: Vec<_> = fs::read_dir(directory)
        .expect("capture directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("capture entries")
        .into_iter()
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .expect("captured invocation")
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

fn cargo_fixture_command(
    root: &Path,
    wrapper: &Path,
    capture: &Path,
    target_directory: &str,
) -> Command {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(root)
        .env("RUSTC_WRAPPER", wrapper)
        .env("TEMPER_RUSTC_CAPTURE_DIR", capture)
        .env_remove("RUSTFLAGS")
        .args([
            "build",
            "--release",
            "--locked",
            "--target",
            HOST,
            "--package",
            "temper-rustflags-app",
            "--bin",
            "temper-rustflags-app",
            "--target-dir",
        ])
        .arg(root.join(target_directory));
    command
}

fn target_invocation(invocations: &[Vec<String>]) -> &[String] {
    invocations
        .iter()
        .find(|arguments| {
            crate_name(arguments) == Some("temper_rustflags_app")
                && arguments.iter().any(|argument| argument == "--target")
        })
        .map(Vec::as_slice)
        .expect("target binary rustc invocation")
}

fn crate_name(arguments: &[String]) -> Option<&str> {
    arguments
        .windows(2)
        .find(|window| window[0] == "--crate-name")
        .map(|window| window[1].as_str())
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("wrapper metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make wrapper executable");
}
