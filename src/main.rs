#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cargo;
mod cli;
mod error;
mod hash;
mod measurement;
mod preflight;
mod run;
mod workload;

use crate::cli::ParseOutcome;
use crate::error::{Result, TemperError};

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

    match cargo::build_baseline(run.target(), run.target_directory()) {
        Ok(baseline) => {
            run.complete_baseline(baseline)?;
            let baseline_path = run
                .baseline_path()
                .map(std::path::Path::to_path_buf)
                .ok_or_else(|| {
                    TemperError::new("Baseline completed without a persisted executable path.")
                })?;
            workload::install_interrupt_handler()?;
            run.begin_baseline_measurement()?;
            match workload.screen(&baseline_path) {
                Ok(measurement) => {
                    let median_duration_ns = measurement.median_duration_ns;
                    run.complete_baseline_measurement(measurement)?;
                    println!(
                        "Baseline measured: {} (median {} ns)",
                        baseline_path.display(),
                        median_duration_ns
                    );
                    Ok(())
                }
                Err(failure) => {
                    run.fail_workload(&failure)?;
                    Err(TemperError::new(failure.message))
                }
            }
        }
        Err(failure) => {
            run.fail_baseline(&failure)?;
            Err(TemperError::new(failure.message))
        }
    }
}
