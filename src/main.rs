#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod anchored;
mod cargo;
mod cli;
mod error;
mod hash;
mod measurement;
mod preflight;
mod promotion;
mod run;
mod strategy;
mod workload;

use crate::cli::ParseOutcome;
use crate::error::{Result, TemperError};
use crate::measurement::MeasurementOutcome;
use crate::promotion::PromotionErrorKind;
use crate::strategy::{BuildPlan, PgoTrainingOutcome, Strategy};
use crate::workload::{ConfirmationStep, WorkloadFailureKind};

fn main() {
    let exit_code = match cli::parse() {
        ParseOutcome::Optimize(arguments) => match optimize(arguments) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        },
        ParseOutcome::ClapError(error) => {
            let exit_code = error.exit_code();
            if let Err(print_error) = error.print() {
                eprintln!("error: could not print command help: {print_error}");
                1
            } else {
                exit_code
            }
        }
        ParseOutcome::MissingWorkload => {
            eprintln!("error: Provide a workload executable after `--`.");
            eprintln!("Usage: cargo temper optimize [OPTIONS] -- <WORKLOAD>...");
            2
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn optimize(arguments: cli::OptimizeArgs) -> Result<()> {
    eprintln!(
        "Temper CLI {} and report schema 2 are experimental; 0.x provides no backward-compatibility promise.",
        env!("CARGO_PKG_VERSION")
    );
    let preflight = preflight::run(
        &arguments.manifest_path,
        arguments.allow_dirty,
        arguments.target.as_deref(),
    )
    .map_err(|error| emit_pre_run_failure(arguments.json, error))?;
    let selection = cargo::resolve_target(&preflight, &arguments)
        .map_err(|error| emit_pre_run_failure(arguments.json, error))?;
    let workload = workload::WorkloadSpec::new(
        &arguments.workload,
        &selection.workspace_root,
        arguments.timeout,
    )
    .map_err(|error| emit_pre_run_failure(arguments.json, TemperError::new(error.message)))?;
    let json = arguments.json;
    let mut run = run::Run::create(&arguments, preflight, selection)
        .map_err(|error| emit_pre_run_failure(json, error))?;
    let baseline_plan = BuildPlan::baseline(run.target_root());
    let baseline = match cargo::build(run.target(), &baseline_plan) {
        Ok(baseline) => baseline,
        Err(failure) => {
            let message = failure.message.clone();
            run.fail_baseline(failure)?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
    };
    let baseline_path = baseline.executable_path.clone();
    run.complete_baseline(baseline)?;

    let mut static_artifacts = Vec::new();
    for strategy in Strategy::STATIC_CANDIDATES {
        let plan = BuildPlan::candidate(strategy, run.target_root());
        match cargo::build(run.target(), &plan) {
            Ok(build) => {
                let executable = build.executable_path.clone();
                run.record_static_build(build)?;
                static_artifacts.push((strategy, executable));
            }
            Err(failure) => run.reject_static_build(failure)?,
        }
    }

    workload::install_interrupt_handler()?;
    run.begin_screening()?;
    let baseline_measurement = match workload.screen(&baseline_path) {
        Ok(measurement) => measurement,
        Err(failure) => {
            run.fail_workload("screening", &failure)?;
            return Err(emit_run_failure(
                &run,
                json,
                TemperError::new(failure.message),
            ));
        }
    };
    let baseline_median = baseline_measurement.median_duration_ns;
    let baseline_dispersion = baseline_measurement.relative_mad;
    let baseline_stable = baseline_measurement.outcome == MeasurementOutcome::Stable;
    run.record_baseline_screening(baseline_measurement)?;
    if !baseline_stable {
        let failure = workload::WorkloadFailure::unstable_baseline(baseline_dispersion);
        run.fail_workload("screening", &failure)?;
        return Err(emit_run_failure(
            &run,
            json,
            TemperError::new(failure.message),
        ));
    }

    let mut valid_measurements = vec![(Strategy::Baseline, baseline_median)];
    for (strategy, executable) in &static_artifacts {
        match workload.screen(executable) {
            Ok(measurement) => {
                let stable = measurement.outcome == MeasurementOutcome::Stable;
                let median = measurement.median_duration_ns;
                run.record_strategy_screening(*strategy, measurement)?;
                if stable {
                    valid_measurements.push((*strategy, median));
                }
            }
            Err(failure) if failure.kind == WorkloadFailureKind::Interrupted => {
                run.fail_workload("screening", &failure)?;
                return Err(emit_run_failure(
                    &run,
                    json,
                    TemperError::new(failure.message),
                ));
            }
            Err(failure) => run.reject_strategy_workload(*strategy, &failure)?,
        }
    }

    let pgo_base = strategy::lowest_median_strategy(&valid_measurements)
        .ok_or_else(|| TemperError::new("No valid pre-PGO strategy remained after screening."))?;
    run.complete_screening(pgo_base)?;
    match strategy::train_pgo(
        run.target(),
        &workload,
        run.directory(),
        run.target_root(),
        pgo_base,
    ) {
        PgoTrainingOutcome::Interrupted(record, failure) => {
            run.fail_pgo_training(record, &failure)?;
            return Err(emit_run_failure(
                &run,
                json,
                TemperError::new(failure.message),
            ));
        }
        PgoTrainingOutcome::Rejected(record) => {
            run.record_pgo_training(record)?;
            run.skip_pgo_build()?;
        }
        PgoTrainingOutcome::Trained(success) => {
            let optimized_plan = success.optimized_plan;
            run.record_pgo_training(success.record)?;
            match cargo::build(run.target(), &optimized_plan) {
                Ok(build) => {
                    let executable = build.executable_path.clone();
                    run.record_pgo_build(build)?;
                    match workload.screen(&executable) {
                        Ok(measurement) => {
                            let stable = measurement.outcome == MeasurementOutcome::Stable;
                            let median = measurement.median_duration_ns;
                            run.record_strategy_screening(Strategy::Pgo, measurement)?;
                            if stable {
                                valid_measurements.push((Strategy::Pgo, median));
                            }
                        }
                        Err(failure) if failure.kind == WorkloadFailureKind::Interrupted => {
                            run.fail_workload("pgo_build", &failure)?;
                            return Err(emit_run_failure(
                                &run,
                                json,
                                TemperError::new(failure.message),
                            ));
                        }
                        Err(failure) => {
                            run.reject_strategy_workload(Strategy::Pgo, &failure)?;
                        }
                    }
                }
                Err(failure) => run.reject_pgo_build(failure)?,
            }
        }
    }
    run.complete_pgo_build()?;

    let selected = strategy::selected_candidate(&valid_measurements);
    run.select_candidate(selected)?;
    eprintln!(
        "Bounded search complete: baseline median {baseline_median} ns; selected candidate {}.",
        selected.map(Strategy::canonical_identity).unwrap_or("none")
    );

    let Some(plans) = run.confirmation_plans()? else {
        run.complete_without_candidate()?;
        run.emit_report(json)?;
        return Ok(());
    };
    let confirmation_baseline = match cargo::build(run.target(), &plans.baseline) {
        Ok(build) => build,
        Err(failure) => {
            let message = failure.message.clone();
            run.fail_confirmation_baseline(failure)?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
    };
    let confirmation_baseline_path = confirmation_baseline.executable_path.clone();
    let confirmation_baseline_sha256 = confirmation_baseline.sha256.clone();
    run.record_confirmation_baseline(confirmation_baseline)?;

    let confirmation_candidate = match cargo::build(run.target(), &plans.candidate) {
        Ok(build) => build,
        Err(failure) => {
            run.reject_confirmation_build(failure)?;
            run.emit_report(json)?;
            return Ok(());
        }
    };
    let confirmation_candidate_path = confirmation_candidate.executable_path.clone();
    let confirmation_candidate_sha256 = confirmation_candidate.sha256.clone();
    run.record_confirmation_candidate(confirmation_candidate)?;

    let confirmation_baseline_artifact = match promotion::AnchoredArtifact::open(
        run.directory(),
        &confirmation_baseline_path,
        &confirmation_baseline_sha256,
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            let message = error.message;
            run.fail_confirmation_integrity(message.clone())?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
    };
    let confirmation_candidate_artifact = match promotion::AnchoredArtifact::open(
        run.directory(),
        &confirmation_candidate_path,
        &confirmation_candidate_sha256,
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            let message = error.message;
            run.fail_confirmation_integrity(message.clone())?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
    };
    let confirmation = match workload.confirm(
        confirmation_baseline_artifact.path(),
        confirmation_candidate_artifact.path(),
        run.minimum_improvement_percent(),
    ) {
        Ok(confirmation) => confirmation,
        Err(failure)
            if failure.failure.kind == WorkloadFailureKind::Interrupted
                || failure.step != ConfirmationStep::Candidate =>
        {
            let message = failure.failure.message.clone();
            run.fail_confirmation_workload(&failure.failure)?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
        Err(failure) => {
            run.reject_confirmation_workload(&failure.failure)?;
            run.emit_report(json)?;
            return Ok(());
        }
    };
    if let Err(error) = confirmation_baseline_artifact
        .verify_unchanged()
        .and_then(|()| confirmation_candidate_artifact.verify_unchanged())
    {
        let message = error.message;
        run.fail_confirmation_integrity(message.clone())?;
        return Err(emit_run_failure(&run, json, TemperError::new(message)));
    }
    if !run.record_confirmation(confirmation)? {
        run.emit_report(json)?;
        return Ok(());
    }

    let source_strategy = {
        let (strategy, build) = run.confirmation_candidate()?;
        if build.sha256 != confirmation_candidate_sha256 {
            return Err(TemperError::new(
                "Accepted confirmation candidate checksum changed in run state.",
            ));
        }
        strategy
    };
    let promotion = match promotion::promote_artifact(
        run.directory(),
        run.anchored_directory(),
        &confirmation_candidate_artifact,
        source_strategy,
        &workload::interrupted,
    ) {
        Ok(promotion) => promotion,
        Err(error) => {
            let interrupted = error.kind == PromotionErrorKind::Interrupted;
            let message = error.message;
            run.fail_promotion(message.clone(), interrupted)?;
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
    };
    run.record_promotion(promotion)?;
    if let Err(error) = run.publish_latest(&workload::interrupted) {
        if error.committed {
            return Err(emit_run_failure(
                &run,
                json,
                TemperError::new(error.message),
            ));
        }
        let message = error.message;
        run.fail_promotion(message.clone(), error.interrupted)?;
        return Err(emit_run_failure(&run, json, TemperError::new(message)));
    }
    run.emit_report(json)
}

fn emit_run_failure(run: &run::Run, json: bool, error: TemperError) -> TemperError {
    match run.emit_report(json) {
        Ok(()) => error,
        Err(report_error) => TemperError::new(format!(
            "{error} Final report emission also failed: {report_error}"
        )),
    }
}

fn emit_pre_run_failure(json: bool, error: TemperError) -> TemperError {
    if !json {
        return error;
    }
    let report = serde_json::json!({
        "schema_version": 2,
        "experimental": true,
        "status": "failed",
        "final_decision": "failed",
        "failure": {
            "phase": "preflight",
            "message": error.to_string(),
        }
    });
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let emitted = serde_json::to_writer(&mut output, &report).and_then(|()| {
        use std::io::Write;
        output.write_all(b"\n").map_err(serde_json::Error::io)
    });
    match emitted {
        Ok(()) => error,
        Err(report_error) => TemperError::new(format!(
            "{error} Preflight JSON emission also failed: {report_error}"
        )),
    }
}
