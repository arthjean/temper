#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::Fixture;

const MAXIMUM_PARENT_RSS_KIB: u64 = 150 * 1024;

#[test]
#[ignore = "reference-host NFR: measures parent CPU before the first Cargo build"]
fn hundred_package_planning_uses_less_than_one_parent_cpu_second() {
    let fixture = Fixture::workspace_with_packages(100);
    let marker = fixture.root.join(".temper-nfr-build-started");
    let wrapper = fixture.root.join("cargo-nfr-wrapper");
    fs::write(
        &wrapper,
        "#!/bin/sh\n\
         set -eu\n\
         if [ \"${1:-}\" = build ]; then\n\
           : > \"$TEMPER_NFR_BUILD_MARKER\"\n\
           sleep 30\n\
           exit 130\n\
         fi\n\
         exec \"$TEMPER_NFR_REAL_CARGO\" \"$@\"\n",
    )
    .expect("write Cargo NFR wrapper");
    make_executable(&wrapper);

    let real_cargo = std::env::var_os("CARGO").expect("Cargo test runner path");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .process_group(0)
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .env("TEMPER_NFR_REAL_CARGO", real_cargo)
        .env("TEMPER_NFR_BUILD_MARKER", &marker)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args([
            "--package",
            "package-000",
            "--bin",
            "package-000",
            "--",
            "/bin/true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("start 100-package NFR fixture");

    let deadline = Instant::now() + Duration::from_secs(15);
    while !marker.exists() {
        assert!(
            child.try_wait().expect("inspect NFR fixture").is_none(),
            "Temper exited before the first build"
        );
        assert!(
            Instant::now() < deadline,
            "100-package planning did not reach the first build"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let parent_cpu_seconds = process_cpu_seconds(child.id());
    let process_group = i32::try_from(child.id()).expect("PID fits Linux pid_t");
    assert_eq!(unsafe { libc::kill(-process_group, libc::SIGINT) }, 0);
    let status = child.wait().expect("wait for NFR fixture");
    assert_eq!(status.code(), Some(1));
    assert!(
        parent_cpu_seconds < 1.0,
        "100-package metadata parsing and plan construction used {parent_cpu_seconds:.3} parent CPU seconds"
    );
    eprintln!(
        "reference-host evidence: 100-package parent planning CPU = {parent_cpu_seconds:.3} s"
    );
}

#[test]
#[ignore = "reference-host NFR: samples direct Temper RSS during a complete four-artifact run"]
fn four_artifact_run_stays_below_parent_rss_limit() {
    let fixture = Fixture::single("rss-nfr", true, true);
    let mut child = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/sh", "-c", "sleep 0.01; exec \"$TEMPER_BINARY\""])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start RSS NFR fixture");

    let mut maximum_rss_kib = 0_u64;
    let status = loop {
        maximum_rss_kib = maximum_rss_kib.max(process_rss_kib(child.id()).unwrap_or(0));
        if let Some(status) = child.try_wait().expect("inspect RSS NFR fixture") {
            break status;
        }
        thread::sleep(Duration::from_millis(2));
    };
    assert!(status.success(), "complete RSS NFR fixture failed");

    let manifest = fixture.manifest();
    let built_artifacts = usize::from(manifest["baseline"]["outcome"] == "built")
        + manifest["strategies"]
            .as_array()
            .expect("strategy records")
            .iter()
            .filter(|strategy| strategy["build"]["outcome"] == "built")
            .count();
    let sample_count = recorded_sample_count(&manifest);
    assert_eq!(
        built_artifacts, 4,
        "reference run must build four artifacts"
    );
    assert!(
        sample_count <= 100,
        "fixed v0.0.1 protocol exceeded its 100-sample NFR envelope"
    );
    assert!(
        maximum_rss_kib < MAXIMUM_PARENT_RSS_KIB,
        "Temper parent RSS reached {maximum_rss_kib} KiB"
    );
    eprintln!(
        "reference-host evidence: parent max RSS = {maximum_rss_kib} KiB; artifacts = {built_artifacts}; samples = {sample_count}"
    );
}

fn process_cpu_seconds(pid: u32) -> f64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("read parent process stat");
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .expect("parse parent process name");
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let user_ticks: u64 = fields[11].parse().expect("parse parent user ticks");
    let system_ticks: u64 = fields[12].parse().expect("parse parent system ticks");
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    assert!(
        ticks_per_second > 0,
        "Linux clock tick frequency unavailable"
    );
    (user_ticks + system_ticks) as f64 / ticks_per_second as f64
}

fn process_rss_kib(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse().ok())
    })
}

fn recorded_sample_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(recorded_sample_count).sum(),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| {
                if matches!(
                    key.as_str(),
                    "sample_durations_ns" | "baseline_durations_ns" | "candidate_durations_ns"
                ) {
                    value.as_array().map_or(0, Vec::len)
                } else {
                    recorded_sample_count(value)
                }
            })
            .sum(),
        _ => 0,
    }
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("wrapper metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make wrapper executable");
}
