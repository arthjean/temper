#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::process::Command;

use support::{Fixture, stderr};

#[test]
fn builds_screens_and_records_the_fixed_strategy_order() {
    let fixture = Fixture::single("bounded-search", true, true);
    let output = fixture.optimize(&[]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_ne!(manifest["final_decision"], "pending");
    let completed_phases = manifest["completed_phases"]
        .as_array()
        .expect("completed phases");
    assert_eq!(
        &completed_phases[..7],
        serde_json::json!([
            "preflight",
            "baseline_build",
            "static_builds",
            "screening",
            "pgo_training",
            "pgo_build",
            "candidate_selection"
        ])
        .as_array()
        .expect("expected phases")
    );
    assert!(completed_phases.iter().any(|phase| phase == "confirmation"));
    assert_eq!(manifest["baseline"]["strategy"], "baseline");
    assert_eq!(manifest["baseline"]["outcome"], "built");
    assert_eq!(
        manifest["baseline"]["cargo_config_overrides"],
        serde_json::json!([])
    );

    let strategies = manifest["strategies"].as_array().expect("strategy records");
    assert_eq!(strategies.len(), 3);
    assert_eq!(strategies[0]["identity"], "thin-lto");
    assert_eq!(strategies[1]["identity"], "fat-lto-cgu1");
    assert_eq!(strategies[2]["identity"], "pgo");
    assert_eq!(
        strategies[0]["build"]["cargo_config_overrides"],
        serde_json::json!(["profile.release.lto=\"thin\""])
    );
    assert_eq!(
        strategies[1]["build"]["cargo_config_overrides"],
        serde_json::json!([
            "profile.release.lto=\"fat\"",
            "profile.release.codegen-units=1"
        ])
    );
    for (record, identity) in strategies[..2].iter().zip(["thin-lto", "fat-lto-cgu1"]) {
        assert_eq!(record["build"]["outcome"], "built");
        assert_eq!(
            record["screening"]["sample_durations_ns"]
                .as_array()
                .map(Vec::len),
            Some(7)
        );
        assert!(
            record["build"]["target_directory"]
                .as_str()
                .is_some_and(|path| path.ends_with(identity))
        );
        assert!(
            record["build"]["cargo_arguments"]
                .as_array()
                .is_some_and(|arguments| arguments.iter().any(|value| value == "--locked"))
        );
    }
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "pgo_training_produced_no_profraw_files"
    );
}

#[test]
fn ambient_rustflags_reject_only_pgo() {
    let fixture = Fixture::single("flag-boundary", true, true);
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("RUSTFLAGS", "-Ctarget-cpu=x86-64")
        .args(["temper", "optimize", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run flag boundary");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert!(
        manifest["pgo_training"]["rejection_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("RUSTFLAGS"))
    );
    let strategies = manifest["strategies"].as_array().expect("strategy records");
    assert_eq!(strategies[0]["build"]["outcome"], "built");
    assert_eq!(strategies[1]["build"]["outcome"], "built");
    assert_eq!(strategies[2]["rejection_reason"], "pgo_training_rejected");
}

#[test]
fn rejects_a_symlinked_temper_root_before_building() {
    let fixture = Fixture::single("symlink-boundary", true, true);
    let redirected = tempfile::tempdir().expect("redirect target");
    symlink(redirected.path(), fixture.root.join(".temper")).expect("symlink .temper");

    let output = fixture.optimize(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("must not be a symbolic link"));
    assert!(
        fs::read_dir(redirected.path())
            .expect("redirect directory")
            .next()
            .is_none()
    );
}

#[test]
fn a_static_build_failure_does_not_stop_remaining_strategies() {
    let fixture = Fixture::single("isolated-failure", true, true);
    let real_cargo = std::env::var("CARGO").expect("Cargo test runner path");
    assert!(!real_cargo.contains('\''));
    let wrapper = fixture.root.join("cargo-wrapper");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    'profile.release.lto=\"thin\"') exit 97 ;;\n    *-Cprofile-generate=*) exit 98 ;;\n  esac\ndone\nexec '{real_cargo}' \"$@\"\n"
        ),
    )
    .expect("write Cargo wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run isolated failure");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    let strategies = manifest["strategies"].as_array().expect("strategy records");
    assert_eq!(strategies[0]["identity"], "thin-lto");
    assert_eq!(strategies[0]["build_failure"]["outcome"], "failed");
    assert_eq!(strategies[0]["rejection_reason"], "static_build_failed");
    assert_eq!(strategies[1]["identity"], "fat-lto-cgu1");
    assert_eq!(strategies[1]["build"]["outcome"], "built");
    assert_eq!(
        manifest["pgo_training"]["rejection_reason"],
        "pgo_instrumentation_failed"
    );
    assert_eq!(
        manifest["pgo_training"]["instrumentation_failure"]["outcome"],
        "failed"
    );
    assert_ne!(manifest["final_decision"], "pending");
}

#[test]
fn pgo_never_falls_back_to_path_for_llvm_profdata() {
    let fixture = Fixture::single("toolchain-boundary", true, true);
    let target_libdir = fixture.root.join("fake-toolchain/host/lib");
    fs::create_dir_all(&target_libdir).expect("fake target libdir");
    let rustc_wrapper = fixture.root.join("rustc-wrapper");
    fs::write(
        &rustc_wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '--print' ] && [ \"$2\" = 'target-libdir' ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nexec rustc \"$@\"\n",
            target_libdir.display()
        ),
    )
    .expect("write rustc wrapper");
    let mut permissions = fs::metadata(&rustc_wrapper)
        .expect("rustc wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&rustc_wrapper, permissions).expect("make rustc wrapper executable");

    let path_bin = fixture.root.join("path-bin");
    fs::create_dir(&path_bin).expect("PATH directory");
    let path_profdata = path_bin.join("llvm-profdata");
    fs::write(&path_profdata, "#!/bin/sh\nexit 0\n").expect("PATH llvm-profdata");
    let mut permissions = fs::metadata(&path_profdata)
        .expect("PATH tool metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path_profdata, permissions).expect("make PATH tool executable");
    let inherited_path = std::env::var_os("PATH").expect("PATH");
    let mut path_entries = vec![path_bin];
    path_entries.extend(std::env::split_paths(&inherited_path));
    let combined_path = std::env::join_paths(path_entries).expect("combined PATH");

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("RUSTC", &rustc_wrapper)
        .env("PATH", combined_path)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .args(["--", "/bin/true"])
        .output()
        .expect("run toolchain boundary");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "rejected");
    assert!(
        manifest["pgo_training"]["rejection_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("fake-toolchain/host/bin/llvm-profdata"))
    );
    assert!(
        manifest["pgo_training"]["remediation"]
            .as_str()
            .is_some_and(|hint| hint.contains("llvm-tools-preview"))
    );
}

#[test]
fn trains_merges_rebuilds_and_screens_pgo() {
    let fixture = Fixture::single("pgo-search", true, true);
    let cargo_config = fixture.root.join(".cargo");
    fs::create_dir(&cargo_config).expect("Cargo config directory");
    fs::write(
        cargo_config.join("config.toml"),
        "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"-Cdebuginfo=0\"]\n",
    )
    .expect("target-specific rustflags");
    let workload = fixture.root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n").expect("write workload");
    let mut permissions = fs::metadata(&workload)
        .expect("workload metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&workload, permissions).expect("make workload executable");

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload)
        .output()
        .expect("run PGO search");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    assert_eq!(
        manifest["pgo_training"]["instrumentation_build"]["outcome"],
        "built"
    );
    assert!(
        manifest["pgo_training"]["prerequisites"]["preserved_target_rustflags"]
            .as_array()
            .is_some_and(|flags| flags.iter().any(|value| value == "-Cdebuginfo=0"))
    );
    assert!(
        manifest["pgo_training"]["instrumentation_build"]["cargo_config_overrides"]
            .as_array()
            .is_some_and(|overrides| overrides.iter().all(|value| {
                value
                    .as_str()
                    .is_none_or(|value| !value.contains("-Cdebuginfo=0"))
            }))
    );
    assert!(
        manifest["pgo_training"]["training_duration_ns"]
            .as_u64()
            .is_some_and(|duration| duration > 0)
    );
    assert!(
        manifest["pgo_training"]["raw_profile_files"]
            .as_array()
            .is_some_and(|profiles| !profiles.is_empty())
    );
    assert_eq!(manifest["pgo_training"]["merge"]["outcome"], "merged");
    assert!(
        manifest["pgo_training"]["merge"]["profdata_sha256"]
            .as_str()
            .is_some_and(|checksum| checksum.len() == 64)
    );

    let pgo = manifest["strategies"]
        .as_array()
        .and_then(|strategies| strategies.iter().find(|record| record["identity"] == "pgo"))
        .expect("PGO strategy record");
    assert_eq!(pgo["build"]["outcome"], "built");
    assert_eq!(
        pgo["screening"]["sample_durations_ns"]
            .as_array()
            .map(Vec::len),
        Some(7)
    );
    assert!(
        pgo["build"]["cargo_config_overrides"]
            .as_array()
            .is_some_and(|overrides| overrides.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|value| value.contains("-Cprofile-use="))
            }))
    );
}
