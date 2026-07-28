use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use cargo_metadata::{Message, Metadata, PackageId, TargetKind};
use serde::Serialize;

use crate::cli::OptimizeArgs;
use crate::error::{Result, TemperError};
use crate::hash::sha256_file;
use crate::preflight::{Preflight, SUPPORTED_HOST, cargo_program};
use crate::strategy::{BuildPlan, BuildStage, Strategy};

const DIAGNOSTIC_LIMIT: usize = 64 * 1024;
const DIAGNOSTIC_FIELD_LIMIT: usize = 8 * 1024;
const ENCODED_RUSTFLAGS_SEPARATOR: char = '\u{1f}';
const MISSING_PROFILE_MESSAGE_PREFIX: &str = "no profile data available for function ";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TargetSelection {
    pub(crate) package_id: String,
    pub(crate) package_name: String,
    pub(crate) binary_name: String,
    pub(crate) workspace_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BuildOutcome {
    Built,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BuildEnvironmentOverride {
    pub(crate) name: String,
    pub(crate) arguments: Vec<String>,
}

impl BuildEnvironmentOverride {
    pub(crate) fn cargo_encoded_rustflags(arguments: Vec<String>) -> Self {
        Self {
            name: "CARGO_ENCODED_RUSTFLAGS".to_owned(),
            arguments,
        }
    }

    fn process_value(&self) -> OsString {
        self.arguments
            .join(&ENCODED_RUSTFLAGS_SEPARATOR.to_string())
            .into()
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BuildInvocation {
    pub(crate) strategy: Strategy,
    pub(crate) stage: BuildStage,
    pub(crate) target_directory: PathBuf,
    pub(crate) cargo_arguments: Vec<String>,
    pub(crate) cargo_config_overrides: Vec<String>,
    pub(crate) environment_overrides: Vec<BuildEnvironmentOverride>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BuildRecord {
    #[serde(flatten)]
    pub(crate) invocation: BuildInvocation,
    pub(crate) outcome: BuildOutcome,
    pub(crate) executable_path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) build_duration_ms: u128,
    pub(crate) bounded_diagnostics: String,
    pub(crate) compiler_diagnostics: Vec<CompilerDiagnosticRecord>,
    pub(crate) diagnostics_truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CompilerDiagnosticRecord {
    package_id: String,
    target_name: String,
    target_kind: Vec<String>,
    target_src_path: String,
    level: String,
    code: Option<String>,
    message: String,
    rendered: String,
}

impl CompilerDiagnosticRecord {
    fn from_message(message: &cargo_metadata::CompilerMessage) -> Self {
        Self {
            package_id: bounded_field(&message.package_id.repr),
            target_name: bounded_field(&message.target.name),
            target_kind: message
                .target
                .kind
                .iter()
                .take(16)
                .map(|kind| format!("{kind:?}").to_ascii_lowercase())
                .collect(),
            target_src_path: bounded_field(message.target.src_path.as_str()),
            level: format!("{:?}", message.message.level).to_ascii_lowercase(),
            code: message
                .message
                .code
                .as_ref()
                .map(|code| bounded_field(&code.code)),
            message: bounded_field(&message.message.message),
            rendered: bounded_field(
                message
                    .message
                    .rendered
                    .as_deref()
                    .unwrap_or(&message.message.message),
            ),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct BuildFailure {
    #[serde(flatten)]
    pub(crate) invocation: Box<BuildInvocation>,
    pub(crate) outcome: BuildOutcome,
    pub(crate) reason: &'static str,
    pub(crate) build_duration_ms: u128,
    pub(crate) message: String,
    pub(crate) bounded_diagnostics: String,
    pub(crate) compiler_diagnostics: Box<[CompilerDiagnosticRecord]>,
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

pub(crate) fn build(
    selection: &TargetSelection,
    plan: &BuildPlan,
) -> std::result::Result<BuildRecord, BuildFailure> {
    let (arguments, invocation) = build_invocation(selection, plan);
    let started = Instant::now();
    let mut command = Command::new(cargo_program());
    command
        .args(&arguments)
        .current_dir(&selection.workspace_root);
    for environment in &plan.environment_overrides {
        command.env(&environment.name, environment.process_value());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            BuildFailure::new(
                &invocation,
                started,
                "cargo_build_start_failed",
                format!(
                    "Cargo {} build could not start.",
                    plan.strategy.canonical_identity()
                ),
            )
        })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        BuildFailure::new(
            &invocation,
            started,
            "cargo_stdout_unavailable",
            "Cargo build stdout was unavailable.",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        BuildFailure::new(
            &invocation,
            started,
            "cargo_stderr_unavailable",
            "Cargo build stderr was unavailable.",
        )
    })?;
    let stderr_reader = thread::spawn(move || drain_bounded(stderr));

    let mut diagnostics = BoundedDiagnostics::default();
    let mut executables = Vec::new();
    let mut stream_error = None;
    for message in Message::parse_stream(BufReader::new(stdout)) {
        match message {
            Ok(Message::CompilerArtifact(artifact))
                if artifact.package_id.repr == selection.package_id
                    && artifact.target.name == selection.binary_name
                    && artifact.target.kind.as_slice() == [TargetKind::Bin] =>
            {
                if let Some(executable) = artifact.executable {
                    executables.push(executable.into_std_path_buf());
                }
            }
            Ok(Message::CompilerMessage(message)) => {
                diagnostics.push_compiler_message(&message);
            }
            Ok(Message::BuildScriptExecuted(message)) => {
                diagnostics.push(&format!("build-script-executed: {}", message.package_id));
            }
            Ok(Message::BuildFinished(message)) => {
                diagnostics.push(&format!("build-finished: success={}", message.success));
            }
            Ok(Message::TextLine(line)) if line.trim_start().starts_with('{') => {
                diagnostics.push(&line);
                if stream_error.is_none() {
                    stream_error = Some((
                        "unrecognized_cargo_json",
                        "Cargo emitted a JSON-looking record that cargo_metadata 0.23.1 could not classify."
                            .to_owned(),
                    ));
                }
            }
            Ok(Message::TextLine(line)) => diagnostics.push(&line),
            Ok(_) => {}
            Err(error) => {
                if stream_error.is_none() {
                    stream_error = Some(("invalid_cargo_message", error.to_string()));
                }
            }
        }
    }

    let status = child.wait().map_err(|error| {
        BuildFailure::new(
            &invocation,
            started,
            "cargo_wait_failed",
            format!("Cargo build wait failed: {error}"),
        )
    })?;
    let stderr = stderr_reader.join().map_err(|_| {
        BuildFailure::new(
            &invocation,
            started,
            "cargo_stderr_reader_failed",
            "Cargo stderr reader terminated unexpectedly.",
        )
    })?;
    diagnostics.push(&stderr.text);
    diagnostics.truncated |= stderr.truncated;

    if let Some((reason, error)) = stream_error {
        return Err(BuildFailure::with_diagnostics(
            &invocation,
            started,
            reason,
            format!("Cargo JSON message stream failed: {error}"),
            diagnostics,
        ));
    }
    if plan.strategy == Strategy::Pgo && diagnostics.missing_profile_data {
        return Err(BuildFailure::with_diagnostics(
            &invocation,
            started,
            "pgo_missing_profile_data",
            "PGO rejected: LLVM reported missing profile data.",
            diagnostics,
        ));
    }
    if !status.success() {
        return Err(BuildFailure::with_diagnostics(
            &invocation,
            started,
            "cargo_build_failed",
            format!(
                "Cargo build failed for {}; see bounded diagnostics.",
                plan.strategy.canonical_identity()
            ),
            diagnostics,
        ));
    }
    let executable = match executables.as_slice() {
        [executable] => executable,
        [] => {
            return Err(BuildFailure::with_diagnostics(
                &invocation,
                started,
                "cargo_executable_missing",
                format!(
                    "Cargo emitted no matching executable for {}/{}.",
                    selection.package_name, selection.binary_name
                ),
                diagnostics,
            ));
        }
        _ => {
            return Err(BuildFailure::with_diagnostics(
                &invocation,
                started,
                "cargo_executable_ambiguous",
                format!(
                    "Cargo emitted multiple matching executables for {}/{}.",
                    selection.package_name, selection.binary_name
                ),
                diagnostics,
            ));
        }
    };
    let executable = std::fs::canonicalize(executable).map_err(|error| {
        BuildFailure::with_diagnostics(
            &invocation,
            started,
            "cargo_executable_unavailable",
            format!("Cargo executable could not be canonicalized: {error}"),
            diagnostics.clone(),
        )
    })?;
    let canonical_target = std::fs::canonicalize(&plan.target_directory).map_err(|error| {
        BuildFailure::with_diagnostics(
            &invocation,
            started,
            "target_directory_unavailable",
            format!("Isolated target directory could not be canonicalized: {error}"),
            diagnostics.clone(),
        )
    })?;
    if !executable.starts_with(&canonical_target) {
        return Err(BuildFailure::with_diagnostics(
            &invocation,
            started,
            "cargo_executable_outside_target",
            "Cargo emitted an executable outside the isolated target directory.",
            diagnostics,
        ));
    }
    let file_metadata = std::fs::metadata(&executable).map_err(|error| {
        BuildFailure::with_diagnostics(
            &invocation,
            started,
            "cargo_executable_metadata_failed",
            format!("Cargo executable metadata could not be read: {error}"),
            diagnostics.clone(),
        )
    })?;
    let sha256 = sha256_file(&executable).map_err(|error| {
        BuildFailure::with_diagnostics(
            &invocation,
            started,
            "cargo_executable_hash_failed",
            error.to_string(),
            diagnostics.clone(),
        )
    })?;

    Ok(BuildRecord {
        invocation,
        outcome: BuildOutcome::Built,
        executable_path: executable,
        sha256,
        size_bytes: file_metadata.len(),
        build_duration_ms: started.elapsed().as_millis(),
        bounded_diagnostics: diagnostics.text,
        compiler_diagnostics: diagnostics.compiler_diagnostics,
        diagnostics_truncated: diagnostics.truncated,
    })
}

pub(crate) fn planned_invocation(selection: &TargetSelection, plan: &BuildPlan) -> BuildInvocation {
    build_invocation(selection, plan).1
}

fn build_invocation(
    selection: &TargetSelection,
    plan: &BuildPlan,
) -> (Vec<OsString>, BuildInvocation) {
    let mut arguments: Vec<OsString> = [
        "build",
        "--release",
        "--locked",
        "--target",
        SUPPORTED_HOST,
        "--message-format=json",
        "--package",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    arguments.push(selection.package_name.clone().into());
    arguments.push("--bin".into());
    arguments.push(selection.binary_name.clone().into());
    arguments.push("--target-dir".into());
    arguments.push(plan.target_directory.as_os_str().to_owned());
    for configuration in &plan.cargo_config_overrides {
        arguments.push("--config".into());
        arguments.push(configuration.into());
    }
    let cargo_arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    (
        arguments,
        BuildInvocation {
            strategy: plan.strategy,
            stage: plan.stage,
            target_directory: plan.target_directory.clone(),
            cargo_arguments,
            cargo_config_overrides: plan.cargo_config_overrides.clone(),
            environment_overrides: plan.environment_overrides.clone(),
        },
    )
}

impl BuildFailure {
    fn new(
        invocation: &BuildInvocation,
        started: Instant,
        reason: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invocation: Box::new(invocation.clone()),
            outcome: BuildOutcome::Failed,
            reason,
            build_duration_ms: started.elapsed().as_millis(),
            message: message.into(),
            bounded_diagnostics: String::new(),
            compiler_diagnostics: Vec::new().into_boxed_slice(),
            diagnostics_truncated: false,
        }
    }

    fn with_diagnostics(
        invocation: &BuildInvocation,
        started: Instant,
        reason: &'static str,
        message: impl Into<String>,
        diagnostics: BoundedDiagnostics,
    ) -> Self {
        Self {
            invocation: Box::new(invocation.clone()),
            outcome: BuildOutcome::Failed,
            reason,
            build_duration_ms: started.elapsed().as_millis(),
            message: message.into(),
            bounded_diagnostics: diagnostics.text,
            compiler_diagnostics: diagnostics.compiler_diagnostics.into_boxed_slice(),
            diagnostics_truncated: diagnostics.truncated,
        }
    }
}

#[derive(Clone, Default)]
struct BoundedDiagnostics {
    text: String,
    compiler_diagnostics: Vec<CompilerDiagnosticRecord>,
    compiler_diagnostics_bytes: usize,
    missing_profile_data: bool,
    truncated: bool,
}

impl BoundedDiagnostics {
    fn push(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        let separator_length = usize::from(!self.text.is_empty());
        let available = DIAGNOSTIC_LIMIT
            .saturating_sub(self.text.len() + self.compiler_diagnostics_bytes + separator_length);
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

    fn push_compiler_message(&mut self, message: &cargo_metadata::CompilerMessage) {
        let record = CompilerDiagnosticRecord::from_message(message);
        let missing_profile_data = matches_missing_profile_data(message);
        let serialized_length = serde_json::to_vec(&record)
            .map(|serialized| serialized.len())
            .unwrap_or(DIAGNOSTIC_LIMIT);
        if missing_profile_data
            && self.text.len() + self.compiler_diagnostics_bytes + serialized_length
                > DIAGNOSTIC_LIMIT
        {
            self.text.clear();
            self.compiler_diagnostics.clear();
            self.compiler_diagnostics_bytes = 0;
            self.truncated = true;
        }
        if self.text.len() + self.compiler_diagnostics_bytes + serialized_length <= DIAGNOSTIC_LIMIT
        {
            self.compiler_diagnostics_bytes += serialized_length;
            self.compiler_diagnostics.push(record);
        } else {
            self.truncated = true;
        }
        self.missing_profile_data |= missing_profile_data;
        self.push(&message.message.to_string());
    }
}

fn matches_missing_profile_data(message: &cargo_metadata::CompilerMessage) -> bool {
    format!("{:?}", message.message.level).eq_ignore_ascii_case("warning")
        && message.message.code.is_none()
        && message
            .message
            .message
            .split_once(": ")
            .is_some_and(|(_, detail)| detail.starts_with(MISSING_PROFILE_MESSAGE_PREFIX))
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

fn bounded_field(value: &str) -> String {
    let boundary = floor_char_boundary(value, DIAGNOSTIC_FIELD_LIMIT);
    value[..boundary].to_owned()
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
