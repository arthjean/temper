use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::error::{Result, TemperError};
use crate::hash::sha256_file;

pub(crate) const SUPPORTED_HOST: &str = "x86_64-unknown-linux-gnu";
const VERSION_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceReproducibility {
    Clean,
    Dirty,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreflightRecord {
    pub(crate) host_triple: String,
    pub(crate) kernel: String,
    pub(crate) cpu_model: String,
    pub(crate) logical_core_count: usize,
    pub(crate) cargo_version: String,
    pub(crate) rustc_version: String,
    pub(crate) manifest_path: PathBuf,
    pub(crate) lockfile_path: PathBuf,
    pub(crate) lockfile_sha256: String,
}

pub(crate) struct Preflight {
    pub(crate) record: PreflightRecord,
    pub(crate) source_reproducibility: SourceReproducibility,
}

pub(crate) fn run(
    manifest_path: &Path,
    allow_dirty: bool,
    requested_target: Option<&str>,
) -> Result<Preflight> {
    validate_requested_target(requested_target)?;
    let cargo_version = version_output(cargo_program(), &["-Vv"], "Cargo")?;
    let rustc_version = version_output(rustc_program(), &["-Vv"], "rustc")?;
    let host_triple = parse_host_triple(&rustc_version)?;
    validate_supported_host(&host_triple)?;

    let manifest_path = fs::canonicalize(manifest_path).map_err(|error| {
        TemperError::new(format!(
            "Cargo manifest {} is unavailable: {error}",
            manifest_path.display()
        ))
    })?;
    if !manifest_path.is_file() {
        return Err(TemperError::new(format!(
            "Cargo manifest {} is not a file.",
            manifest_path.display()
        )));
    }

    let lockfile_path = find_lockfile(&manifest_path)
        .ok_or_else(|| TemperError::new("Temper v0.0.1 requires an existing Cargo.lock."))?;
    let lockfile_sha256 = sha256_file(&lockfile_path)?;
    let source_reproducibility = inspect_source(&manifest_path, &lockfile_path, allow_dirty)?;

    Ok(Preflight {
        record: PreflightRecord {
            host_triple,
            kernel: kernel_version()?,
            cpu_model: cpu_model()?,
            logical_core_count: std::thread::available_parallelism()
                .map_err(|error| {
                    TemperError::new(format!("Could not determine logical core count: {error}"))
                })?
                .get(),
            cargo_version,
            rustc_version,
            manifest_path,
            lockfile_path,
            lockfile_sha256,
        },
        source_reproducibility,
    })
}

pub(crate) fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn rustc_program() -> OsString {
    std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"))
}

fn version_output(program: OsString, arguments: &[&str], name: &str) -> Result<String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|_| {
            TemperError::new(format!(
                "{name} executable is unavailable; install the Rust toolchain and ensure it can be executed."
            ))
        })?;
    if !output.status.success() {
        return Err(TemperError::new(format!(
            "{name} version probe failed; repair the active Rust toolchain before running Temper."
        )));
    }
    if output.stdout.len() > VERSION_OUTPUT_LIMIT {
        return Err(TemperError::new(format!(
            "{name} version output exceeded the 64 KiB diagnostic limit."
        )));
    }
    String::from_utf8(output.stdout)
        .map(|output| output.trim().to_owned())
        .map_err(|_| TemperError::new(format!("{name} version output was not valid UTF-8.")))
}

fn parse_host_triple(rustc_version: &str) -> Result<String> {
    rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| TemperError::new("rustc -Vv did not report a host triple."))
}

fn validate_supported_host(host_triple: &str) -> Result<()> {
    if std::env::consts::OS == "linux" && host_triple == SUPPORTED_HOST {
        Ok(())
    } else {
        Err(TemperError::new(format!(
            "Unsupported host triple {host_triple}; Temper v0.0.1 supports only {SUPPORTED_HOST}."
        )))
    }
}

fn validate_requested_target(requested_target: Option<&str>) -> Result<()> {
    match requested_target {
        Some(target) if target != SUPPORTED_HOST => Err(TemperError::new(format!(
            "Cross-target selection {target} is unsupported; Temper v0.0.1 supports only {SUPPORTED_HOST} host binaries."
        ))),
        _ => Ok(()),
    }
}

fn find_lockfile(manifest_path: &Path) -> Option<PathBuf> {
    let mut directory = manifest_path.parent();
    while let Some(current) = directory {
        let lockfile = current.join("Cargo.lock");
        if lockfile.is_file() {
            return fs::canonicalize(lockfile).ok();
        }
        directory = current.parent();
    }
    None
}

fn inspect_source(
    manifest_path: &Path,
    lockfile_path: &Path,
    allow_dirty: bool,
) -> Result<SourceReproducibility> {
    let project_directory = manifest_path
        .parent()
        .ok_or_else(|| TemperError::new("Cargo manifest has no parent directory."))?;
    let root_output = Command::new("git")
        .args(["-C"])
        .arg(project_directory)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| {
            TemperError::new(
                "Git is unavailable; Temper cannot prove source reproducibility before building.",
            )
        })?;
    if !root_output.status.success() {
        return Err(TemperError::new(
            "The Cargo project is not in a Git worktree; Temper cannot prove source reproducibility.",
        ));
    }
    let git_root = String::from_utf8(root_output.stdout)
        .map_err(|_| TemperError::new("Git worktree path was not valid UTF-8."))?;
    let git_root = PathBuf::from(git_root.trim());
    let exclusion = temper_exclusion(&git_root, lockfile_path)?;
    let status = Command::new("git")
        .arg("-C")
        .arg(&git_root)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            ".",
        ])
        .arg(exclusion)
        .output()
        .map_err(|_| TemperError::new("Git status could not be executed."))?;
    if !status.status.success() {
        return Err(TemperError::new(
            "Git status failed; Temper cannot prove source reproducibility.",
        ));
    }
    let dirty = !status.stdout.is_empty();

    match (dirty, allow_dirty) {
        (true, false) => Err(TemperError::new(
            "Git worktree is dirty. Commit or stash changes, or pass --allow-dirty.",
        )),
        (true, true) => Ok(SourceReproducibility::Dirty),
        (false, _) => Ok(SourceReproducibility::Clean),
    }
}

fn temper_exclusion(git_root: &Path, lockfile_path: &Path) -> Result<String> {
    let workspace_root = lockfile_path
        .parent()
        .ok_or_else(|| TemperError::new("Cargo.lock has no parent directory."))?;
    let relative_temper = workspace_root
        .join(".temper")
        .strip_prefix(git_root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            TemperError::new(
                "Cargo workspace root is outside its Git worktree; source state is ambiguous.",
            )
        })?;
    Ok(format!(
        ":(exclude,literal){}",
        relative_temper.to_string_lossy()
    ))
}

fn kernel_version() -> Result<String> {
    let name = fs::read_to_string("/proc/sys/kernel/ostype")
        .map_err(|error| TemperError::new(format!("Could not read kernel name: {error}")))?;
    let release = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map_err(|error| TemperError::new(format!("Could not read kernel release: {error}")))?;
    Ok(format!("{} {}", name.trim(), release.trim()))
}

fn cpu_model() -> Result<String> {
    let cpu_info = fs::read_to_string("/proc/cpuinfo")
        .map_err(|error| TemperError::new(format!("Could not read CPU model: {error}")))?;
    cpu_info
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim() == "model name")
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| TemperError::new("Linux did not report a CPU model."))
}

#[cfg(test)]
mod tests {
    use super::{
        SUPPORTED_HOST, parse_host_triple, temper_exclusion, validate_requested_target,
        validate_supported_host,
    };
    use std::path::Path;

    #[test]
    fn parses_and_validates_the_supported_host() {
        let version = "rustc 1.97.1\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1";
        assert_eq!(
            parse_host_triple(version).expect("supported host parses"),
            SUPPORTED_HOST
        );
        assert!(validate_supported_host(SUPPORTED_HOST).is_ok());
        assert!(validate_supported_host("aarch64-unknown-linux-gnu").is_err());
        assert!(validate_requested_target(None).is_ok());
        assert!(validate_requested_target(Some(SUPPORTED_HOST)).is_ok());
        assert!(validate_requested_target(Some("aarch64-unknown-linux-gnu")).is_err());
    }

    #[test]
    fn excludes_the_exact_nested_temper_directory() {
        assert_eq!(
            temper_exclusion(Path::new("/repo"), Path::new("/repo/workspace/Cargo.lock"))
                .expect("nested workspace exclusion"),
            ":(exclude,literal)workspace/.temper"
        );
    }
}
