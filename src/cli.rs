use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub(crate) const DEFAULT_MINIMUM_IMPROVEMENT_PERCENT: f64 = 2.0;
pub(crate) const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Parser)]
#[command(
    name = "cargo temper",
    bin_name = "cargo temper",
    version,
    about = "Experimental measured optimization for Rust binaries",
    long_about = "Temper CLI and report schema version 2 are experimental. Interfaces may break during 0.x without a backward-compatibility promise."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a project and build an isolated release baseline.
    ///
    /// Temper CLI and report schema version 2 are experimental. Interfaces may
    /// break during 0.x without a backward-compatibility promise.
    #[command(override_usage = "cargo temper optimize [OPTIONS] -- <WORKLOAD>...")]
    Optimize(OptimizeArgs),
}

#[derive(Clone, Debug, Args)]
pub(crate) struct OptimizeArgs {
    /// Path to the Cargo manifest to optimize.
    #[arg(long, default_value = "Cargo.toml", value_name = "PATH")]
    pub(crate) manifest_path: PathBuf,

    /// Exact workspace package name to select.
    #[arg(long, value_name = "PACKAGE")]
    pub(crate) package: Option<String>,

    /// Exact Cargo binary target name to select.
    #[arg(long = "bin", value_name = "BINARY")]
    pub(crate) binary: Option<String>,

    /// Compilation target. This release accepts only x86_64-unknown-linux-gnu.
    #[arg(long, value_name = "TRIPLE")]
    pub(crate) target: Option<String>,

    /// Minimum confirmed runtime improvement, as a percentage.
    #[arg(
        long,
        default_value_t = DEFAULT_MINIMUM_IMPROVEMENT_PERCENT,
        value_parser = parse_minimum_improvement,
        value_name = "PERCENT"
    )]
    pub(crate) minimum_improvement: f64,

    /// Per-workload invocation timeout in seconds (1 to 86400).
    #[arg(
        long,
        default_value_t = DEFAULT_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=86_400),
        value_name = "SECONDS"
    )]
    pub(crate) timeout: u64,

    /// Permit a dirty Git worktree and record reduced source reproducibility.
    #[arg(long)]
    pub(crate) allow_dirty: bool,

    /// Emit exactly one schema-v2 JSON report on stdout.
    #[arg(long)]
    pub(crate) json: bool,

    /// Workload executable followed by literal arguments. It is never run through a shell.
    #[arg(last = true, num_args = 0.., value_name = "WORKLOAD")]
    pub(crate) workload: Vec<OsString>,
}

pub(crate) enum ParseOutcome {
    Optimize(OptimizeArgs),
    ClapError(Box<clap::Error>),
    MissingWorkload,
}

pub(crate) fn parse() -> ParseOutcome {
    parse_from(std::env::args_os())
}

fn parse_from(arguments: impl IntoIterator<Item = OsString>) -> ParseOutcome {
    let mut arguments: Vec<OsString> = arguments.into_iter().collect();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "temper")
    {
        arguments.remove(1);
    }

    match Cli::try_parse_from(arguments) {
        Ok(Cli {
            command: Command::Optimize(arguments),
        }) if arguments.workload.is_empty() => ParseOutcome::MissingWorkload,
        Ok(Cli {
            command: Command::Optimize(arguments),
        }) => ParseOutcome::Optimize(arguments),
        Err(error) => ParseOutcome::ClapError(Box::new(error)),
    }
}

fn parse_minimum_improvement(value: &str) -> std::result::Result<f64, String> {
    let value = value
        .parse::<f64>()
        .map_err(|_| "minimum improvement must be a number".to_owned())?;
    if value.is_finite() && (0.0..100.0).contains(&value) {
        Ok(value)
    } else {
        Err(
            "minimum improvement must be finite and between 0 (inclusive) and 100 (exclusive)"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ParseOutcome, parse_from};
    use std::ffi::OsString;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn accepts_cargo_supplied_subcommand_name() {
        let outcome = parse_from(arguments(&[
            "cargo-temper",
            "temper",
            "optimize",
            "--",
            "/bin/true",
        ]));
        assert!(matches!(outcome, ParseOutcome::Optimize(_)));
    }

    #[test]
    fn rejects_a_missing_workload_before_execution() {
        let outcome = parse_from(arguments(&["cargo-temper", "optimize"]));
        assert!(matches!(outcome, ParseOutcome::MissingWorkload));
    }
}
