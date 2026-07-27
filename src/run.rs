use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cargo::{BaselineRecord, BuildFailure, TargetSelection};
use crate::cli::OptimizeArgs;
use crate::error::{Result, TemperError};
use crate::measurement::ScreeningResult;
use crate::preflight::{Preflight, SourceReproducibility};
use crate::workload::{WorkloadFailure, WorkloadFailureKind};

const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    BuildingBaseline,
    BaselineReady,
    MeasuringBaseline,
    BaselineMeasured,
    Interrupted,
    Failed,
}

#[derive(Debug, Serialize)]
struct RequestRecord {
    minimum_improvement_percent: f64,
    timeout_seconds: u64,
    workload_argv: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FailureRecord {
    phase: &'static str,
    outcome: Option<WorkloadFailureKind>,
    message: String,
    bounded_diagnostics: String,
    diagnostics_truncated: bool,
}

#[derive(Debug, Serialize)]
struct RunManifest {
    schema_version: u8,
    experimental: bool,
    compatibility: &'static str,
    cli_version: &'static str,
    run_id: String,
    status: RunStatus,
    source_reproducibility: SourceReproducibility,
    request: RequestRecord,
    preflight: crate::preflight::PreflightRecord,
    target: TargetSelection,
    baseline: Option<BaselineRecord>,
    baseline_measurement: Option<ScreeningResult>,
    failure: Option<FailureRecord>,
}

pub(crate) struct Run {
    directory: PathBuf,
    target_directory: PathBuf,
    manifest: RunManifest,
}

impl Run {
    pub(crate) fn create(
        arguments: &OptimizeArgs,
        preflight: Preflight,
        selection: TargetSelection,
    ) -> Result<Self> {
        let run_id = run_id()?;
        let directory = selection
            .workspace_root
            .join(".temper")
            .join("runs")
            .join(&run_id);
        let target_directory = directory.join("target").join("baseline");
        fs::create_dir_all(&target_directory).map_err(|error| {
            TemperError::new(format!(
                "Could not create run directory {}: {error}",
                directory.display()
            ))
        })?;
        let workload_argv = arguments
            .workload
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        let run = Self {
            directory,
            target_directory,
            manifest: RunManifest {
                schema_version: SCHEMA_VERSION,
                experimental: true,
                compatibility: "No backward-compatibility promise during 0.x.",
                cli_version: env!("CARGO_PKG_VERSION"),
                run_id,
                status: RunStatus::BuildingBaseline,
                source_reproducibility: preflight.source_reproducibility,
                request: RequestRecord {
                    minimum_improvement_percent: arguments.minimum_improvement,
                    timeout_seconds: arguments.timeout,
                    workload_argv,
                },
                preflight: preflight.record,
                target: selection,
                baseline: None,
                baseline_measurement: None,
                failure: None,
            },
        };
        run.persist()?;
        Ok(run)
    }

    pub(crate) fn target_directory(&self) -> &Path {
        &self.target_directory
    }

    pub(crate) fn target(&self) -> &TargetSelection {
        &self.manifest.target
    }

    pub(crate) fn complete_baseline(&mut self, baseline: BaselineRecord) -> Result<()> {
        self.manifest.baseline = Some(baseline);
        self.manifest.status = RunStatus::BaselineReady;
        self.persist()
    }

    pub(crate) fn fail_baseline(&mut self, failure: &BuildFailure) -> Result<()> {
        self.manifest.status = RunStatus::Failed;
        self.manifest.failure = Some(FailureRecord {
            phase: "baseline_build",
            outcome: None,
            message: failure.message.clone(),
            bounded_diagnostics: failure.bounded_diagnostics.clone(),
            diagnostics_truncated: failure.diagnostics_truncated,
        });
        self.persist()
    }

    pub(crate) fn begin_baseline_measurement(&mut self) -> Result<()> {
        self.manifest.status = RunStatus::MeasuringBaseline;
        self.persist()
    }

    pub(crate) fn complete_baseline_measurement(
        &mut self,
        measurement: ScreeningResult,
    ) -> Result<()> {
        self.manifest.baseline_measurement = Some(measurement);
        self.manifest.status = RunStatus::BaselineMeasured;
        self.persist()
    }

    pub(crate) fn fail_workload(&mut self, failure: &WorkloadFailure) -> Result<()> {
        self.manifest.status = if failure.kind == WorkloadFailureKind::Interrupted {
            RunStatus::Interrupted
        } else {
            RunStatus::Failed
        };
        self.manifest.failure = Some(FailureRecord {
            phase: "baseline_measurement",
            outcome: Some(failure.kind),
            message: failure.message.clone(),
            bounded_diagnostics: failure.bounded_diagnostics.clone(),
            diagnostics_truncated: failure.diagnostics_truncated,
        });
        self.persist()
    }

    pub(crate) fn baseline_path(&self) -> Option<&Path> {
        self.manifest
            .baseline
            .as_ref()
            .map(|baseline| baseline.executable_path.as_path())
    }

    fn persist(&self) -> Result<()> {
        write_json_atomic(&self.directory.join("manifest.json"), &self.manifest)
    }
}

fn run_id() -> Result<String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| TemperError::new(format!("System clock is before Unix epoch: {error}")))?;
    Ok(format!(
        "{}-{}-{}",
        duration.as_secs(),
        duration.subsec_nanos(),
        std::process::id()
    ))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary_path = path.with_extension(format!("tmp-{}", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary_path)
        .map_err(|error| {
            TemperError::new(format!(
                "Could not create temporary manifest {}: {error}",
                temporary_path.display()
            ))
        })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|error| TemperError::new(format!("Could not serialize run manifest: {error}")))?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|error| TemperError::new(format!("Could not write run manifest: {error}")))?;
    let file: File = writer
        .into_inner()
        .map_err(|error| TemperError::new(format!("Could not finalize run manifest: {error}")))?;
    file.sync_all()
        .map_err(|error| TemperError::new(format!("Could not sync run manifest: {error}")))?;
    fs::rename(&temporary_path, path)
        .map_err(|error| TemperError::new(format!("Could not publish run manifest: {error}")))?;
    let parent = path
        .parent()
        .ok_or_else(|| TemperError::new("Run manifest path has no parent directory."))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| TemperError::new(format!("Could not sync run directory: {error}")))
}
