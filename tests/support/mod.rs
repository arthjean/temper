#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "shared integration test assertions"
)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

pub(crate) struct Fixture {
    _temporary_directory: TempDir,
    pub(crate) root: PathBuf,
}

impl Fixture {
    pub(crate) fn single(package: &str, valid_source: bool, with_lockfile: bool) -> Self {
        let temporary_directory = tempfile::tempdir().expect("create fixture directory");
        let root = temporary_directory.path().to_path_buf();
        fs::create_dir(root.join("src")).expect("create fixture source directory");
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .expect("write fixture manifest");
        let source = if valid_source {
            "fn main() { println!(\"fixture\"); }\n"
        } else {
            "fn main() { this does not compile }\n"
        };
        fs::write(root.join("src/main.rs"), source).expect("write fixture source");
        fs::write(root.join(".gitignore"), ".temper/\n").expect("write fixture gitignore");
        if with_lockfile {
            write_lockfile(&root, &[package]);
        }
        initialize_git(&root);
        Self {
            _temporary_directory: temporary_directory,
            root,
        }
    }

    pub(crate) fn workspace() -> Self {
        let temporary_directory = tempfile::tempdir().expect("create fixture directory");
        let root = temporary_directory.path().to_path_buf();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"3\"\n",
        )
        .expect("write workspace manifest");
        fs::write(root.join(".gitignore"), ".temper/\n").expect("write fixture gitignore");
        for (package, binary) in [("alpha", "alpha"), ("beta", "beta-tool")] {
            let package_root = root.join(package);
            fs::create_dir_all(package_root.join("src")).expect("create member source");
            fs::write(
                package_root.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[[bin]]\nname = \"{binary}\"\npath = \"src/main.rs\"\n"
                ),
            )
            .expect("write member manifest");
            fs::write(
                package_root.join("src/main.rs"),
                format!("fn main() {{ println!(\"{binary}\"); }}\n"),
            )
            .expect("write member source");
        }
        write_lockfile(&root, &["alpha", "beta"]);
        initialize_git(&root);
        Self {
            _temporary_directory: temporary_directory,
            root,
        }
    }

    pub(crate) fn workspace_with_packages(package_count: usize) -> Self {
        assert!(package_count > 0, "workspace requires at least one package");
        let temporary_directory = tempfile::tempdir().expect("create fixture directory");
        let root = temporary_directory.path().to_path_buf();
        let packages: Vec<String> = (0..package_count)
            .map(|index| format!("package-{index:03}"))
            .collect();
        let members = packages
            .iter()
            .map(|package| format!("\"{package}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nmembers = [{members}]\nresolver = \"3\"\n"),
        )
        .expect("write large workspace manifest");
        fs::write(root.join(".gitignore"), ".temper/\n").expect("write fixture gitignore");
        for package in &packages {
            let package_root = root.join(package);
            fs::create_dir_all(package_root.join("src")).expect("create package source");
            fs::write(
                package_root.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                ),
            )
            .expect("write package manifest");
            fs::write(
                package_root.join("src/main.rs"),
                format!("fn main() {{ println!(\"{package}\"); }}\n"),
            )
            .expect("write package source");
        }
        let package_names: Vec<&str> = packages.iter().map(String::as_str).collect();
        write_lockfile(&root, &package_names);
        initialize_git(&root);
        Self {
            _temporary_directory: temporary_directory,
            root,
        }
    }

    pub(crate) fn checked_in_workspace() -> Self {
        Self::checked_in_fixture("workspace")
    }

    pub(crate) fn checked_in_fixture(name: &str) -> Self {
        let temporary_directory = tempfile::tempdir().expect("create fixture directory");
        let root = temporary_directory.path().to_path_buf();
        copy_directory(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
            &root,
        );
        initialize_git(&root);
        Self {
            _temporary_directory: temporary_directory,
            root,
        }
    }

    pub(crate) fn nested_single(package: &str) -> Self {
        let temporary_directory = tempfile::tempdir().expect("create fixture directory");
        let root = temporary_directory.path().join("workspace");
        fs::create_dir_all(root.join("src")).expect("create nested fixture source");
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .expect("write nested fixture manifest");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write nested fixture source");
        write_lockfile(&root, &[package]);
        initialize_git(temporary_directory.path());
        Self {
            _temporary_directory: temporary_directory,
            root,
        }
    }

    /// Runs one optimization with the default measurable workload.
    ///
    /// The workload sleeps rather than exiting immediately: measurement-v1
    /// rejects a screening cohort whose relative median absolute deviation
    /// exceeds 10%, and a zero-duration workload makes that ratio pure process
    /// noise on a loaded host. The floor bounds the ratio without changing any
    /// decision, because every artifact runs the same workload.
    pub(crate) fn optimize(&self, extra_arguments: &[&str]) -> Output {
        self.optimize_workload(
            extra_arguments,
            &[OsStr::new("/bin/sleep"), OsStr::new("0.02")],
        )
    }

    pub(crate) fn optimize_workload(
        &self,
        extra_arguments: &[&str],
        workload: &[&OsStr],
    ) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
        command
            .current_dir(&self.root)
            .arg("temper")
            .arg("optimize")
            .arg("--manifest-path")
            .arg(self.root.join("Cargo.toml"))
            .args(extra_arguments)
            .arg("--")
            .args(workload);
        command.output().expect("run cargo-temper")
    }

    pub(crate) fn manifest(&self) -> Value {
        let runs = self.root.join(".temper/runs");
        let entries: Vec<_> = fs::read_dir(runs)
            .expect("read runs directory")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("read run entries");
        assert_eq!(entries.len(), 1, "one run should be persisted");
        let manifest = entries[0].path().join("run.json");
        let contents = fs::read_to_string(manifest).expect("read run manifest");
        serde_json::from_str(&contents).expect("parse run manifest")
    }

    pub(crate) fn run_directory(&self) -> PathBuf {
        let entries: Vec<_> = fs::read_dir(self.root.join(".temper/runs"))
            .expect("read runs directory")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("read run entries");
        assert_eq!(entries.len(), 1, "one run should be persisted");
        entries[0].path()
    }

    pub(crate) fn latest(&self) -> Option<Value> {
        let path = self.root.join(".temper/latest.json");
        path.exists().then(|| {
            serde_json::from_str(&fs::read_to_string(path).expect("read latest pointer"))
                .expect("parse latest pointer")
        })
    }
}

pub(crate) fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cargo-temper"))
}

/// Asserts one interposed stage recorded the expected private protocol and a
/// complete capture aggregate that injected the phase controls on target
/// compilations only.
pub(crate) fn assert_interposed_stage(build: &Value, stage: &str, injected: &[&str]) {
    let injected = serde_json::to_value(injected).expect("injected flag list");
    assert_eq!(build["outcome"], "built", "{stage} must have been built");
    let interposition = &build["interposition"];
    assert_eq!(interposition["protocol"], "temper-rustc-shim-1");
    assert_eq!(interposition["normalization"], 1);
    assert_eq!(interposition["stage"], stage);
    assert_eq!(interposition["injected_flags"], injected);
    let evidence = &build["compiler_evidence"];
    assert_eq!(evidence["stage"], stage);
    assert_eq!(evidence["injected_flags"], injected);
    let target_compilations = evidence["target_compilations"]
        .as_u64()
        .expect("target compilation count");
    assert!(
        target_compilations > 0,
        "{stage} observed no target compilation"
    );
    let expected_injections = if injected == serde_json::json!([]) {
        0
    } else {
        target_compilations
    };
    assert_eq!(
        evidence["injected_invocations"].as_u64(),
        Some(expected_injections),
        "{stage} injected the wrong number of invocations"
    );
    assert!(
        evidence["capture_digest"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
    assert!(
        evidence["record_count"]
            .as_u64()
            .is_some_and(|count| count >= target_compilations)
    );
    // Persisted evidence stays inside the documented per-stage budget.
    assert!(
        evidence["record_count"]
            .as_u64()
            .is_some_and(|count| count <= 10_000)
    );
    assert!(
        evidence["capture_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes <= 32 * 1024 * 1024)
    );
}

/// Every capture record one interposed stage persisted, ordered by record
/// identity exactly as the parent aggregates them.
pub(crate) fn capture_records(run_directory: &Path, stage: &str) -> Vec<Value> {
    let directory = run_directory.join("captures").join(stage);
    let mut paths: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {} captures: {error}", directory.display()))
        .map(|entry| entry.expect("capture entry").path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            serde_json::from_slice(&fs::read(path).expect("read capture record"))
                .expect("parse capture record")
        })
        .collect()
}

/// The digest a shim record carries for one whole compiler argument.
///
/// It reproduces the shim's length framing independently, so an ordered
/// argument assertion never trusts the digest the shim wrote about itself.
pub(crate) fn argument_digest(argument: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = argument.as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(1_u64.to_be_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    let mut digest = format!("{:x}", hasher.finalize());
    digest.truncate(16);
    digest
}

pub(crate) fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

fn write_lockfile(root: &Path, packages: &[&str]) {
    let packages = packages
        .iter()
        .map(|package| format!("[[package]]\nname = \"{package}\"\nversion = \"0.1.0\"\n"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        root.join("Cargo.lock"),
        format!("# This file is automatically @generated by Cargo.\nversion = 4\n\n{packages}"),
    )
    .expect("write fixture lockfile");
}

fn copy_directory(source: PathBuf, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied fixture directory");
    for entry in fs::read_dir(source)
        .expect("read checked-in fixture")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("checked-in fixture entries")
    {
        let destination = destination.join(entry.file_name());
        if entry.file_type().expect("fixture file type").is_dir() {
            copy_directory(entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy checked-in fixture file");
        }
    }
}

fn initialize_git(root: &Path) {
    run_git(root, &["init", "--quiet"]);
    run_git(root, &["config", "user.name", "Temper Tests"]);
    run_git(
        root,
        &["config", "user.email", "temper-tests@example.invalid"],
    );
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "--quiet", "-m", "fixture"]);
}

fn run_git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .status()
        .expect("run git");
    assert!(status.success(), "git command failed: {arguments:?}");
}
