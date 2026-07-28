#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_metadata::{Message, TargetKind};
use support::{Fixture, stderr};

const HOST_TARGET: &str = "x86_64-unknown-linux-gnu";
const ENCODED_SEPARATOR: &str = "\u{1f}";

#[test]
fn missing_function_contract_is_structured_and_narrow() {
    let evidence = reproduce_missing_function_diagnostic();
    assert!(
        evidence.optimized.status.success(),
        "{}",
        stderr(&evidence.optimized)
    );

    let raw_stdout = String::from_utf8_lossy(&evidence.optimized.stdout);
    let raw_lines: Vec<&str> = raw_stdout
        .lines()
        .filter(|line| line.contains("\"reason\":\"compiler-message\""))
        .collect();
    let diagnostics: Vec<_> = Message::parse_stream(Cursor::new(&evidence.optimized.stdout))
        .filter_map(Result::ok)
        .filter_map(|message| match message {
            Message::CompilerMessage(message) => Some(message),
            _ => None,
        })
        .collect();
    let rendered: Vec<String> = diagnostics
        .iter()
        .map(|message| {
            format!(
                "package={} target={} kind={:?} level={:?} code={:?} message={} rendered={:?}",
                message.package_id,
                message.target.name,
                message.target.kind,
                message.message.level,
                message.message.code.as_ref().map(|code| &code.code),
                message.message.message,
                message.message.rendered,
            )
        })
        .collect();
    let missing: Vec<_> = diagnostics
        .iter()
        .filter(|message| is_missing_profile_message(message))
        .collect();
    assert_eq!(
        missing.len(),
        1,
        "raw compiler-message lines:\n{}\nparsed diagnostics:\n{}",
        raw_lines.join("\n"),
        rendered.join("\n")
    );
    assert_eq!(missing[0].target.name, "pgo-missing-function");
    assert_eq!(missing[0].target.kind.as_slice(), [TargetKind::Bin]);
    assert!(
        raw_lines
            .iter()
            .any(|line| line.contains("no profile data available for function"))
    );
    let unrelated = diagnostics
        .iter()
        .find(|message| {
            message
                .message
                .code
                .as_ref()
                .is_some_and(|code| code.code == "dead_code")
        })
        .expect("unrelated warning");
    assert!(!is_missing_profile_message(unrelated));
}

#[test]
fn optimized_missing_profile_diagnostic_rejects_only_pgo() {
    let fixture = Fixture::single("missing-profile-rejection", true, true);
    let workload = executable_workload(&fixture.root);
    let real_cargo = std::env::var("CARGO").expect("Cargo test runner path");
    assert!(!real_cargo.contains('\''));
    let wrapper = fixture.root.join("cargo-wrapper");
    let diagnostic = synthetic_missing_profile_message();
    assert!(!diagnostic.contains('\''));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"${{CARGO_ENCODED_RUSTFLAGS:-}}\" in\n  *-Cprofile-use=*)\n    '{real_cargo}' \"$@\"\n    status=$?\n    if [ \"$status\" -eq 0 ]; then\n      printf '%s\\n' '{diagnostic}'\n    fi\n    exit \"$status\"\n    ;;\n  *) exec '{real_cargo}' \"$@\" ;;\nesac\n"
        ),
    )
    .expect("write Cargo wrapper");
    make_executable(&wrapper);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload)
        .output()
        .expect("run missing-profile rejection");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["pgo_training"]["outcome"], "trained");
    let pgo = manifest["strategies"]
        .as_array()
        .and_then(|strategies| strategies.iter().find(|record| record["identity"] == "pgo"))
        .expect("PGO strategy record");
    assert_eq!(pgo["rejection_reason"], "pgo_missing_profile_data");
    assert_eq!(pgo["build_failure"]["reason"], "pgo_missing_profile_data");
    assert!(pgo["screening"].is_null());
    assert!(
        pgo["build_failure"]["compiler_diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| {
                diagnostics.iter().any(|diagnostic| {
                    diagnostic["level"] == "warning"
                        && diagnostic["code"].is_null()
                        && diagnostic["message"].as_str().is_some_and(|message| {
                            message.contains("no profile data available for function")
                        })
                })
            })
    );
    assert!(
        pgo["build_failure"]["environment_overrides"][0]["arguments"]
            .as_array()
            .is_some_and(|arguments| {
                arguments
                    .iter()
                    .any(|argument| argument == "-Cllvm-args=-pgo-warn-missing-function")
            })
    );
    assert_ne!(manifest["selected_candidate"], "pgo");
}

#[test]
fn confirmation_missing_profile_diagnostic_cannot_reach_promotion() {
    let fixture = Fixture::single("confirmation-missing-profile", true, true);
    let workload = fixture.root.join("prefer-pgo");
    fs::write(
        &workload,
        "#!/bin/sh\nset -eu\ncase \"$TEMPER_BINARY\" in\n  */target/baseline/*) sleep 0.03 ;;\n  */target/thin-lto/*) sleep 0.02 ;;\n  */target/fat-lto-cgu1/*) sleep 0.01 ;;\nesac\nexec \"$TEMPER_BINARY\"\n",
    )
    .expect("write PGO-selecting workload");
    make_executable(&workload);

    let real_cargo = std::env::var("CARGO").expect("Cargo test runner path");
    assert!(!real_cargo.contains('\''));
    let wrapper = fixture.root.join("confirmation-cargo-wrapper");
    let diagnostic = synthetic_missing_profile_message();
    assert!(!diagnostic.contains('\''));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nconfirmation=\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    */confirmation/pgo) confirmation=1 ;;\n  esac\ndone\ncase \"${{CARGO_ENCODED_RUSTFLAGS:-}}:$confirmation\" in\n  *-Cprofile-use=*:1)\n    '{real_cargo}' \"$@\"\n    status=$?\n    if [ \"$status\" -eq 0 ]; then\n      printf '%s\\n' '{diagnostic}'\n    fi\n    exit \"$status\"\n    ;;\n  *) exec '{real_cargo}' \"$@\" ;;\nesac\n"
        ),
    )
    .expect("write confirmation Cargo wrapper");
    make_executable(&wrapper);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
        .current_dir(&fixture.root)
        .env("CARGO", &wrapper)
        .args(["temper", "optimize", "--allow-dirty", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--")
        .arg(&workload)
        .output()
        .expect("run confirmation missing-profile rejection");
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = fixture.manifest();
    assert_eq!(manifest["confirmation"]["strategy"], "pgo");
    assert_eq!(manifest["confirmation"]["outcome"], "rejected");
    assert_eq!(
        manifest["confirmation"]["candidate_build_failure"]["reason"],
        "pgo_missing_profile_data"
    );
    assert!(
        manifest["confirmation"]["candidate_build_failure"]["compiler_diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| diagnostics.iter().any(|diagnostic| {
                diagnostic["message"].as_str().is_some_and(|message| {
                    message.contains("no profile data available for function")
                })
            }))
    );
    assert!(manifest["promotion"].is_null());
    assert!(fixture.latest().is_none());
}

struct MissingFunctionEvidence {
    optimized: Output,
}

fn reproduce_missing_function_diagnostic() -> MissingFunctionEvidence {
    let fixture = Fixture::checked_in_fixture("pgo-missing-function");
    let raw_directory = fixture.root.join("profiles/raw");
    fs::create_dir_all(&raw_directory).expect("raw profile directory");
    let instrumented_target = fixture.root.join("target/instrumented");
    let generate_flag = format!("-Cprofile-generate={}", raw_directory.display());
    let instrumented = cargo_build(&fixture.root, &instrumented_target, &[&generate_flag]);
    assert!(instrumented.status.success(), "{}", stderr(&instrumented));
    let executable = matching_executable(&instrumented);
    let profile_pattern = raw_directory.join("temper-%m-%p.profraw");
    let training = Command::new(executable)
        .env("LLVM_PROFILE_FILE", &profile_pattern)
        .output()
        .expect("run instrumented fixture");
    assert!(training.status.success(), "instrumented fixture failed");

    let mut profiles: Vec<PathBuf> = fs::read_dir(&raw_directory)
        .expect("read raw profiles")
        .map(|entry| entry.expect("raw profile entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "profraw")
        })
        .collect();
    profiles.sort();
    assert!(
        !profiles.is_empty(),
        "instrumented fixture produced no profile"
    );
    let profdata = fixture.root.join("profiles/merged.profdata");
    let mut merge = Command::new(adjacent_llvm_profdata(&fixture.root));
    merge
        .arg("merge")
        .arg("--failure-mode=any")
        .arg("-o")
        .arg(&profdata);
    merge.args(&profiles);
    let merge = merge.output().expect("merge fixture profiles");
    assert!(
        merge.status.success(),
        "llvm-profdata failed: {}",
        stderr(&merge)
    );

    fs::copy(
        fixture.root.join("src/optimized.rs"),
        fixture.root.join("src/main.rs"),
    )
    .expect("install incompatible optimized source");
    let optimized_target = fixture.root.join("target/optimized");
    let use_flag = format!("-Cprofile-use={}", profdata.display());
    let optimized = cargo_build(
        &fixture.root,
        &optimized_target,
        &[&use_flag, "-Cllvm-args=-pgo-warn-missing-function"],
    );
    MissingFunctionEvidence { optimized }
}

fn cargo_build(root: &Path, target_directory: &Path, rustflags: &[&str]) -> Output {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", rustflags.join(ENCODED_SEPARATOR))
        .args([
            "build",
            "--release",
            "--locked",
            "--target",
            HOST_TARGET,
            "--message-format=json",
            "--target-dir",
        ])
        .arg(target_directory)
        .output()
        .expect("build PGO fixture")
}

fn is_missing_profile_message(message: &cargo_metadata::CompilerMessage) -> bool {
    format!("{:?}", message.message.level).eq_ignore_ascii_case("warning")
        && message.message.code.is_none()
        && message
            .message
            .message
            .split_once(": ")
            .is_some_and(|(_, detail)| {
                detail.starts_with("no profile data available for function ")
            })
}

fn synthetic_missing_profile_message() -> String {
    serde_json::json!({
        "reason": "compiler-message",
        "package_id": "path+file:///fixture#missing-profile-rejection@0.1.0",
        "manifest_path": "/fixture/Cargo.toml",
        "target": {
            "kind": ["bin"],
            "crate_types": ["bin"],
            "name": "missing-profile-rejection",
            "src_path": "/fixture/src/main.rs",
            "edition": "2024",
            "doc": true,
            "doctest": false,
            "test": true
        },
        "message": {
            "rendered": "warning: fixture-cgu.0: no profile data available for function synthetic Hash = 1 up to 0 count discarded\n\n",
            "$message_type": "diagnostic",
            "children": [],
            "code": null,
            "level": "warning",
            "message": "fixture-cgu.0: no profile data available for function synthetic Hash = 1 up to 0 count discarded",
            "spans": []
        }
    })
    .to_string()
}

fn executable_workload(root: &Path) -> PathBuf {
    let workload = root.join("run-candidate");
    fs::write(&workload, "#!/bin/sh\nexec \"$TEMPER_BINARY\"\n")
        .expect("write executable workload");
    make_executable(&workload);
    workload
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make file executable");
}

fn matching_executable(output: &Output) -> PathBuf {
    let executables: Vec<PathBuf> = Message::parse_stream(Cursor::new(&output.stdout))
        .filter_map(Result::ok)
        .filter_map(|message| match message {
            Message::CompilerArtifact(artifact)
                if artifact.package_id.repr.starts_with("path+file:")
                    && artifact.target.name == "pgo-missing-function"
                    && artifact.target.kind.as_slice() == [TargetKind::Bin] =>
            {
                artifact.executable.map(|path| path.into_std_path_buf())
            }
            _ => None,
        })
        .collect();
    assert_eq!(executables.len(), 1, "one fixture executable");
    executables.into_iter().next().expect("fixture executable")
}

fn adjacent_llvm_profdata(workspace_root: &Path) -> PathBuf {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .current_dir(workspace_root)
        .args(["--print", "target-libdir", "--target", HOST_TARGET])
        .output()
        .expect("query rustc target libdir");
    assert!(output.status.success(), "rustc target-libdir failed");
    let target_libdir = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("target-libdir UTF-8")
            .trim(),
    );
    let llvm_profdata = target_libdir
        .parent()
        .expect("toolchain target root")
        .join("bin/llvm-profdata");
    let metadata = fs::metadata(&llvm_profdata).expect("adjacent llvm-profdata");
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    llvm_profdata
}
