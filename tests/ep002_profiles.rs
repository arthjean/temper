#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Fixture, stderr};

#[test]
fn raw_profiles_and_strict_merge_have_complete_evidence() {
    let fixture = Fixture::single("profile-evidence", true, true);
    let output = optimize_with_workload(&fixture, "exec \"$TEMPER_BINARY\"\n", &[]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    let profiles = manifest["pgo_training"]["raw_profile_files"]
        .as_array()
        .expect("raw profile evidence");
    assert!(!profiles.is_empty());
    let run_directory = fixture
        .run_directory()
        .canonicalize()
        .expect("canonical run");
    let mut recorded_paths = Vec::new();
    for profile in profiles {
        let path = PathBuf::from(profile["path"].as_str().expect("profile path"));
        assert_eq!(path, path.canonicalize().expect("canonical profile"));
        assert!(path.starts_with(run_directory.join("pgo/raw")));
        let metadata = fs::metadata(&path).expect("profile metadata");
        assert_eq!(profile["size_bytes"].as_u64(), Some(metadata.len()));
        assert_eq!(
            profile["sha256"].as_str(),
            Some(sha256_file(&path).as_str())
        );
        recorded_paths.push(path.to_string_lossy().into_owned());
    }
    let mut sorted_paths = recorded_paths.clone();
    sorted_paths.sort();
    assert_eq!(recorded_paths, sorted_paths);

    let merge = &manifest["pgo_training"]["merge"];
    assert_eq!(merge["outcome"], "merged");
    let arguments = merge["arguments"].as_array().expect("merge arguments");
    assert_eq!(arguments[0], "merge");
    assert_eq!(arguments[1], "--failure-mode=any");
    assert_eq!(arguments[2], "-o");
    let profdata = PathBuf::from(merge["profdata_path"].as_str().expect("profdata path"));
    let metadata = fs::metadata(&profdata).expect("profdata metadata");
    assert!(metadata.is_file());
    assert!(metadata.len() > 0);
    assert_eq!(merge["profdata_size_bytes"].as_u64(), Some(metadata.len()));
    assert_eq!(
        merge["profdata_sha256"].as_str(),
        Some(sha256_file(&profdata).as_str())
    );
}

#[test]
fn symlinked_raw_profile_rejects_before_merge() {
    let fixture = Fixture::single("symlinked-profile", true, true);
    let output = optimize_with_workload(
        &fixture,
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  profile_dir=${LLVM_PROFILE_FILE%/*}\n  ln -s /dev/null \"$profile_dir/escape.profraw\"\nfi\n",
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_profile_rejection(
        &fixture.manifest(),
        "pgo_profile_discovery_failed",
        "symbolic link",
    );
}

#[test]
fn non_regular_raw_profile_rejects_before_merge() {
    let fixture = Fixture::single("non-regular-profile", true, true);
    let output = optimize_with_workload(
        &fixture,
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  profile_dir=${LLVM_PROFILE_FILE%/*}\n  mkfifo \"$profile_dir/non-regular.profraw\"\nfi\n",
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_profile_rejection(
        &fixture.manifest(),
        "pgo_profile_discovery_failed",
        "regular files",
    );
}

#[test]
fn excessive_raw_profiles_reject_before_merge() {
    let fixture = Fixture::single("excessive-profiles", true, true);
    let output = optimize_with_workload(
        &fixture,
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  profile_dir=${LLVM_PROFILE_FILE%/*}\n  i=0\n  while [ \"$i\" -le 10000 ]; do\n    : > \"$profile_dir/$i.profraw\"\n    i=$((i + 1))\n  done\nfi\n",
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_profile_rejection(
        &fixture.manifest(),
        "pgo_profile_discovery_failed",
        "more than 10000",
    );
}

#[test]
fn corrupt_profile_rejects_only_pgo_during_strict_merge() {
    let fixture = Fixture::single("corrupt-profile", true, true);
    let output = optimize_with_workload(
        &fixture,
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  profile_dir=${LLVM_PROFILE_FILE%/*}\n  printf '%s\\n' corrupt > \"$profile_dir/corrupt.profraw\"\nfi\n",
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "pgo_profile_merge_failed"
    );
    assert_eq!(manifest["pgo_training"]["merge"]["outcome"], "rejected");
    assert!(
        manifest["pgo_training"]["merge"]["arguments"]
            .as_array()
            .is_some_and(|arguments| arguments[1] == "--failure-mode=any")
    );
    assert_static_candidates_survive(&manifest);
}

#[test]
fn preexisting_merged_profile_symlink_rejects_before_profdata_starts() {
    let fixture = Fixture::single("symlinked-merged-profile", true, true);
    let output = optimize_with_workload(
        &fixture,
        "\"$TEMPER_BINARY\"\nif [ -n \"${LLVM_PROFILE_FILE:-}\" ]; then\n  profile_dir=${LLVM_PROFILE_FILE%/*}\n  ln -s /dev/null \"$profile_dir/../merged.profdata\"\nfi\n",
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "pgo_profile_merge_failed"
    );
    assert!(
        manifest["pgo_training"]["merge"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("already exists before merge"))
    );
    assert!(
        manifest["pgo_training"]["merge"]["bounded_diagnostics"]
            .as_str()
            .is_some_and(str::is_empty)
    );
    assert_static_candidates_survive(&manifest);
}

#[test]
fn merge_diagnostics_reject_a_zero_exit_merge() {
    let fixture = Fixture::single("merge-diagnostics", true, true);
    let target_libdir = fixture.root.join("fake-toolchain/target/lib");
    let tool_directory = fixture.root.join("fake-toolchain/target/bin");
    fs::create_dir_all(&target_libdir).expect("fake target libdir");
    fs::create_dir_all(&tool_directory).expect("fake tool directory");

    let rustc_wrapper = fixture.root.join("rustc-wrapper");
    fs::write(
        &rustc_wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '--print' ] && [ \"$2\" = 'target-libdir' ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nexec rustc \"$@\"\n",
            target_libdir.display()
        ),
    )
    .expect("write rustc wrapper");
    make_executable(&rustc_wrapper);

    let llvm_profdata = tool_directory.join("llvm-profdata");
    fs::write(
        &llvm_profdata,
        "#!/bin/sh\nif [ \"$1\" = '--version' ]; then\n  printf '%s\\n' 'llvm-profdata synthetic 1'\n  exit 0\nfi\noutput=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = '-o' ]; then\n    shift\n    output=$1\n  fi\n  shift\ndone\nprintf '%s\\n' synthetic > \"$output\"\nprintf '%s\\n' 'synthetic merge diagnostic' >&2\n",
    )
    .expect("write llvm-profdata wrapper");
    make_executable(&llvm_profdata);

    let output = optimize_with_workload(
        &fixture,
        "exec \"$TEMPER_BINARY\"\n",
        &[("RUSTC", rustc_wrapper.as_os_str())],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fixture.manifest();
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "pgo_profile_merge_failed"
    );
    assert_eq!(manifest["pgo_training"]["merge"]["outcome"], "rejected");
    assert!(
        manifest["pgo_training"]["merge"]["bounded_diagnostics"]
            .as_str()
            .is_some_and(|diagnostics| diagnostics.contains("synthetic merge diagnostic"))
    );
    assert_static_candidates_survive(&manifest);
}

fn optimize_with_workload(
    fixture: &Fixture,
    body: &str,
    environment: &[(&str, &std::ffi::OsStr)],
) -> Output {
    let tracked_before = tracked_input_sha256(&fixture.root);
    let workload = fixture.root.join("profile-workload");
    fs::write(&workload, format!("#!/bin/sh\nset -eu\n{body}")).expect("write profile workload");
    make_executable(&workload);
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().expect("run profile fixture");
    assert_eq!(
        tracked_input_sha256(&fixture.root),
        tracked_before,
        "profile case changed tracked source, manifest, or lockfile"
    );
    output
}

fn assert_profile_rejection(manifest: &Value, reason: &str, message_fragment: &str) {
    assert_eq!(manifest["pgo_training"]["rejection_reason"], reason);
    assert!(
        manifest["pgo_training"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(message_fragment))
    );
    assert!(manifest["pgo_training"]["merge"].is_null());
    assert_static_candidates_survive(manifest);
}

fn assert_static_candidates_survive(manifest: &Value) {
    let strategies = manifest["strategies"].as_array().expect("strategies");
    assert_eq!(strategies[0]["build"]["outcome"], "built");
    assert_eq!(strategies[1]["build"]["outcome"], "built");
    let pgo = strategies
        .iter()
        .find(|strategy| strategy["identity"] == "pgo")
        .expect("PGO strategy");
    assert!(pgo["build"].is_null());
    assert!(pgo["screening"].is_null());
    assert_ne!(manifest["selected_candidate"], "pgo");
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}

fn sha256_file(path: &Path) -> String {
    let mut file = fs::File::open(path).expect("open hashed file");
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).expect("read hashed file");
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    format!("{:x}", digest.finalize())
}

fn tracked_input_sha256(root: &Path) -> String {
    let mut digest = Sha256::new();
    for relative in ["Cargo.toml", "Cargo.lock", "src/main.rs"] {
        digest.update(relative.as_bytes());
        digest.update(fs::read(root.join(relative)).expect("read tracked fixture input"));
    }
    format!("{:x}", digest.finalize())
}
