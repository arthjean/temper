use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::anchored::AnchoredDirectory;
use crate::cargo::{BuildFailure, BuildRecord, TargetSelection};
use crate::cli::OptimizeArgs;
use crate::error::{Result, TemperError};
use crate::measurement::{ConfirmationResult, MeasurementOutcome, ScreeningResult};
use crate::preflight::{Preflight, SourceReproducibility};
use crate::promotion::PromotionRecord;
use crate::strategy::{BuildPlan, PgoTrainingRecord, Strategy};
use crate::workload::{WorkloadFailure, WorkloadFailureKind};

const SCHEMA_VERSION: u8 = 1;
const HUMAN_REPORT_LINE_LIMIT: usize = 25;

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
    Promotion,
    Confirmed,
    NoImprovement,
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
    Confirmation,
    Promotion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinalDecision {
    Pending,
    Confirmed,
    NoImprovement,
    Interrupted,
    Failed,
}

#[derive(Debug, Serialize)]
struct WorkloadRecord {
    executable: String,
    arguments: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RequestRecord {
    minimum_improvement_percent: f64,
    timeout_seconds: u64,
    workload: WorkloadRecord,
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

impl From<&WorkloadFailure> for StrategyWorkloadFailure {
    fn from(failure: &WorkloadFailure) -> Self {
        Self {
            outcome: failure.kind,
            message: failure.message.clone(),
            bounded_diagnostics: failure.bounded_diagnostics.clone(),
            diagnostics_truncated: failure.diagnostics_truncated,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfirmationOutcome {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Serialize)]
struct ConfirmationRecord {
    strategy: Option<Strategy>,
    outcome: ConfirmationOutcome,
    baseline_build: Option<BuildRecord>,
    baseline_build_failure: Option<BuildFailure>,
    candidate_build: Option<BuildRecord>,
    candidate_build_failure: Option<BuildFailure>,
    measurement: Option<ConfirmationResult>,
    workload_failure: Option<StrategyWorkloadFailure>,
    rejection_reason: Option<String>,
}

impl ConfirmationRecord {
    fn pending(strategy: Option<Strategy>) -> Self {
        Self {
            strategy,
            outcome: ConfirmationOutcome::Pending,
            baseline_build: None,
            baseline_build_failure: None,
            candidate_build: None,
            candidate_build_failure: None,
            measurement: None,
            workload_failure: None,
            rejection_reason: None,
        }
    }
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
    final_decision: FinalDecision,
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
    confirmation: Option<ConfirmationRecord>,
    promotion: Option<PromotionRecord>,
    failure: Option<FailureRecord>,
}

pub(crate) struct ConfirmationPlans {
    pub(crate) baseline: BuildPlan,
    pub(crate) candidate: BuildPlan,
}

pub(crate) struct Run {
    directory: PathBuf,
    target_root: PathBuf,
    temper_directory: AnchoredDirectory,
    run_directory: AnchoredDirectory,
    manifest: RunManifest,
}

pub(crate) struct LatestPublishError {
    pub(crate) message: String,
    pub(crate) committed: bool,
    pub(crate) interrupted: bool,
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
        let temper_directory = AnchoredDirectory::open(&temper_root)
            .map_err(|error| TemperError::new(error.message))?;
        let run_directory =
            AnchoredDirectory::open(&directory).map_err(|error| TemperError::new(error.message))?;
        let (workload_executable, workload_arguments) =
            arguments.workload.split_first().ok_or_else(|| {
                TemperError::new("The validated workload unexpectedly had no executable.")
            })?;
        let run = Self {
            directory,
            target_root,
            temper_directory,
            run_directory,
            manifest: RunManifest {
                schema_version: SCHEMA_VERSION,
                experimental: true,
                compatibility: "No backward-compatibility promise during 0.x.",
                cli_version: env!("CARGO_PKG_VERSION"),
                run_id,
                status: RunStatus::BaselineBuild,
                completed_phases: vec![CompletedPhase::Preflight],
                final_decision: FinalDecision::Pending,
                source_reproducibility: preflight.source_reproducibility,
                request: RequestRecord {
                    minimum_improvement_percent: arguments.minimum_improvement,
                    timeout_seconds: arguments.timeout,
                    workload: WorkloadRecord {
                        executable: workload_executable.to_string_lossy().into_owned(),
                        arguments: workload_arguments
                            .iter()
                            .map(|value| value.to_string_lossy().into_owned())
                            .collect(),
                    },
                },
                preflight: preflight.record,
                target: selection,
                baseline: None,
                baseline_measurement: None,
                strategies: Vec::new(),
                pgo_base_strategy: None,
                pgo_training: None,
                selected_candidate: None,
                confirmation: None,
                promotion: None,
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

    pub(crate) fn anchored_directory(&self) -> &AnchoredDirectory {
        &self.run_directory
    }

    pub(crate) fn minimum_improvement_percent(&self) -> f64 {
        self.manifest.request.minimum_improvement_percent
    }

    pub(crate) fn complete_baseline(&mut self, baseline: BuildRecord) -> Result<()> {
        self.manifest.baseline = Some(baseline);
        self.transition(CompletedPhase::BaselineBuild, RunStatus::StaticBuilds)
    }

    pub(crate) fn fail_baseline(&mut self, failure: BuildFailure) -> Result<()> {
        let message = failure.message.clone();
        let bounded_diagnostics = failure.bounded_diagnostics.clone();
        let diagnostics_truncated = failure.diagnostics_truncated;
        self.fail(
            "baseline_build",
            None,
            message,
            bounded_diagnostics,
            diagnostics_truncated,
            Some(failure),
        )
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
        let rejected = measurement.outcome == MeasurementOutcome::UnstableMeasurement;
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
        record.rejection_reason = Some(workload_rejection_reason(failure.kind).to_owned());
        record.workload_failure = Some(failure.into());
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
        self.manifest.confirmation = Some(ConfirmationRecord::pending(candidate));
        self.transition(CompletedPhase::CandidateSelection, RunStatus::Confirmation)
    }

    pub(crate) fn confirmation_plans(&self) -> Result<Option<ConfirmationPlans>> {
        let Some(candidate) = self.manifest.selected_candidate else {
            return Ok(None);
        };
        let candidate_record = self
            .manifest
            .strategies
            .iter()
            .find(|record| record.identity == candidate)
            .and_then(|record| record.build.as_ref())
            .ok_or_else(|| {
                TemperError::new(format!(
                    "Selected candidate {} has no successful frozen build configuration.",
                    candidate.canonical_identity()
                ))
            })?;
        let baseline_overrides = self
            .manifest
            .baseline
            .as_ref()
            .map(|record| record.invocation.cargo_config_overrides.clone())
            .ok_or_else(|| TemperError::new("Baseline build record is unavailable."))?;
        Ok(Some(ConfirmationPlans {
            baseline: BuildPlan::confirmation(
                Strategy::Baseline,
                &self.target_root,
                baseline_overrides,
            ),
            candidate: BuildPlan::confirmation(
                candidate,
                &self.target_root,
                candidate_record.invocation.cargo_config_overrides.clone(),
            ),
        }))
    }

    pub(crate) fn complete_without_candidate(&mut self) -> Result<()> {
        let confirmation = self.confirmation_mut()?;
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.rejection_reason = Some("no_valid_candidate".to_owned());
        self.finish_no_improvement()
    }

    pub(crate) fn record_confirmation_baseline(&mut self, build: BuildRecord) -> Result<()> {
        self.confirmation_mut()?.baseline_build = Some(build);
        self.persist()
    }

    pub(crate) fn fail_confirmation_baseline(&mut self, failure: BuildFailure) -> Result<()> {
        let message = failure.message.clone();
        let bounded_diagnostics = failure.bounded_diagnostics.clone();
        let diagnostics_truncated = failure.diagnostics_truncated;
        let confirmation = self.confirmation_mut()?;
        confirmation.baseline_build_failure = Some(failure);
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.rejection_reason = Some("confirmation_baseline_build_failed".to_owned());
        self.fail(
            "confirmation_baseline_build",
            None,
            message,
            bounded_diagnostics,
            diagnostics_truncated,
            None,
        )
    }

    pub(crate) fn record_confirmation_candidate(&mut self, build: BuildRecord) -> Result<()> {
        self.confirmation_mut()?.candidate_build = Some(build);
        self.persist()
    }

    pub(crate) fn reject_confirmation_build(&mut self, failure: BuildFailure) -> Result<()> {
        let confirmation = self.confirmation_mut()?;
        confirmation.candidate_build_failure = Some(failure);
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.rejection_reason = Some("confirmation_candidate_build_failed".to_owned());
        self.finish_no_improvement()
    }

    pub(crate) fn reject_confirmation_workload(&mut self, failure: &WorkloadFailure) -> Result<()> {
        let confirmation = self.confirmation_mut()?;
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.workload_failure = Some(failure.into());
        confirmation.rejection_reason = Some(format!(
            "confirmation_{}",
            workload_rejection_reason(failure.kind)
        ));
        self.finish_no_improvement()
    }

    pub(crate) fn fail_confirmation_workload(&mut self, failure: &WorkloadFailure) -> Result<()> {
        let confirmation = self.confirmation_mut()?;
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.workload_failure = Some(failure.into());
        confirmation.rejection_reason = Some("confirmation_failed".to_owned());
        self.fail_workload("confirmation", failure)
    }

    pub(crate) fn fail_confirmation_integrity(&mut self, message: String) -> Result<()> {
        let confirmation = self.confirmation_mut()?;
        confirmation.outcome = ConfirmationOutcome::Rejected;
        confirmation.rejection_reason = Some("confirmation_artifact_changed".to_owned());
        self.fail(
            "confirmation_integrity",
            None,
            message,
            String::new(),
            false,
            None,
        )
    }

    pub(crate) fn record_confirmation(&mut self, measurement: ConfirmationResult) -> Result<bool> {
        let accepted = measurement.outcome == MeasurementOutcome::Accepted;
        let confirmation = self.confirmation_mut()?;
        confirmation.outcome = if accepted {
            ConfirmationOutcome::Accepted
        } else {
            ConfirmationOutcome::Rejected
        };
        confirmation.rejection_reason = match measurement.outcome {
            MeasurementOutcome::UnstableMeasurement => Some("unstable_measurement".to_owned()),
            MeasurementOutcome::InsufficientImprovement => {
                Some("insufficient_improvement".to_owned())
            }
            MeasurementOutcome::Accepted => None,
            MeasurementOutcome::Stable => Some("invalid_confirmation_outcome".to_owned()),
        };
        confirmation.measurement = Some(measurement);
        if accepted {
            self.transition(CompletedPhase::Confirmation, RunStatus::Promotion)?;
        } else {
            self.finish_no_improvement()?;
        }
        Ok(accepted)
    }

    pub(crate) fn confirmation_candidate(&self) -> Result<(Strategy, &BuildRecord)> {
        let confirmation = self
            .manifest
            .confirmation
            .as_ref()
            .ok_or_else(|| TemperError::new("Confirmation record is unavailable."))?;
        if confirmation.outcome != ConfirmationOutcome::Accepted {
            return Err(TemperError::new(
                "Only an accepted confirmation can enter promotion.",
            ));
        }
        let strategy = confirmation
            .strategy
            .ok_or_else(|| TemperError::new("Accepted confirmation has no candidate strategy."))?;
        let build = confirmation.candidate_build.as_ref().ok_or_else(|| {
            TemperError::new("Accepted confirmation has no candidate build record.")
        })?;
        Ok((strategy, build))
    }

    pub(crate) fn record_promotion(&mut self, promotion: PromotionRecord) -> Result<()> {
        let (strategy, build) = self.confirmation_candidate()?;
        if promotion.source_strategy != strategy
            || promotion.source_sha256 != build.sha256
            || promotion.promoted_sha256 != build.sha256
        {
            return Err(TemperError::new(
                "Promoted artifact identity did not match the accepted confirmation.",
            ));
        }
        self.manifest.promotion = Some(promotion);
        self.manifest.final_decision = FinalDecision::Confirmed;
        self.manifest.status = RunStatus::Confirmed;
        self.push_completed(CompletedPhase::Promotion);
        self.persist()
    }

    pub(crate) fn publish_latest(
        &self,
        interrupt: &dyn Fn() -> bool,
    ) -> std::result::Result<(), LatestPublishError> {
        if self.manifest.status != RunStatus::Confirmed {
            return Err(LatestPublishError {
                message: "The latest pointer can only reference a confirmed run.".to_owned(),
                committed: false,
                interrupted: false,
            });
        }
        let promotion = self
            .manifest
            .promotion
            .as_ref()
            .ok_or_else(|| LatestPublishError {
                message: "Confirmed run has no promotion record.".to_owned(),
                committed: false,
                interrupted: false,
            })?;
        let latest = LatestRecord {
            schema_version: SCHEMA_VERSION,
            run_id: &self.manifest.run_id,
            decision: FinalDecision::Confirmed,
            run_json: self.directory.join("run.json"),
            artifact_path: &promotion.promoted_path,
            artifact_sha256: &promotion.promoted_sha256,
            source_strategy: promotion.source_strategy,
        };
        self.temper_directory
            .write_json_atomic("latest.json", &latest, Some(interrupt))
            .map_err(|error| LatestPublishError {
                message: error.message,
                committed: error.committed,
                interrupted: error.interrupted,
            })
    }

    pub(crate) fn fail_promotion(&mut self, message: String, interrupted: bool) -> Result<()> {
        let outcome = interrupted.then_some(WorkloadFailureKind::Interrupted);
        self.fail("promotion", outcome, message, String::new(), false, None)
    }

    pub(crate) fn fail_workload(
        &mut self,
        phase: &'static str,
        failure: &WorkloadFailure,
    ) -> Result<()> {
        self.fail(
            phase,
            Some(failure.kind),
            failure.message.clone(),
            failure.bounded_diagnostics.clone(),
            failure.diagnostics_truncated,
            None,
        )
    }

    pub(crate) fn emit_report(&self, json: bool) -> Result<()> {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        if json {
            serde_json::to_writer(&mut output, &self.manifest).map_err(|error| {
                TemperError::new(format!("Could not serialize final JSON report: {error}"))
            })?;
            output
                .write_all(b"\n")
                .map_err(|error| TemperError::new(format!("Could not write JSON report: {error}")))
        } else {
            let lines = self.human_report_lines();
            if lines.len() > HUMAN_REPORT_LINE_LIMIT {
                return Err(TemperError::new(
                    "Human report exceeded the 25-line success limit.",
                ));
            }
            for line in lines {
                writeln!(output, "{line}").map_err(|error| {
                    TemperError::new(format!("Could not write terminal report: {error}"))
                })?;
            }
            Ok(())
        }
    }

    fn human_report_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Temper {} schema 1 (experimental)",
            env!("CARGO_PKG_VERSION")
        )];
        if let Some(baseline) = &self.manifest.baseline {
            let screening = self
                .manifest
                .baseline_measurement
                .as_ref()
                .map(|measurement| {
                    format!(
                        ", screening={} ns, dispersion={:.2}%",
                        measurement.median_duration_ns,
                        measurement.relative_mad * 100.0
                    )
                })
                .unwrap_or_default();
            lines.push(format!(
                "baseline: built={} ms{screening}",
                baseline.build_duration_ms
            ));
        }
        for strategy in &self.manifest.strategies {
            let build = strategy
                .build
                .as_ref()
                .map(|build| format!("built={} ms", build.build_duration_ms))
                .unwrap_or_else(|| "build=rejected".to_owned());
            let screening = strategy
                .screening
                .as_ref()
                .map(|measurement| {
                    format!(
                        ", screening={} ns, dispersion={:.2}%",
                        measurement.median_duration_ns,
                        measurement.relative_mad * 100.0
                    )
                })
                .unwrap_or_default();
            let rejection = strategy
                .rejection_reason
                .as_deref()
                .map(|reason| format!(", rejection={reason}"))
                .unwrap_or_default();
            lines.push(format!(
                "{}: {build}{screening}{rejection}",
                strategy.identity.canonical_identity()
            ));
        }
        if let Some(confirmation) = &self.manifest.confirmation {
            if let Some(measurement) = &confirmation.measurement {
                lines.push(format!(
                    "confirmation: ratio={:.6}, ci95=[{:.6}, {:.6}], dispersion=[{:.2}%, {:.2}%]",
                    measurement.median_ratio,
                    measurement.confidence_interval_95.lower,
                    measurement.confidence_interval_95.upper,
                    measurement.baseline_relative_mad * 100.0,
                    measurement.candidate_relative_mad * 100.0
                ));
            } else if let Some(reason) = &confirmation.rejection_reason {
                lines.push(format!("confirmation: rejection={reason}"));
            }
        }
        lines.push(format!(
            "decision: {}",
            decision_label(self.manifest.final_decision)
        ));
        if let Some(promotion) = &self.manifest.promotion {
            lines.push(format!(
                "artifact: {} ({})",
                promotion.promoted_path.display(),
                promotion.promoted_sha256
            ));
        }
        lines
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

    fn confirmation_mut(&mut self) -> Result<&mut ConfirmationRecord> {
        self.manifest
            .confirmation
            .as_mut()
            .ok_or_else(|| TemperError::new("Confirmation record is unavailable."))
    }

    fn finish_no_improvement(&mut self) -> Result<()> {
        if let Some(selected) = self.manifest.selected_candidate
            && let Some(record) = self
                .manifest
                .strategies
                .iter_mut()
                .find(|record| record.identity == selected)
        {
            record.rejection_reason = Some("confirmation_rejected".to_owned());
        }
        self.push_completed(CompletedPhase::Confirmation);
        self.manifest.status = RunStatus::NoImprovement;
        self.manifest.final_decision = FinalDecision::NoImprovement;
        self.persist()
    }

    fn transition(&mut self, completed: CompletedPhase, next: RunStatus) -> Result<()> {
        self.push_completed(completed);
        self.manifest.status = next;
        self.persist()
    }

    fn push_completed(&mut self, completed: CompletedPhase) {
        if !self.manifest.completed_phases.contains(&completed) {
            self.manifest.completed_phases.push(completed);
        }
    }

    fn fail(
        &mut self,
        phase: &'static str,
        outcome: Option<WorkloadFailureKind>,
        message: String,
        bounded_diagnostics: String,
        diagnostics_truncated: bool,
        build_failure: Option<BuildFailure>,
    ) -> Result<()> {
        let interrupted = outcome == Some(WorkloadFailureKind::Interrupted);
        self.manifest.status = if interrupted {
            RunStatus::Interrupted
        } else {
            RunStatus::Failed
        };
        self.manifest.final_decision = if interrupted {
            FinalDecision::Interrupted
        } else {
            FinalDecision::Failed
        };
        self.manifest.failure = Some(FailureRecord {
            phase,
            outcome,
            message,
            bounded_diagnostics,
            diagnostics_truncated,
            build_failure,
        });
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        self.run_directory
            .write_json_atomic("run.json", &self.manifest, None)
            .map_err(|error| TemperError::new(error.message))
    }
}

#[derive(Serialize)]
struct LatestRecord<'a> {
    schema_version: u8,
    run_id: &'a str,
    decision: FinalDecision,
    run_json: PathBuf,
    artifact_path: &'a Path,
    artifact_sha256: &'a str,
    source_strategy: Strategy,
}

fn workload_rejection_reason(kind: WorkloadFailureKind) -> &'static str {
    match kind {
        WorkloadFailureKind::Timeout => "timeout",
        WorkloadFailureKind::OutputLimit => "output_limit",
        WorkloadFailureKind::Interrupted => "interrupted",
        _ => "workload_failed",
    }
}

fn decision_label(decision: FinalDecision) -> &'static str {
    match decision {
        FinalDecision::Pending => "pending",
        FinalDecision::Confirmed => "confirmed",
        FinalDecision::NoImprovement => "no_improvement",
        FinalDecision::Interrupted => "interrupted",
        FinalDecision::Failed => "failed",
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
