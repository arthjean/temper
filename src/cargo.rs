use std::collections::HashSet;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use cargo_metadata::{Message, Metadata, PackageId};
use serde::Serialize;

use crate::cli::OptimizeArgs;
use crate::error::{Result, TemperError};
use crate::hash::sha256_file;
use crate::preflight::{Preflight, SUPPORTED_HOST, cargo_program};

const DIAGNOSTIC_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TargetSelection {
    pub(crate) package_id: String,
    pub(crate) package_name: String,
    pub(crate) binary_name: String,
    pub(crate) workspace_root: PathBuf,
}

#[derive(Debug, Serialize)]
pub(crate) struct BaselineRecord {
    pub(crate) executable_path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) build_duration_ms: u128,
    pub(crate) bounded_diagnostics: String,
    pub(crate) diagnostics_truncated: bool,
}

pub(crate) struct BuildFailure {
    pub(crate) message: String,
    pub(crate) bounded_diagnostics: String,
    pub(crate) diagnostics_truncated: bool,
}

pub(crate) fn resolve_target(
    preflight: &Preflight,
    arguments: &OptimizeArgs,
) -> Result<TargetSelection> {
    let output = Command::new(cargo_program())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--manifest-path",
        ])
        .arg(&preflight.record.manifest_path)
        .output()
        .map_err(|_| {
            TemperError::new(
                "Cargo executable is unavailable; metadata resolution could not start.",
            )
        })?;
    if !output.status.success() {
        return Err(TemperError::new(format!(
            "Cargo metadata failed before any build: {}",
            bounded_text(&output.stderr)
        )));
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout).map_err(|error| {
        TemperError::new(format!(
            "Cargo metadata format 1 could not be parsed: {error}"
        ))
    })?;

    validate_workspace_lockfile(preflight, &metadata)?;
    select_binary(
        &metadata,
        arguments.package.as_deref(),
        arguments.binary.as_deref(),
    )
}

fn validate_workspace_lockfile(preflight: &Preflight, metadata: &Metadata) -> Result<()> {
    let workspace_lockfile = metadata.workspace_root.join("Cargo.lock");
    let workspace_lockfile = std::fs::canonicalize(workspace_lockfile).map_err(|_| {
        TemperError::new("Temper v0.0.1 requires an existing Cargo.lock at the workspace root.")
    })?;
    if workspace_lockfile == preflight.record.lockfile_path {
        Ok(())
    } else {
        Err(TemperError::new(
            "The discovered Cargo.lock does not belong to Cargo's resolved workspace root.",
        ))
    }
}

fn select_binary(
    metadata: &Metadata,
    requested_package: Option<&str>,
    requested_binary: Option<&str>,
) -> Result<TargetSelection> {
    let workspace_members: HashSet<&PackageId> = metadata.workspace_members.iter().collect();
    let mut choices = Vec::new();

    for package in &metadata.packages {
        if !workspace_members.contains(&package.id) {
            continue;
        }
        if requested_package.is_some_and(|name| package.name.as_ref() != name) {
            continue;
        }
        for target in &package.targets {
            if !target.is_bin() {
                continue;
            }
            if requested_binary.is_some_and(|name| target.name != name) {
                continue;
            }
            choices.push(TargetSelection {
                package_id: package.id.repr.clone(),
                package_name: package.name.to_string(),
                binary_name: target.name.clone(),
                workspace_root: metadata.workspace_root.clone().into_std_path_buf(),
            });
        }
    }

    match choices.as_slice() {
        [selection] => Ok(selection.clone()),
        [] => Err(selection_error(
            "No supported Cargo binary matched the selection.",
            metadata,
        )),
        _ => Err(selection_error(
            "Select one target with --package and --bin.",
            metadata,
        )),
    }
}

fn selection_error(message: &str, metadata: &Metadata) -> TemperError {
    let mut valid_choices: Vec<String> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .flat_map(|package| {
            package
                .targets
                .iter()
                .filter(|target| target.is_bin())
                .map(|target| format!("{}/{}", package.name, target.name))
        })
        .collect();
    valid_choices.sort();
    let choices = if valid_choices.is_empty() {
        "none (libraries, examples, tests, and benchmarks are unsupported)".to_owned()
    } else {
        valid_choices.join(", ")
    };
    TemperError::new(format!(
        "{message} Temper v0.0.1 supports only host Cargo binary targets. Valid package/binary choices: {choices}"
    ))
}

pub(crate) fn build_baseline(
    selection: &TargetSelection,
    target_directory: &Path,
) -> std::result::Result<BaselineRecord, BuildFailure> {
    let started = Instant::now();
    let mut child = Command::new(cargo_program())
        .args(["build", "--release", "--locked", "--target", SUPPORTED_HOST])
        .args(["--message-format=json", "--package"])
        .arg(&selection.package_name)
        .args(["--bin"])
        .arg(&selection.binary_name)
        .arg("--target-dir")
        .arg(target_directory)
        .current_dir(&selection.workspace_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| BuildFailure::new("Cargo baseline build could not start."))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BuildFailure::new("Cargo baseline stdout was unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BuildFailure::new("Cargo baseline stderr was unavailable."))?;
    let stderr_reader = thread::spawn(move || drain_bounded(stderr));

    let mut diagnostics = BoundedDiagnostics::default();
    let mut executables = Vec::new();
    let mut stream_error = None;
    for message in Message::parse_stream(BufReader::new(stdout)) {
        match message {
            Ok(Message::CompilerArtifact(artifact))
                if artifact.package_id.repr == selection.package_id
                    && artifact.target.name == selection.binary_name
                    && artifact.target.is_bin() =>
            {
                if let Some(executable) = artifact.executable {
                    executables.push(executable.into_std_path_buf());
                }
            }
            Ok(Message::CompilerMessage(message)) => {
                diagnostics.push(&message.message.to_string());
            }
            Ok(Message::BuildScriptExecuted(message)) => {
                diagnostics.push(&format!("build-script-executed: {}", message.package_id));
            }
            Ok(Message::BuildFinished(message)) => {
                diagnostics.push(&format!("build-finished: success={}", message.success));
            }
            Ok(Message::TextLine(line)) => diagnostics.push(&line),
            Ok(_) => {}
            Err(error) => {
                stream_error = Some(error.to_string());
                break;
            }
        }
    }

    let status = child
        .wait()
        .map_err(|error| BuildFailure::new(format!("Cargo baseline wait failed: {error}")))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| BuildFailure::new("Cargo stderr reader terminated unexpectedly."))?;
    diagnostics.push(&stderr.text);
    diagnostics.truncated |= stderr.truncated;

    if let Some(error) = stream_error {
        return Err(BuildFailure::with_diagnostics(
            format!("Cargo JSON message stream failed: {error}"),
            diagnostics,
        ));
    }
    if !status.success() {
        return Err(BuildFailure::with_diagnostics(
            "Cargo build failed for baseline; see bounded diagnostics.",
            diagnostics,
        ));
    }
    let executable = match executables.as_slice() {
        [executable] => executable,
        _ => {
            return Err(BuildFailure::with_diagnostics(
                format!(
                    "Cargo did not emit exactly one executable for {}/{}.",
                    selection.package_name, selection.binary_name
                ),
                diagnostics,
            ));
        }
    };
    let executable = std::fs::canonicalize(executable).map_err(|error| {
        BuildFailure::with_diagnostics(
            format!("Cargo executable could not be canonicalized: {error}"),
            diagnostics.clone(),
        )
    })?;
    let canonical_target = std::fs::canonicalize(target_directory).map_err(|error| {
        BuildFailure::with_diagnostics(
            format!("Isolated target directory could not be canonicalized: {error}"),
            diagnostics.clone(),
        )
    })?;
    if !executable.starts_with(&canonical_target) {
        return Err(BuildFailure::with_diagnostics(
            "Cargo emitted an executable outside the isolated target directory.",
            diagnostics,
        ));
    }
    let file_metadata = std::fs::metadata(&executable).map_err(|error| {
        BuildFailure::with_diagnostics(
            format!("Cargo executable metadata could not be read: {error}"),
            diagnostics.clone(),
        )
    })?;
    let sha256 = sha256_file(&executable)
        .map_err(|error| BuildFailure::with_diagnostics(error.to_string(), diagnostics.clone()))?;

    Ok(BaselineRecord {
        executable_path: executable,
        sha256,
        size_bytes: file_metadata.len(),
        build_duration_ms: started.elapsed().as_millis(),
        bounded_diagnostics: diagnostics.text,
        diagnostics_truncated: diagnostics.truncated,
    })
}

impl BuildFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            bounded_diagnostics: String::new(),
            diagnostics_truncated: false,
        }
    }

    fn with_diagnostics(message: impl Into<String>, diagnostics: BoundedDiagnostics) -> Self {
        Self {
            message: message.into(),
            bounded_diagnostics: diagnostics.text,
            diagnostics_truncated: diagnostics.truncated,
        }
    }
}

#[derive(Clone, Default)]
struct BoundedDiagnostics {
    text: String,
    truncated: bool,
}

impl BoundedDiagnostics {
    fn push(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        let separator_length = usize::from(!self.text.is_empty());
        let available = DIAGNOSTIC_LIMIT.saturating_sub(self.text.len() + separator_length);
        if available == 0 {
            self.truncated = true;
            return;
        }
        if separator_length == 1 {
            self.text.push('\n');
        }
        let boundary = floor_char_boundary(value, available);
        self.text.push_str(&value[..boundary]);
        self.truncated |= boundary < value.len();
    }
}

fn drain_bounded(mut reader: impl Read) -> BoundedDiagnostics {
    let mut diagnostics = BoundedDiagnostics::default();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(bytes_read) => {
                diagnostics.push(&String::from_utf8_lossy(&buffer[..bytes_read]));
            }
            Err(error) => {
                diagnostics.push(&format!("Could not read Cargo stderr: {error}"));
                break;
            }
        }
    }
    diagnostics
}

fn bounded_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let boundary = floor_char_boundary(&text, DIAGNOSTIC_LIMIT);
    text[..boundary].to_owned()
}

fn floor_char_boundary(value: &str, maximum: usize) -> usize {
    let mut boundary = maximum.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::{BoundedDiagnostics, DIAGNOSTIC_LIMIT, floor_char_boundary};

    #[test]
    fn diagnostics_are_bounded_on_utf8_boundaries() {
        let mut diagnostics = BoundedDiagnostics::default();
        diagnostics.push(&"é".repeat(DIAGNOSTIC_LIMIT));
        assert!(diagnostics.text.len() <= DIAGNOSTIC_LIMIT);
        assert!(diagnostics.truncated);
        assert!(diagnostics.text.is_char_boundary(diagnostics.text.len()));
        assert_eq!(floor_char_boundary("é", 1), 0);
    }
}
