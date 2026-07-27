#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use support::{Fixture, command, stderr};

#[test]
fn output_limited_workload_is_rejected_and_persisted() {
    let fixture = Fixture::single("output-limit-fixture", true, true);
    let output = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/usr/bin/head", "-c", "1048577", "/dev/zero"])
        .output()
        .expect("run output-limited workload");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("stdout exceeded the 1 MiB limit"));
    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["failure"]["phase"], "baseline_measurement");
    assert_eq!(manifest["failure"]["outcome"], "output_limit");
    assert!(manifest["baseline_measurement"].is_null());
}

#[test]
fn timeout_marks_the_artifact_without_recording_partial_samples() {
    let fixture = Fixture::single("timeout-fixture", true, true);
    let output = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--timeout", "1", "--", "/bin/sleep", "30"])
        .output()
        .expect("run timed-out workload");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("exceeded its 1 second timeout"));
    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["failure"]["outcome"], "timeout");
    assert!(manifest["baseline_measurement"].is_null());
}

#[test]
fn sigint_marks_the_run_interrupted_and_removes_the_workload_tree() {
    let fixture = Fixture::single("interrupt-fixture", true, true);
    let marker = fixture.root.join(".temper-child-pid");
    let mut child = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--timeout", "30", "--", "/bin/sh", "-c"])
        .arg("sleep 30 & child=$!; printf '%s' \"$child\" > \"$1\"; wait")
        .arg("temper-test")
        .arg(&marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start interrupt fixture");

    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while !marker.exists() && Instant::now() < marker_deadline {
        assert!(
            child.try_wait().expect("inspect Temper process").is_none(),
            "Temper exited before starting the workload"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "workload did not expose its descendant PID"
    );

    let parent_pid = i32::try_from(child.id()).expect("PID fits Linux pid_t");
    assert_eq!(unsafe { libc::kill(parent_pid, libc::SIGINT) }, 0);
    let output = child.wait_with_output().expect("wait for interrupted run");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("interrupted by SIGINT"));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "interrupted");
    assert_eq!(manifest["failure"]["outcome"], "interrupted");
    assert!(manifest["baseline_measurement"].is_null());

    let descendant_pid = fs::read_to_string(marker)
        .expect("read descendant PID")
        .trim()
        .to_owned();
    let descendant = format!("/proc/{descendant_pid}");
    let cleanup_deadline = Instant::now() + Duration::from_secs(1);
    while Path::new(&descendant).exists() && Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !Path::new(&descendant).exists(),
        "descendant {descendant_pid} survived interrupted cleanup"
    );
}
