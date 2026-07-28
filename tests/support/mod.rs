#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

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

    pub(crate) fn optimize(&self, extra_arguments: &[&str]) -> Output {
        self.optimize_workload(extra_arguments, &[OsStr::new("/bin/true")])
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
