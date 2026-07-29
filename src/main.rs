#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod anchored;
mod cargo;
mod cli;
mod config_graph;
mod error;
mod hash;
mod interposition;
mod measurement;
mod parity;
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
    // The private compiler shim is dispatched before any public CLI parsing so
    // an interposed rustc invocation never reaches Clap.
    interposition::dispatch();
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
        "Temper CLI {} and report schema {} are experimental; 0.x provides no backward-compatibility promise.",
        env!("CARGO_PKG_VERSION"),
        run::SCHEMA_VERSION
    );
    workload::install_interrupt_handler()?;
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
    if let Some(error) = interrupted_run(&mut run, json, "preflight") {
        return Err(error);
    }
    let baseline_plan = BuildPlan::baseline(run.target_root());
    let baseline_result = cargo::build(run.target(), &baseline_plan);
    if let Some(error) = interrupted_run(&mut run, json, "baseline_build") {
        return Err(error);
    }
    let baseline = match baseline_result {
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
        let build_result = cargo::build(run.target(), &plan);
        if let Some(error) = interrupted_run(&mut run, json, "static_builds") {
            return Err(error);
        }
        match build_result {
            Ok(build) => {
                let executable = build.executable_path.clone();
                run.record_static_build(build)?;
                static_artifacts.push((strategy, executable));
            }
            Err(failure) => run.reject_static_build(failure)?,
        }
    }

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

    // Observed inputs of the PGO build that passed training parity. The
    // confirmation rebuild is compared against them before promotion.
    let mut accepted_pgo_inputs = None;
    let pgo_base = strategy::lowest_median_strategy(&valid_measurements)
        .ok_or_else(|| TemperError::new("No valid pre-PGO strategy remained after screening."))?;
    run.complete_screening(pgo_base)?;
    let pgo_training = strategy::train_pgo(
        run.target(),
        &workload,
        run.directory(),
        run.target_root(),
        pgo_base,
    );
    match pgo_training {
        PgoTrainingOutcome::Interrupted(record, failure) => {
            run.fail_pgo_training(record, &failure)?;
            return Err(emit_run_failure(
                &run,
                json,
                TemperError::new(failure.message),
            ));
        }
        PgoTrainingOutcome::Rejected(record) => {
            if let Some(error) = interrupted_run(&mut run, json, "pgo_training") {
                return Err(error);
            }
            run.record_pgo_training(record)?;
            run.skip_pgo_build()?;
        }
        PgoTrainingOutcome::Trained(success) => {
            if let Some(error) = interrupted_run(&mut run, json, "pgo_training") {
                return Err(error);
            }
            let optimized_plan = success.optimized_plan;
            let reference_inputs = success.reference_inputs;
            let instrumentation_inputs = success.instrumentation_inputs;
            run.record_pgo_training(success.record)?;
            let build_result = cargo::build(run.target(), &optimized_plan);
            if let Some(error) = interrupted_run(&mut run, json, "pgo_build") {
                return Err(error);
            }
            match build_result {
                Ok(build) => {
                    // Observed parity is decided after every PGO stage has
                    // executed and before the optimized candidate can receive a
                    // single screening sample. A mismatch rejects only PGO.
                    let optimization_inputs = parity::stage_inputs(&build);
                    let compiler_parity = parity::decide_training(
                        reference_inputs,
                        instrumentation_inputs,
                        optimization_inputs.clone(),
                    );
                    if compiler_parity.matched {
                        accepted_pgo_inputs = optimization_inputs;
                        run.record_pgo_parity(compiler_parity)?;
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
                    } else {
                        run.reject_pgo_parity(build, compiler_parity)?;
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
    let confirmation_baseline_result = cargo::build(run.target(), &plans.baseline);
    if let Some(error) = interrupted_run(&mut run, json, "confirmation") {
        return Err(error);
    }
    let confirmation_baseline = match confirmation_baseline_result {
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

    let confirmation_candidate_result = cargo::build(run.target(), &plans.candidate);
    if let Some(error) = interrupted_run(&mut run, json, "confirmation") {
        return Err(error);
    }
    let confirmation_candidate = match confirmation_candidate_result {
        Ok(build) => build,
        Err(failure) => {
            run.reject_confirmation_build(failure)?;
            run.emit_report(json)?;
            return Ok(());
        }
    };
    // A PGO confirmation must rebuild the accepted compiler input contract.
    // Parity is revalidated here, before any promotion can follow.
    if confirmation_candidate.invocation.strategy == Strategy::Pgo {
        let confirmation_parity = parity::decide_confirmation(
            accepted_pgo_inputs.clone(),
            parity::stage_inputs(&confirmation_candidate),
        );
        if !confirmation_parity.matched {
            run.reject_confirmation_parity(confirmation_candidate, confirmation_parity)?;
            run.emit_report(json)?;
            return Ok(());
        }
        run.record_confirmation_parity(confirmation_parity)?;
    }
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
    if let Err(error) = run.reserve_promotion_failure_space() {
        let mut message =
            format!("Could not reserve space for an atomic promotion failure record: {error}");
        if let Err(emergency_error) = run.activate_emergency_promotion_failure() {
            message.push_str(" Emergency manifest activation also failed: ");
            message.push_str(&emergency_error.to_string());
        }
        return Err(emit_run_failure(&run, json, TemperError::new(message)));
    }

    let source_strategy = match run.confirmation_candidate() {
        Ok((strategy, build)) if build.sha256 == confirmation_candidate_sha256 => strategy,
        Ok(_) => {
            let message =
                "Accepted confirmation candidate checksum changed in run state.".to_owned();
            return Err(fail_reserved_promotion(&mut run, json, message, false));
        }
        Err(error) => {
            return Err(fail_reserved_promotion(
                &mut run,
                json,
                error.to_string(),
                false,
            ));
        }
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
            return Err(fail_reserved_promotion(
                &mut run,
                json,
                error.message,
                interrupted,
            ));
        }
    };
    if let Err(error) = run.record_promotion(promotion) {
        let message = error.to_string();
        return Err(rollback_promotion(&mut run, json, message, false));
    }
    if let Err(error) = run.publish_latest(&workload::interrupted) {
        if error.committed {
            let mut message = error.message;
            release_promotion_reserve(&run, &mut message);
            release_emergency_promotion_failure(&mut run, &mut message);
            return Err(emit_run_failure(&run, json, TemperError::new(message)));
        }
        return Err(rollback_promotion(
            &mut run,
            json,
            error.message,
            error.interrupted,
        ));
    }
    if let Err(error) = run.release_promotion_failure_space() {
        let mut message = format!(
            "Confirmed promotion succeeded, but its failure-space reserve could not be released: {error}"
        );
        release_emergency_promotion_failure(&mut run, &mut message);
        return Err(emit_run_failure(&run, json, TemperError::new(message)));
    }
    if let Err(error) = run.release_emergency_promotion_failure() {
        return Err(emit_run_failure(
            &run,
            json,
            TemperError::new(format!(
                "Confirmed promotion succeeded, but its emergency failure manifest could not be released: {error}"
            )),
        ));
    }
    run.emit_report(json)
}

fn rollback_promotion(
    run: &mut run::Run,
    json: bool,
    mut message: String,
    interrupted: bool,
) -> TemperError {
    if let Err(error) = promotion::discard_promoted_artifact(run.anchored_directory()) {
        message.push_str(" Promoted artifact cleanup also failed: ");
        message.push_str(&error.message);
    }
    release_promotion_reserve(run, &mut message);
    if let Err(error) = run.fail_promotion(message.clone(), interrupted) {
        message.push_str(" Promotion failure persistence also failed: ");
        message.push_str(&error.to_string());
    }
    emit_run_failure(run, json, TemperError::new(message))
}

fn fail_reserved_promotion(
    run: &mut run::Run,
    json: bool,
    mut message: String,
    interrupted: bool,
) -> TemperError {
    release_promotion_reserve(run, &mut message);
    if let Err(error) = run.fail_promotion(message.clone(), interrupted) {
        message.push_str(" Promotion failure persistence also failed: ");
        message.push_str(&error.to_string());
    }
    emit_run_failure(run, json, TemperError::new(message))
}

fn release_promotion_reserve(run: &run::Run, message: &mut String) {
    if let Err(error) = run.release_promotion_failure_space() {
        message.push_str(" Promotion failure-space release also failed: ");
        message.push_str(&error.to_string());
    }
}

fn release_emergency_promotion_failure(run: &mut run::Run, message: &mut String) {
    if let Err(error) = run.release_emergency_promotion_failure() {
        message.push_str(" Emergency promotion manifest release also failed: ");
        message.push_str(&error.to_string());
    }
}

fn interrupted_run(run: &mut run::Run, json: bool, phase: &'static str) -> Option<TemperError> {
    if !workload::interrupted() {
        return None;
    }
    let mut message =
        format!("Run interrupted by SIGINT during {phase}; no artifact was promoted.");
    if let Err(error) = run.fail_interrupted(phase, message.clone()) {
        message.push_str(" Interrupted state persistence also failed: ");
        message.push_str(&error.to_string());
    }
    Some(emit_run_failure(run, json, TemperError::new(message)))
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
        "schema_version": run::SCHEMA_VERSION,
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
