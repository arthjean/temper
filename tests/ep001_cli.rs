#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use support::{command, stderr, stdout};

#[test]
fn help_documents_the_experimental_cli_contract() {
    let directory = tempfile::tempdir().expect("create test directory");
    let output = command()
        .current_dir(directory.path())
        .args(["temper", "optimize", "--help"])
        .output()
        .expect("run help");
    assert!(output.status.success());
    let help = stdout(&output);
    for expected in [
        "--package",
        "--bin",
        "--minimum-improvement",
        "--timeout",
        "--allow-dirty",
        "-- <WORKLOAD>...",
        "experimental",
    ] {
        assert!(help.contains(expected), "missing help text: {expected}");
    }
    assert!(!directory.path().join(".temper").exists());
}

#[test]
fn parse_errors_exit_with_code_two_without_starting_cargo() {
    let directory = tempfile::tempdir().expect("create test directory");
    let cargo_marker = directory.path().join("fake-cargo");
    let marker = directory.path().join("cargo-started");
    fs::write(
        &cargo_marker,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
    )
    .expect("write fake cargo");
    let mut permissions = fs::metadata(&cargo_marker)
        .expect("read fake cargo metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&cargo_marker, permissions).expect("make fake cargo executable");

    let missing = command()
        .current_dir(directory.path())
        .env("CARGO", &cargo_marker)
        .args(["temper", "optimize"])
        .output()
        .expect("run missing workload case");
    assert_eq!(missing.status.code(), Some(2));
    assert_eq!(
        stderr(&missing)
            .matches("Provide a workload executable after `--`.")
            .count(),
        1
    );

    let unknown = command()
        .current_dir(directory.path())
        .env("CARGO", &cargo_marker)
        .args(["temper", "optimize", "--unknown-option", "--", "/bin/true"])
        .output()
        .expect("run unknown option case");
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("unexpected argument '--unknown-option'"));
    assert!(!marker.exists(), "Cargo must not start for parse errors");
    assert!(!directory.path().join(".temper").exists());
}
