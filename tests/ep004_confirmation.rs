#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::{Fixture, command, stderr, stdout};

#[test]
fn checked_in_workspace_resolves_real_cargo_artifacts_without_source_mutation() {
    let fixture = Fixture::checked_in_workspace();
    let output = fixture.optimize(&["--package", "beta", "--bin", "beta-tool"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["target"]["package_name"], "beta");
    assert_eq!(manifest["target"]["binary_name"], "beta-tool");
    assert!(
        manifest["confirmation"]["baseline_build"]["executable_path"]
            .as_str()
            .is_some_and(|path| path.contains("/target/confirmation/baseline/"))
    );
    let status = Command::new("git")
        .current_dir(&fixture.root)
        .args(["status", "--porcelain=v1"])
        .output()
        .expect("inspect fixture source");
    assert!(
        status.stdout.is_empty(),
        "Temper changed the checked-in fixture copy: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn aa_confirmation_is_a_successful_no_improvement_json_run() {
    let fixture = Fixture::single("aa-confirmation", true, true);
    let output = fixture.optimize_workload(
        &["--json"],
        &[
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("sleep 0.02"),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let json = stdout(&output);
    assert!(!json.contains('\u{1b}'));
    let report: Value = serde_json::from_str(&json).expect("one final JSON object");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["status"], "no_improvement");
    assert_eq!(report["final_decision"], "no_improvement");
    assert_eq!(
        report["confirmation"]["measurement"]["baseline_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert_eq!(
        report["confirmation"]["measurement"]["candidate_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert!(report["promotion"].is_null());
    assert!(fixture.latest().is_none());
    assert!(!fixture.run_directory().join("best/artifact").exists());
    assert_no_failure_reserves(&fixture);
}

#[test]
fn confirmed_candidate_is_promoted_with_matching_identity_and_latest_pointer() {
    let fixture = Fixture::single("confirmed-promotion", true, true);
    // The absolute durations are large enough that per-invocation process
    // noise on a loaded test host stays well inside the measurement-v1
    // dispersion gate; the 5:1 contrast is what forces the decision.
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*|*/target/baseline/*) sleep 0.15 ;; \
         *) sleep 0.03 ;; \
         esac",
    );
    let output =
        fixture.optimize_workload(&[], &[OsStr::new("/bin/sh"), OsStr::new("-c"), workload]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).lines().count() <= 25);
    assert!(!stdout(&output).contains('\u{1b}'));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "confirmed");
    assert_eq!(manifest["final_decision"], "confirmed");
    assert_eq!(manifest["confirmation"]["outcome"], "accepted");
    assert!(
        manifest["confirmation"]["measurement"]["confidence_interval_95"]["upper"]
            .as_f64()
            .zip(manifest["confirmation"]["measurement"]["threshold_ratio"].as_f64())
            .is_some_and(|(upper, threshold)| upper <= threshold)
    );
    assert_eq!(
        manifest["promotion"]["source_strategy"],
        manifest["confirmation"]["strategy"]
    );
    assert_eq!(
        manifest["promotion"]["source_sha256"],
        manifest["confirmation"]["candidate_build"]["sha256"]
    );
    assert_eq!(
        manifest["promotion"]["promoted_sha256"],
        manifest["confirmation"]["candidate_build"]["sha256"]
    );
    assert_eq!(
        manifest["strategies"].as_array().map(Vec::len),
        Some(3),
        "screening records remain after promotion"
    );

    let promoted = manifest["promotion"]["promoted_path"]
        .as_str()
        .map(std::path::Path::new)
        .expect("promoted path");
    assert!(promoted.starts_with(fixture.run_directory().join("best")));
    assert!(promoted.is_file());
    let source = manifest["promotion"]["source_path"]
        .as_str()
        .map(std::path::Path::new)
        .expect("source path");
    assert_eq!(
        fs::metadata(promoted)
            .expect("promoted metadata")
            .permissions()
            .mode()
            & 0o7777,
        fs::metadata(source)
            .expect("source metadata")
            .permissions()
            .mode()
            & 0o7777
    );

    let latest = fixture.latest().expect("latest pointer");
    assert_eq!(latest["run_id"], manifest["run_id"]);
    assert_eq!(
        latest["artifact_path"],
        manifest["promotion"]["promoted_path"]
    );
    assert_eq!(
        latest["artifact_sha256"],
        manifest["promotion"]["promoted_sha256"]
    );
    assert_no_failure_reserves(&fixture);
}

#[test]
fn candidate_confirmation_failure_rejects_without_promotion() {
    let fixture = Fixture::single("confirmation-failure", true, true);
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*) sleep 0.01 ;; \
         */confirmation/*) exit 7 ;; \
         *) sleep 0.01 ;; \
         esac",
    );
    let output =
        fixture.optimize_workload(&[], &[OsStr::new("/bin/sh"), OsStr::new("-c"), workload]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "no_improvement");
    assert_eq!(
        manifest["confirmation"]["workload_failure"]["outcome"],
        "nonzero_exit"
    );
    assert_eq!(
        manifest["confirmation"]["rejection_reason"],
        "confirmation_workload_failed"
    );
    let selected = manifest["confirmation"]["strategy"]
        .as_str()
        .expect("selected strategy");
    assert!(
        manifest["strategies"]
            .as_array()
            .expect("strategies")
            .iter()
            .find(|strategy| strategy["identity"] == selected)
            .is_some_and(|strategy| strategy["rejection_reason"] == "confirmation_rejected")
    );
    assert!(manifest["promotion"].is_null());
    assert!(fixture.latest().is_none());
}

#[test]
fn latest_precommit_failure_rolls_back_the_promoted_artifact_and_manifest_state() {
    let fixture = Fixture::single("latest-precommit-failure", true, true);
    let temper_directory = fixture.root.join(".temper");
    fs::create_dir(&temper_directory).expect("create Temper directory");
    let previous_latest = b"{\"sentinel\":\"previous latest\"}\n";
    fs::write(temper_directory.join("latest.json"), previous_latest)
        .expect("seed previous latest pointer");

    // The absolute durations are large enough that per-invocation process
    // noise on a loaded test host stays well inside the measurement-v1
    // dispersion gate; the 5:1 contrast is what forces the decision.
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*|*/target/baseline/*) sleep 0.15 ;; \
         *) sleep 0.03 ;; \
         esac",
    );
    let child = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/sh", "-c"])
        .arg(workload)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start promotion failure fixture");
    let latest_temporary = temper_directory.join(format!(".latest.json.tmp-{}", child.id()));
    fs::write(&latest_temporary, b"block atomic latest publication")
        .expect("block latest temporary creation");

    let output = child
        .wait_with_output()
        .expect("wait for promotion failure fixture");
    assert_eq!(output.status.code(), Some(1));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["final_decision"], "failed");
    assert_eq!(manifest["failure"]["phase"], "promotion");
    assert!(manifest["promotion"].is_null());
    assert!(
        manifest["completed_phases"]
            .as_array()
            .is_some_and(|phases| phases.iter().all(|phase| phase != "promotion"))
    );
    assert!(!fixture.run_directory().join("best/artifact").exists());
    assert_no_failure_reserves(&fixture);
    assert_eq!(
        fs::read(temper_directory.join("latest.json")).expect("read previous latest"),
        previous_latest
    );
}

#[test]
fn promotion_reserve_creation_failure_activates_the_preallocated_failed_manifest() {
    let fixture = Fixture::single("promotion-reserve-failure", true, true);
    let temper_directory = fixture.root.join(".temper");
    fs::create_dir(&temper_directory).expect("create Temper directory");
    let previous_latest = b"{\"sentinel\":\"previous latest\"}\n";
    fs::write(temper_directory.join("latest.json"), previous_latest)
        .expect("seed previous latest pointer");
    // The absolute durations are large enough that per-invocation process
    // noise on a loaded test host stays well inside the measurement-v1
    // dispersion gate; the 5:1 contrast is what forces the decision.
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*|*/target/baseline/*) sleep 0.15 ;; \
         *) sleep 0.03 ;; \
         esac",
    );
    let child = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/sh", "-c"])
        .arg(workload)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start reserve failure fixture");
    let run_directory = wait_for_run_directory(&fixture.root, Duration::from_secs(5));
    let blocked_reserve = run_directory.join(".promotion-failure-reserve");
    fs::write(&blocked_reserve, b"force reserve creation failure")
        .expect("block promotion reserve creation");

    let output = child
        .wait_with_output()
        .expect("wait for reserve failure fixture");
    assert_eq!(output.status.code(), Some(1));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["final_decision"], "failed");
    assert_eq!(manifest["failure"]["phase"], "promotion");
    assert!(
        manifest["failure"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("preserves every previously durable"))
    );
    assert!(manifest["baseline"].is_object());
    assert_eq!(manifest["strategies"].as_array().map(Vec::len), Some(3));
    assert_eq!(manifest["confirmation"]["outcome"], "accepted");
    assert_eq!(
        manifest["confirmation"]["measurement"]["baseline_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert_eq!(
        manifest["confirmation"]["measurement"]["candidate_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    assert!(manifest["promotion"].is_null());
    assert!(!run_directory.join(".promotion-failure.json").exists());
    assert!(!run_directory.join("best/artifact").exists());
    assert_eq!(
        fs::read(temper_directory.join("latest.json")).expect("read previous latest"),
        previous_latest
    );
    fs::remove_file(blocked_reserve).expect("remove injected reserve blocker");
}

#[test]
fn sigint_during_promotion_is_end_to_end_atomic() {
    let fixture = Fixture::single("interrupted-promotion", true, true);
    fs::write(
        fixture.root.join("src/payload.bin"),
        vec![0x5a_u8; 16 * 1024 * 1024],
    )
    .expect("write large fixture payload");
    fs::write(
        fixture.root.join("src/main.rs"),
        "static PAYLOAD: &[u8; 16 * 1024 * 1024] = include_bytes!(\"payload.bin\");\n\
         fn main() {\n\
             let index = std::process::id() as usize % PAYLOAD.len();\n\
             println!(\"{}\", std::hint::black_box(PAYLOAD)[index]);\n\
         }\n",
    )
    .expect("write large fixture source");
    commit_fixture_change(&fixture.root);

    let temper_directory = fixture.root.join(".temper");
    fs::create_dir(&temper_directory).expect("create Temper directory");
    let previous_latest = b"{\"sentinel\":\"previous latest\"}\n";
    fs::write(temper_directory.join("latest.json"), previous_latest)
        .expect("seed previous latest pointer");

    // The absolute durations are large enough that per-invocation process
    // noise on a loaded test host stays well inside the measurement-v1
    // dispersion gate; the 5:1 contrast is what forces the decision.
    let workload = OsStr::new(
        "case \"$TEMPER_BINARY\" in \
         */confirmation/baseline/*|*/target/baseline/*) sleep 0.15 ;; \
         *) sleep 0.03 ;; \
         esac",
    );
    let mut child = command()
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/sh", "-c"])
        .arg(workload)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start interrupted promotion fixture");

    let temporary_name = format!(".artifact.tmp-{}", child.id());
    let deadline = Instant::now() + Duration::from_secs(30);
    let temporary_artifact = loop {
        if let Some(path) = find_promotion_temporary(&fixture.root, &temporary_name) {
            break path;
        }
        assert!(
            child
                .try_wait()
                .expect("inspect interrupted promotion fixture")
                .is_none(),
            "Temper exited before entering promotion"
        );
        assert!(
            Instant::now() < deadline,
            "Temper did not expose its temporary promotion artifact"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(temporary_artifact.exists());

    let pid = i32::try_from(child.id()).expect("PID fits Linux pid_t");
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let stop_deadline = Instant::now() + Duration::from_secs(2);
    while process_state(child.id()) != Some('T') {
        assert!(
            Instant::now() < stop_deadline,
            "Temper did not stop inside the promotion copy"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(temporary_artifact.exists());
    assert!(!fixture.run_directory().join("best/artifact").exists());
    assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    let output = child
        .wait_with_output()
        .expect("wait for interrupted promotion fixture");
    assert_eq!(output.status.code(), Some(1));

    let manifest = fixture.manifest();
    assert_eq!(manifest["status"], "interrupted");
    assert_eq!(manifest["final_decision"], "interrupted");
    assert_eq!(manifest["failure"]["phase"], "promotion");
    assert_eq!(manifest["failure"]["outcome"], "interrupted");
    assert!(manifest["promotion"].is_null());
    assert!(
        manifest["completed_phases"]
            .as_array()
            .is_some_and(|phases| phases.iter().all(|phase| phase != "promotion"))
    );
    assert!(!fixture.run_directory().join("best/artifact").exists());
    assert!(!temporary_artifact.exists());
    assert_no_failure_reserves(&fixture);
    assert_eq!(
        fs::read(temper_directory.join("latest.json")).expect("read previous latest"),
        previous_latest
    );
}

#[test]
fn failed_runs_still_emit_schema_v3_and_preflight_json_is_single_object() {
    let fixture = Fixture::single("nonzero-workload", true, true);
    let output = fixture.optimize_workload(&[], &[OsStr::new("/bin/false")]);
    assert_eq!(output.status.code(), Some(1));
    let manifest = fixture.manifest();
    assert_eq!(manifest["schema_version"], 3);
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["final_decision"], "failed");
    assert_eq!(manifest["failure"]["outcome"], "nonzero_exit");

    let missing_lock = Fixture::single("json-preflight", true, false);
    let output = command()
        .current_dir(&missing_lock.root)
        .args(["temper", "optimize", "--json", "--manifest-path"])
        .arg(missing_lock.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run JSON preflight failure");
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).expect("one preflight JSON object");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["status"], "failed");
    assert!(!stdout(&output).contains('\u{1b}'));
}

#[test]
fn successful_and_failed_runs_leave_target_inputs_byte_identical() {
    for (name, script, should_succeed) in [
        ("source-safety-success", "sleep 0.01", true),
        ("source-safety-failure", "exit 7", false),
    ] {
        let fixture = Fixture::single(name, true, true);
        let before = tracked_input_bytes(&fixture.root);

        let output = fixture.optimize_workload(
            &[],
            &[OsStr::new("/bin/sh"), OsStr::new("-c"), OsStr::new(script)],
        );

        assert_eq!(
            output.status.success(),
            should_succeed,
            "{name}: {}",
            stderr(&output)
        );
        assert_eq!(
            tracked_input_bytes(&fixture.root),
            before,
            "{name}: Temper changed a tracked target input"
        );
    }
}

fn commit_fixture_change(root: &std::path::Path) {
    for arguments in [
        ["add", "src/main.rs", "src/payload.bin"].as_slice(),
        ["commit", "--quiet", "-m", "large promotion fixture"].as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .status()
            .expect("commit fixture change");
        assert!(status.success(), "git command failed: {arguments:?}");
    }
}

fn tracked_input_bytes(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    ["Cargo.toml", "Cargo.lock", "src/main.rs"]
        .into_iter()
        .map(|relative| {
            let relative = PathBuf::from(relative);
            let bytes = fs::read(root.join(&relative)).expect("read tracked target input");
            (relative, bytes)
        })
        .collect()
}

fn find_promotion_temporary(root: &std::path::Path, name: &str) -> Option<PathBuf> {
    let runs = fs::read_dir(root.join(".temper/runs")).ok()?;
    for run in runs.filter_map(Result::ok) {
        let candidate = run.path().join("best").join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn wait_for_run_directory(root: &std::path::Path, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut runs) = fs::read_dir(root.join(".temper/runs"))
            && let Some(Ok(run)) = runs.next()
        {
            return run.path();
        }
        assert!(
            Instant::now() < deadline,
            "Temper did not create a run directory"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn promotion_failure_reserve(fixture: &Fixture) -> PathBuf {
    fixture.run_directory().join(".promotion-failure-reserve")
}

fn assert_no_failure_reserves(fixture: &Fixture) {
    assert!(!promotion_failure_reserve(fixture).exists());
    assert!(
        !fixture
            .run_directory()
            .join(".promotion-failure.json")
            .exists()
    );
}

fn process_state(pid: u32) -> Option<char> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("State:")
                .and_then(|state| state.split_whitespace().next())
                .and_then(|state| state.chars().next())
        })
}
