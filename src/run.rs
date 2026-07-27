use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cargo::{BuildFailure, BuildRecord, TargetSelection};
use crate::cli::OptimizeArgs;
use crate::error::{Result, TemperError};
use crate::measurement::ScreeningResult;
use crate::preflight::{Preflight, SourceReproducibility};
use crate::strategy::{PgoTrainingRecord, Strategy};
use crate::workload::{WorkloadFailure, WorkloadFailureKind};

const SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    BaselineBuild,
    StaticBuilds,
    Screening,
    PgoTraining,
    PgoBuild,
    CandidateSelection,
    Confirmation,
    Interrupted,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompletedPhase {
    Preflight,
    BaselineBuild,
    StaticBuilds,
    Screening,
    PgoTraining,
    PgoBuild,
    CandidateSelection,
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
    build_failure: Option<BuildFailure>,
}

#[derive(Debug, Serialize)]
struct StrategyWorkloadFailure {
    outcome: WorkloadFailureKind,
    message: String,
    bounded_diagnostics: String,
    diagnostics_truncated: bool,
}

#[derive(Debug, Serialize)]
struct StrategyRecord {
    identity: Strategy,
    build: Option<BuildRecord>,
    build_failure: Option<BuildFailure>,
    screening: Option<ScreeningResult>,
    workload_failure: Option<StrategyWorkloadFailure>,
    rejection_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunManifest {
    schema_version: u8,
    experimental: bool,
    compatibility: &'static str,
    cli_version: &'static str,
    run_id: String,
    status: RunStatus,
    completed_phases: Vec<CompletedPhase>,
    source_reproducibility: SourceReproducibility,
    request: RequestRecord,
    preflight: crate::preflight::PreflightRecord,
    target: TargetSelection,
    baseline: Option<BuildRecord>,
    baseline_measurement: Option<ScreeningResult>,
    strategies: Vec<StrategyRecord>,
    pgo_base_strategy: Option<Strategy>,
    pgo_training: Option<PgoTrainingRecord>,
    selected_candidate: Option<Strategy>,
    failure: Option<FailureRecord>,
}

pub(crate) struct Run {
    directory: PathBuf,
    target_root: PathBuf,
    manifest: RunManifest,
}

impl Run {
    pub(crate) fn create(
        arguments: &OptimizeArgs,
        preflight: Preflight,
        selection: TargetSelection,
    ) -> Result<Self> {
        let run_id = run_id()?;
        let workspace_root = fs::canonicalize(&selection.workspace_root).map_err(|error| {
            TemperError::new(format!(
                "Could not canonicalize Cargo workspace root {}: {error}",
                selection.workspace_root.display()
            ))
        })?;
        let temper_root = selection.workspace_root.join(".temper");
        ensure_directory(&temper_root)?;
        let runs_root = temper_root.join("runs");
        ensure_directory(&runs_root)?;
        let directory = runs_root.join(&run_id);
        fs::create_dir(&directory).map_err(|error| {
            TemperError::new(format!(
                "Could not create run directory {}: {error}",
                directory.display()
            ))
        })?;
        let directory = fs::canonicalize(&directory).map_err(|error| {
            TemperError::new(format!("Could not canonicalize run directory: {error}"))
        })?;
        if !directory.starts_with(workspace_root.join(".temper").join("runs")) {
            return Err(TemperError::new(
                "Run directory escaped the canonical Cargo workspace .temper/runs directory.",
            ));
        }
        let target_root = directory.join("target");
        fs::create_dir(&target_root).map_err(|error| {
            TemperError::new(format!(
                "Could not create run target directory {}: {error}",
                target_root.display()
            ))
        })?;
        let workload_argv = arguments
            .workload
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        let run = Self {
            directory,
            target_root,
            manifest: RunManifest {
                schema_version: SCHEMA_VERSION,
                experimental: true,
                compatibility: "No backward-compatibility promise during 0.x.",
                cli_version: env!("CARGO_PKG_VERSION"),
                run_id,
                status: RunStatus::BaselineBuild,
                completed_phases: vec![CompletedPhase::Preflight],
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
                strategies: Vec::new(),
                pgo_base_strategy: None,
                pgo_training: None,
                selected_candidate: None,
                failure: None,
            },
        };
        run.persist()?;
        Ok(run)
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn target_root(&self) -> &Path {
        &self.target_root
    }

    pub(crate) fn target(&self) -> &TargetSelection {
        &self.manifest.target
    }

    pub(crate) fn complete_baseline(&mut self, baseline: BuildRecord) -> Result<()> {
        self.manifest.baseline = Some(baseline);
        self.transition(CompletedPhase::BaselineBuild, RunStatus::StaticBuilds)
    }

    pub(crate) fn fail_baseline(&mut self, failure: BuildFailure) -> Result<()> {
        let message = failure.message.clone();
        let bounded_diagnostics = failure.bounded_diagnostics.clone();
        let diagnostics_truncated = failure.diagnostics_truncated;
        self.manifest.status = RunStatus::Failed;
        self.manifest.failure = Some(FailureRecord {
            phase: "baseline_build",
            outcome: None,
            message,
            bounded_diagnostics,
            diagnostics_truncated,
            build_failure: Some(failure),
        });
        self.persist()
    }

    pub(crate) fn record_static_build(&mut self, build: BuildRecord) -> Result<()> {
        self.manifest.strategies.push(StrategyRecord {
            identity: build.invocation.strategy,
            build: Some(build),
            build_failure: None,
            screening: None,
            workload_failure: None,
            rejection_reason: None,
        });
        self.persist()
    }

    pub(crate) fn reject_static_build(&mut self, failure: BuildFailure) -> Result<()> {
        self.manifest.strategies.push(StrategyRecord {
            identity: failure.invocation.strategy,
            build: None,
            rejection_reason: Some("static_build_failed".to_owned()),
            build_failure: Some(failure),
            screening: None,
            workload_failure: None,
        });
        self.persist()
    }

    pub(crate) fn begin_screening(&mut self) -> Result<()> {
        self.transition(CompletedPhase::StaticBuilds, RunStatus::Screening)
    }

    pub(crate) fn record_baseline_screening(&mut self, measurement: ScreeningResult) -> Result<()> {
        self.manifest.baseline_measurement = Some(measurement);
        self.persist()
    }

    pub(crate) fn record_strategy_screening(
        &mut self,
        strategy: Strategy,
        measurement: ScreeningResult,
    ) -> Result<()> {
        let rejected = matches!(
            measurement.outcome,
            crate::measurement::MeasurementOutcome::UnstableMeasurement
        );
        let record = self.strategy_mut(strategy)?;
        record.screening = Some(measurement);
        if rejected {
            record.rejection_reason = Some("unstable_measurement".to_owned());
        }
        self.persist()
    }

    pub(crate) fn reject_strategy_workload(
        &mut self,
        strategy: Strategy,
        failure: &WorkloadFailure,
    ) -> Result<()> {
        let record = self.strategy_mut(strategy)?;
        record.rejection_reason = Some(match failure.kind {
            WorkloadFailureKind::Timeout => "timeout".to_owned(),
            WorkloadFailureKind::OutputLimit => "output_limit".to_owned(),
            _ => "workload_failed".to_owned(),
        });
        record.workload_failure = Some(StrategyWorkloadFailure {
            outcome: failure.kind,
            message: failure.message.clone(),
            bounded_diagnostics: failure.bounded_diagnostics.clone(),
            diagnostics_truncated: failure.diagnostics_truncated,
        });
        self.persist()
    }

    pub(crate) fn complete_screening(&mut self, pgo_base_strategy: Strategy) -> Result<()> {
        self.manifest.pgo_base_strategy = Some(pgo_base_strategy);
        self.transition(CompletedPhase::Screening, RunStatus::PgoTraining)
    }

    pub(crate) fn record_pgo_training(&mut self, record: PgoTrainingRecord) -> Result<()> {
        self.manifest.pgo_training = Some(record);
        self.transition(CompletedPhase::PgoTraining, RunStatus::PgoBuild)
    }

    pub(crate) fn fail_pgo_training(
        &mut self,
        record: PgoTrainingRecord,
        failure: &WorkloadFailure,
    ) -> Result<()> {
        self.manifest.pgo_training = Some(record);
        self.fail_workload("pgo_training", failure)
    }

    pub(crate) fn record_pgo_build(&mut self, build: BuildRecord) -> Result<()> {
        self.manifest.strategies.push(StrategyRecord {
            identity: Strategy::Pgo,
            build: Some(build),
            build_failure: None,
            screening: None,
            workload_failure: None,
            rejection_reason: None,
        });
        self.persist()
    }

    pub(crate) fn reject_pgo_build(&mut self, failure: BuildFailure) -> Result<()> {
        self.manifest.strategies.push(StrategyRecord {
            identity: Strategy::Pgo,
            build: None,
            build_failure: Some(failure),
            screening: None,
            workload_failure: None,
            rejection_reason: Some("pgo_build_failed".to_owned()),
        });
        self.persist()
    }

    pub(crate) fn skip_pgo_build(&mut self) -> Result<()> {
        self.manifest.strategies.push(StrategyRecord {
            identity: Strategy::Pgo,
            build: None,
            build_failure: None,
            screening: None,
            workload_failure: None,
            rejection_reason: Some("pgo_training_rejected".to_owned()),
        });
        self.persist()
    }

    pub(crate) fn complete_pgo_build(&mut self) -> Result<()> {
        self.transition(CompletedPhase::PgoBuild, RunStatus::CandidateSelection)
    }

    pub(crate) fn select_candidate(&mut self, candidate: Option<Strategy>) -> Result<()> {
        self.manifest.selected_candidate = candidate;
        self.transition(CompletedPhase::CandidateSelection, RunStatus::Confirmation)
    }

    pub(crate) fn fail_workload(
        &mut self,
        phase: &'static str,
        failure: &WorkloadFailure,
    ) -> Result<()> {
        self.manifest.status = if failure.kind == WorkloadFailureKind::Interrupted {
            RunStatus::Interrupted
        } else {
            RunStatus::Failed
        };
        self.manifest.failure = Some(FailureRecord {
            phase,
            outcome: Some(failure.kind),
            message: failure.message.clone(),
            bounded_diagnostics: failure.bounded_diagnostics.clone(),
            diagnostics_truncated: failure.diagnostics_truncated,
            build_failure: None,
        });
        self.persist()
    }

    fn strategy_mut(&mut self, strategy: Strategy) -> Result<&mut StrategyRecord> {
        self.manifest
            .strategies
            .iter_mut()
            .find(|record| record.identity == strategy)
            .ok_or_else(|| {
                TemperError::new(format!(
                    "Strategy {} has no persisted build record.",
                    strategy.canonical_identity()
                ))
            })
    }

    fn transition(&mut self, completed: CompletedPhase, next: RunStatus) -> Result<()> {
        if !self.manifest.completed_phases.contains(&completed) {
            self.manifest.completed_phases.push(completed);
        }
        self.manifest.status = next;
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        write_json_atomic(&self.directory.join("manifest.json"), &self.manifest)
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(TemperError::new(format!(
            "Run path {} must not be a symbolic link.",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_dir() => Err(TemperError::new(format!(
            "Run path {} exists but is not a directory.",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                TemperError::new(format!(
                    "Could not create run directory {}: {error}",
                    path.display()
                ))
            })
        }
        Err(error) => Err(TemperError::new(format!(
            "Could not inspect run directory {}: {error}",
            path.display()
        ))),
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
