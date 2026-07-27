#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cargo;
mod cli;
mod error;
mod hash;
mod measurement;
mod preflight;
mod run;
mod strategy;
mod workload;

use crate::cli::ParseOutcome;
use crate::error::{Result, TemperError};
use crate::measurement::MeasurementOutcome;
use crate::strategy::{BuildPlan, PgoTrainingOutcome, Strategy};
use crate::workload::WorkloadFailureKind;

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
    println!(
        "Temper CLI {} and report schema 1 are experimental; 0.x provides no backward-compatibility promise.",
        env!("CARGO_PKG_VERSION")
    );
    let preflight = preflight::run(
        &arguments.manifest_path,
        arguments.allow_dirty,
        arguments.target.as_deref(),
    )?;
    let selection = cargo::resolve_target(&preflight, &arguments)?;
    let workload = workload::WorkloadSpec::new(
        &arguments.workload,
        &selection.workspace_root,
        arguments.timeout,
    )
    .map_err(|error| TemperError::new(error.message))?;
    let mut run = run::Run::create(&arguments, preflight, selection)?;
    let baseline_plan = BuildPlan::baseline(run.target_root());
    let baseline = match cargo::build(run.target(), &baseline_plan) {
        Ok(baseline) => baseline,
        Err(failure) => {
            let message = failure.message.clone();
            run.fail_baseline(failure)?;
            return Err(TemperError::new(message));
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
            return Err(TemperError::new(failure.message));
        }
    };
    let baseline_median = baseline_measurement.median_duration_ns;
    let baseline_dispersion = baseline_measurement.relative_mad;
    let baseline_stable = baseline_measurement.outcome == MeasurementOutcome::Stable;
    run.record_baseline_screening(baseline_measurement)?;
    if !baseline_stable {
        let failure = workload::WorkloadFailure::unstable_baseline(baseline_dispersion);
        run.fail_workload("screening", &failure)?;
        return Err(TemperError::new(failure.message));
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
                return Err(TemperError::new(failure.message));
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
            return Err(TemperError::new(failure.message));
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
                            return Err(TemperError::new(failure.message));
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
    let selected_label = selected.map(Strategy::canonical_identity).unwrap_or("none");
    println!(
        "Bounded search complete: baseline median {baseline_median} ns; selected candidate {selected_label}; confirmation is pending."
    );
    Ok(())
}
