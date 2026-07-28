use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use serde::Serialize;

use crate::cargo::{self, BuildEnvironmentOverride, BuildFailure, BuildRecord, TargetSelection};
use crate::hash::sha256_file;
use crate::preflight::{SUPPORTED_HOST, cargo_program, rustc_program};
use crate::workload::{WorkloadFailure, WorkloadFailureKind, WorkloadSpec};

const PROFILE_FILE_LIMIT: usize = 10_000;
const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;
const LLVM_TOOLS_HINT: &str =
    "Install the matching component with `rustup component add llvm-tools-preview`.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Strategy {
    Baseline,
    ThinLto,
    FatLtoCgu1,
    Pgo,
}

impl Strategy {
    pub(crate) const STATIC_CANDIDATES: [Self; 2] = [Self::ThinLto, Self::FatLtoCgu1];

    pub(crate) const fn canonical_identity(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::ThinLto => "thin-lto",
            Self::FatLtoCgu1 => "fat-lto-cgu1",
            Self::Pgo => "pgo",
        }
    }

    fn profile_overrides(self) -> Vec<String> {
        match self {
            Self::Baseline => Vec::new(),
            Self::ThinLto => vec!["profile.release.lto=\"thin\"".to_owned()],
            Self::FatLtoCgu1 => vec![
                "profile.release.lto=\"fat\"".to_owned(),
                "profile.release.codegen-units=1".to_owned(),
            ],
            Self::Pgo => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BuildStage {
    Baseline,
    StaticCandidate,
    PgoInstrumentation,
    PgoOptimized,
    Confirmation,
}

#[derive(Clone, Debug)]
pub(crate) struct BuildPlan {
    pub(crate) strategy: Strategy,
    pub(crate) stage: BuildStage,
    pub(crate) target_directory: PathBuf,
    pub(crate) cargo_config_overrides: Vec<String>,
    pub(crate) environment_overrides: Vec<BuildEnvironmentOverride>,
}

impl BuildPlan {
    pub(crate) fn baseline(target_root: &Path) -> Self {
        Self::candidate(Strategy::Baseline, target_root)
    }

    pub(crate) fn candidate(strategy: Strategy, target_root: &Path) -> Self {
        let stage = match strategy {
            Strategy::Baseline => BuildStage::Baseline,
            Strategy::ThinLto | Strategy::FatLtoCgu1 => BuildStage::StaticCandidate,
            Strategy::Pgo => BuildStage::PgoOptimized,
        };
        Self {
            strategy,
            stage,
            target_directory: target_root.join(strategy.canonical_identity()),
            cargo_config_overrides: strategy.profile_overrides(),
            environment_overrides: Vec::new(),
        }
    }

    pub(crate) fn confirmation(
        strategy: Strategy,
        target_root: &Path,
        cargo_config_overrides: Vec<String>,
        environment_overrides: Vec<BuildEnvironmentOverride>,
    ) -> Self {
        Self {
            strategy,
            stage: BuildStage::Confirmation,
            target_directory: target_root
                .join("confirmation")
                .join(strategy.canonical_identity()),
            cargo_config_overrides,
            environment_overrides,
        }
    }

    fn pgo_instrumentation(
        base_strategy: Strategy,
        target_root: &Path,
        project_rustflags: &[String],
        phase_rustflags: &[String],
    ) -> Self {
        Self::pgo(
            base_strategy,
            BuildStage::PgoInstrumentation,
            target_root.join("pgo-instrumented"),
            project_rustflags,
            phase_rustflags,
        )
    }

    fn pgo_optimized(
        base_strategy: Strategy,
        target_root: &Path,
        project_rustflags: &[String],
        phase_rustflags: &[String],
    ) -> Self {
        Self::pgo(
            base_strategy,
            BuildStage::PgoOptimized,
            target_root.join(Strategy::Pgo.canonical_identity()),
            project_rustflags,
            phase_rustflags,
        )
    }

    fn pgo(
        base_strategy: Strategy,
        stage: BuildStage,
        target_directory: PathBuf,
        project_rustflags: &[String],
        phase_rustflags: &[String],
    ) -> Self {
        let mut rustflags = project_rustflags.to_vec();
        rustflags.extend_from_slice(phase_rustflags);
        Self {
            strategy: Strategy::Pgo,
            stage,
            target_directory,
            cargo_config_overrides: base_strategy.profile_overrides(),
            environment_overrides: vec![BuildEnvironmentOverride::cargo_encoded_rustflags(
                rustflags,
            )],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ConfigSourceRecord {
    path: PathBuf,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ToolIdentity {
    cargo_version: String,
    rustc_version: String,
    llvm_profdata_path: PathBuf,
    llvm_profdata_sha256: String,
    llvm_profdata_version: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PgoPrerequisites {
    llvm_profdata_path: PathBuf,
    target_libdir: PathBuf,
    preserved_target_rustflags: Vec<String>,
    inspected_config_sources: Vec<ConfigSourceRecord>,
    tool_identity: ToolIdentity,
}

#[derive(Clone, Debug, Serialize)]
struct PgoPhaseInputs {
    package_id: String,
    binary_target: String,
    target_triple: &'static str,
    cargo_arguments: Vec<String>,
    base_profile_overrides: Vec<String>,
    project_rustflags: Vec<String>,
    config_sources: Vec<ConfigSourceRecord>,
    cargo_identity: String,
    rustc_identity: String,
    llvm_profdata_identity: ToolIdentity,
    target_directory: PathBuf,
    environment_overrides: Vec<BuildEnvironmentOverride>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PgoParityRecord {
    permitted_differences: [&'static str; 3],
    instrumentation: Option<PgoPhaseInputs>,
    optimization: Option<PgoPhaseInputs>,
    matched: bool,
    unexpected_differences: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PgoTrainingOutcomeKind {
    Trained,
    Rejected,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PgoFailureStage {
    Prerequisites,
    Instrumentation,
    Training,
    ProfileDiscovery,
    ProfileMerge,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProfileMergeOutcome {
    Merged,
    Rejected,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProfileFileRecord {
    path: PathBuf,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProfileMergeRecord {
    outcome: ProfileMergeOutcome,
    llvm_profdata_path: PathBuf,
    arguments: Vec<String>,
    profraw_files: Vec<ProfileFileRecord>,
    profdata_path: PathBuf,
    profdata_size_bytes: Option<u64>,
    profdata_sha256: Option<String>,
    bounded_diagnostics: String,
    diagnostics_truncated: bool,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PgoTrainingRecord {
    pub(crate) outcome: PgoTrainingOutcomeKind,
    pub(crate) base_strategy: Strategy,
    prerequisites: Option<PgoPrerequisites>,
    instrumentation_build: Option<BuildRecord>,
    instrumentation_failure: Option<BuildFailure>,
    training_duration_ns: Option<u64>,
    raw_profile_files: Vec<ProfileFileRecord>,
    merge: Option<ProfileMergeRecord>,
    phase_parity: PgoParityRecord,
    failure_stage: Option<PgoFailureStage>,
    rejection_reason: Option<String>,
    message: Option<String>,
    remediation: Option<&'static str>,
}

pub(crate) struct PgoTrainingSuccess {
    pub(crate) record: PgoTrainingRecord,
    pub(crate) optimized_plan: BuildPlan,
}

pub(crate) enum PgoTrainingOutcome {
    Trained(PgoTrainingSuccess),
    Rejected(PgoTrainingRecord),
    Interrupted(PgoTrainingRecord, WorkloadFailure),
}

pub(crate) fn train_pgo(
    selection: &TargetSelection,
    workload: &WorkloadSpec,
    run_directory: &Path,
    target_root: &Path,
    base_strategy: Strategy,
) -> PgoTrainingOutcome {
    let prerequisites = match prove_pgo_prerequisites(&selection.workspace_root) {
        Ok(prerequisites) => prerequisites,
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
                base_strategy,
                PgoFailureStage::Prerequisites,
                failure.reason,
                failure.message,
                PgoParityRecord::unavailable("instrumentation_inputs_unavailable"),
                failure.remediation,
            ));
        }
    };
    let raw_profile_directory = run_directory.join("pgo").join("raw");
    if let Err(error) = fs::create_dir_all(&raw_profile_directory) {
        return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
            base_strategy,
            PgoFailureStage::Instrumentation,
            "pgo_profile_directory_failed",
            format!(
                "Could not create the run-scoped PGO profile directory {}: {error}",
                raw_profile_directory.display()
            ),
            PgoParityRecord::unavailable("instrumentation_inputs_unavailable"),
            None,
        ));
    }
    let raw_profile_directory = match fs::canonicalize(&raw_profile_directory) {
        Ok(directory) => directory,
        Err(error) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
                base_strategy,
                PgoFailureStage::Instrumentation,
                "pgo_profile_directory_failed",
                format!("Could not canonicalize the PGO profile directory: {error}"),
                PgoParityRecord::unavailable("instrumentation_inputs_unavailable"),
                None,
            ));
        }
    };
    let generate_flag = match path_flag("-Cprofile-generate=", &raw_profile_directory) {
        Ok(flag) => flag,
        Err(message) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
                base_strategy,
                PgoFailureStage::Instrumentation,
                "pgo_profile_path_not_utf8",
                message,
                PgoParityRecord::unavailable("instrumentation_inputs_unavailable"),
                None,
            ));
        }
    };
    let generate_rustflags = [generate_flag];
    let instrumentation_plan = BuildPlan::pgo_instrumentation(
        base_strategy,
        target_root,
        &prerequisites.preserved_target_rustflags,
        &generate_rustflags,
    );
    let instrumentation_inputs = phase_inputs(selection, &instrumentation_plan, &prerequisites);
    let instrumentation_parity =
        PgoParityRecord::instrumentation_only(instrumentation_inputs.clone());
    let instrumentation_build = match cargo::build(selection, &instrumentation_plan) {
        Ok(build) => build,
        Err(failure) => {
            let message = failure.message.clone();
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: None,
                instrumentation_failure: Some(failure),
                training_duration_ns: None,
                raw_profile_files: Vec::new(),
                merge: None,
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::Instrumentation),
                rejection_reason: Some("pgo_instrumentation_failed".to_owned()),
                message: Some(message),
                remediation: None,
            });
        }
    };

    let profile_pattern = raw_profile_directory.join("temper-%m-%p.profraw");
    let training = match workload.invoke_with_environment(
        &instrumentation_build.executable_path,
        &[("LLVM_PROFILE_FILE", profile_pattern.as_os_str())],
    ) {
        Ok(training) => training,
        Err(failure) if failure.kind == WorkloadFailureKind::Interrupted => {
            let record = PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: None,
                raw_profile_files: Vec::new(),
                merge: None,
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::Training),
                rejection_reason: Some("pgo_training_interrupted".to_owned()),
                message: Some(failure.message.clone()),
                remediation: None,
            };
            return PgoTrainingOutcome::Interrupted(record, failure);
        }
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: None,
                raw_profile_files: Vec::new(),
                merge: None,
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::Training),
                rejection_reason: Some("pgo_training_failed".to_owned()),
                message: Some(failure.message),
                remediation: None,
            });
        }
    };
    let raw_profile_files = match collect_profile_files(&raw_profile_directory) {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: Some(training.duration_ns),
                raw_profile_files: Vec::new(),
                merge: None,
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::ProfileDiscovery),
                rejection_reason: Some("pgo_training_produced_no_profraw_files".to_owned()),
                message: Some("PGO training produced no raw profile files.".to_owned()),
                remediation: None,
            });
        }
        Err(message) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: Some(training.duration_ns),
                raw_profile_files: Vec::new(),
                merge: None,
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::ProfileDiscovery),
                rejection_reason: Some("pgo_profile_discovery_failed".to_owned()),
                message: Some(message),
                remediation: None,
            });
        }
    };
    let profdata_path = run_directory.join("pgo").join("merged.profdata");
    let merge = merge_profiles(
        &prerequisites.llvm_profdata_path,
        &raw_profile_files,
        &profdata_path,
        &run_directory.join("pgo"),
    );
    if matches!(merge.outcome, ProfileMergeOutcome::Rejected) {
        let reason = merge
            .message
            .clone()
            .unwrap_or_else(|| "llvm-profdata merge rejected the profile data.".to_owned());
        return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
            outcome: PgoTrainingOutcomeKind::Rejected,
            base_strategy,
            prerequisites: Some(prerequisites),
            instrumentation_build: Some(instrumentation_build),
            instrumentation_failure: None,
            training_duration_ns: Some(training.duration_ns),
            raw_profile_files,
            merge: Some(merge),
            phase_parity: instrumentation_parity,
            failure_stage: Some(PgoFailureStage::ProfileMerge),
            rejection_reason: Some("pgo_profile_merge_failed".to_owned()),
            message: Some(reason),
            remediation: None,
        });
    }
    let merged_profile = match fs::canonicalize(&profdata_path) {
        Ok(profile) => profile,
        Err(error) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: Some(training.duration_ns),
                raw_profile_files,
                merge: Some(merge),
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::ProfileMerge),
                rejection_reason: Some("pgo_merged_profile_unavailable".to_owned()),
                message: Some(format!(
                    "Merged PGO profile could not be canonicalized: {error}"
                )),
                remediation: None,
            });
        }
    };
    let use_flag = match path_flag("-Cprofile-use=", &merged_profile) {
        Ok(flag) => flag,
        Err(message) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: Some(training.duration_ns),
                raw_profile_files,
                merge: Some(merge),
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::ProfileMerge),
                rejection_reason: Some("pgo_profile_path_not_utf8".to_owned()),
                message: Some(message),
                remediation: None,
            });
        }
    };
    let use_rustflags = [
        use_flag,
        "-Cllvm-args=-pgo-warn-missing-function".to_owned(),
    ];
    let optimization_prerequisites = match prove_pgo_prerequisites(&selection.workspace_root) {
        Ok(prerequisites) => prerequisites,
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: Some(instrumentation_build),
                instrumentation_failure: None,
                training_duration_ns: Some(training.duration_ns),
                raw_profile_files,
                merge: Some(merge),
                phase_parity: instrumentation_parity,
                failure_stage: Some(PgoFailureStage::Prerequisites),
                rejection_reason: Some(failure.reason.to_owned()),
                message: Some(failure.message),
                remediation: failure.remediation,
            });
        }
    };
    let optimized_plan = BuildPlan::pgo_optimized(
        base_strategy,
        target_root,
        &optimization_prerequisites.preserved_target_rustflags,
        &use_rustflags,
    );
    let optimization_inputs = phase_inputs(selection, &optimized_plan, &optimization_prerequisites);
    let phase_parity = PgoParityRecord::compare(instrumentation_inputs, optimization_inputs);
    if !phase_parity.matched {
        return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
            outcome: PgoTrainingOutcomeKind::Rejected,
            base_strategy,
            prerequisites: Some(prerequisites),
            instrumentation_build: Some(instrumentation_build),
            instrumentation_failure: None,
            training_duration_ns: Some(training.duration_ns),
            raw_profile_files,
            merge: Some(merge),
            phase_parity,
            failure_stage: Some(PgoFailureStage::Prerequisites),
            rejection_reason: Some("pgo_phase_parity_mismatch".to_owned()),
            message: Some(
                "PGO optimization inputs differed from the instrumentation inputs outside the explicit allowlist."
                    .to_owned(),
            ),
            remediation: None,
        });
    }
    let record = PgoTrainingRecord {
        outcome: PgoTrainingOutcomeKind::Trained,
        base_strategy,
        prerequisites: Some(prerequisites),
        instrumentation_build: Some(instrumentation_build),
        instrumentation_failure: None,
        training_duration_ns: Some(training.duration_ns),
        raw_profile_files,
        merge: Some(merge),
        phase_parity,
        failure_stage: None,
        rejection_reason: None,
        message: None,
        remediation: None,
    };
    PgoTrainingOutcome::Trained(PgoTrainingSuccess {
        record,
        optimized_plan,
    })
}

impl PgoTrainingRecord {
    fn rejected(
        base_strategy: Strategy,
        stage: PgoFailureStage,
        reason: &'static str,
        message: String,
        phase_parity: PgoParityRecord,
        remediation: Option<&'static str>,
    ) -> Self {
        Self {
            outcome: PgoTrainingOutcomeKind::Rejected,
            base_strategy,
            prerequisites: None,
            instrumentation_build: None,
            instrumentation_failure: None,
            training_duration_ns: None,
            raw_profile_files: Vec::new(),
            merge: None,
            phase_parity,
            failure_stage: Some(stage),
            rejection_reason: Some(reason.to_owned()),
            message: Some(message),
            remediation,
        }
    }
}

impl PgoParityRecord {
    const PERMITTED_DIFFERENCES: [&'static str; 3] = [
        "target_directory",
        "profile_generate_vs_use",
        "pgo_warn_missing_function_in_use",
    ];

    fn unavailable(field: &str) -> Self {
        Self {
            permitted_differences: Self::PERMITTED_DIFFERENCES,
            instrumentation: None,
            optimization: None,
            matched: false,
            unexpected_differences: vec![field.to_owned()],
        }
    }

    fn instrumentation_only(instrumentation: PgoPhaseInputs) -> Self {
        Self {
            permitted_differences: Self::PERMITTED_DIFFERENCES,
            instrumentation: Some(instrumentation),
            optimization: None,
            matched: false,
            unexpected_differences: vec!["optimization_inputs_unavailable".to_owned()],
        }
    }

    fn compare(instrumentation: PgoPhaseInputs, optimization: PgoPhaseInputs) -> Self {
        let mut unexpected = Vec::new();
        compare_field(
            "package_id",
            &instrumentation.package_id,
            &optimization.package_id,
            &mut unexpected,
        );
        compare_field(
            "binary_target",
            &instrumentation.binary_target,
            &optimization.binary_target,
            &mut unexpected,
        );
        compare_field(
            "target_triple",
            &instrumentation.target_triple,
            &optimization.target_triple,
            &mut unexpected,
        );
        compare_field(
            "cargo_arguments",
            &normalized_cargo_arguments(&instrumentation.cargo_arguments),
            &normalized_cargo_arguments(&optimization.cargo_arguments),
            &mut unexpected,
        );
        compare_field(
            "base_profile_overrides",
            &instrumentation.base_profile_overrides,
            &optimization.base_profile_overrides,
            &mut unexpected,
        );
        compare_field(
            "project_rustflags",
            &instrumentation.project_rustflags,
            &optimization.project_rustflags,
            &mut unexpected,
        );
        compare_field(
            "config_sources",
            &instrumentation.config_sources,
            &optimization.config_sources,
            &mut unexpected,
        );
        compare_field(
            "cargo_identity",
            &instrumentation.cargo_identity,
            &optimization.cargo_identity,
            &mut unexpected,
        );
        compare_field(
            "rustc_identity",
            &instrumentation.rustc_identity,
            &optimization.rustc_identity,
            &mut unexpected,
        );
        compare_field(
            "llvm_profdata_identity",
            &instrumentation.llvm_profdata_identity,
            &optimization.llvm_profdata_identity,
            &mut unexpected,
        );
        if !owned_pgo_channels_are_valid(&instrumentation, &optimization) {
            unexpected.push("pgo_environment_channel".to_owned());
        }
        Self {
            permitted_differences: Self::PERMITTED_DIFFERENCES,
            instrumentation: Some(instrumentation),
            optimization: Some(optimization),
            matched: unexpected.is_empty(),
            unexpected_differences: unexpected,
        }
    }
}

fn phase_inputs(
    selection: &TargetSelection,
    plan: &BuildPlan,
    prerequisites: &PgoPrerequisites,
) -> PgoPhaseInputs {
    let invocation = cargo::planned_invocation(selection, plan);
    PgoPhaseInputs {
        package_id: selection.package_id.clone(),
        binary_target: selection.binary_name.clone(),
        target_triple: SUPPORTED_HOST,
        cargo_arguments: invocation.cargo_arguments,
        base_profile_overrides: invocation.cargo_config_overrides,
        project_rustflags: prerequisites.preserved_target_rustflags.clone(),
        config_sources: prerequisites.inspected_config_sources.clone(),
        cargo_identity: prerequisites.tool_identity.cargo_version.clone(),
        rustc_identity: prerequisites.tool_identity.rustc_version.clone(),
        llvm_profdata_identity: prerequisites.tool_identity.clone(),
        target_directory: invocation.target_directory,
        environment_overrides: invocation.environment_overrides,
    }
}

fn owned_pgo_channels_are_valid(
    instrumentation: &PgoPhaseInputs,
    optimization: &PgoPhaseInputs,
) -> bool {
    let Some(instrumentation_flags) = owned_pgo_flags(instrumentation) else {
        return false;
    };
    let Some(optimization_flags) = owned_pgo_flags(optimization) else {
        return false;
    };
    let instrumentation_valid = matches!(
        instrumentation_flags,
        [generate] if generate.starts_with("-Cprofile-generate=")
    );
    let optimization_valid = match optimization_flags {
        [profile_use] => profile_use.starts_with("-Cprofile-use="),
        [profile_use, warning] => {
            profile_use.starts_with("-Cprofile-use=")
                && warning == "-Cllvm-args=-pgo-warn-missing-function"
        }
        _ => false,
    };
    instrumentation_valid && optimization_valid
}

fn owned_pgo_flags(inputs: &PgoPhaseInputs) -> Option<&[String]> {
    let [environment] = inputs.environment_overrides.as_slice() else {
        return None;
    };
    if environment.name != "CARGO_ENCODED_RUSTFLAGS"
        || !environment.arguments.starts_with(&inputs.project_rustflags)
    {
        return None;
    }
    Some(&environment.arguments[inputs.project_rustflags.len()..])
}

fn normalized_cargo_arguments(arguments: &[String]) -> Vec<String> {
    let mut normalized = arguments.to_vec();
    if let Some(index) = normalized
        .iter()
        .position(|argument| argument == "--target-dir")
        && let Some(target_directory) = normalized.get_mut(index + 1)
    {
        *target_directory = "<isolated-target-directory>".to_owned();
    }
    normalized
}

fn compare_field<T: PartialEq>(name: &str, left: &T, right: &T, differences: &mut Vec<String>) {
    if left != right {
        differences.push(name.to_owned());
    }
}

#[derive(Debug)]
struct PgoPrerequisiteFailure {
    reason: &'static str,
    message: String,
    remediation: Option<&'static str>,
}

fn prove_pgo_prerequisites(
    workspace_root: &Path,
) -> Result<PgoPrerequisites, PgoPrerequisiteFailure> {
    for variable in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
    ] {
        if std::env::var_os(variable).is_some() {
            return Err(PgoPrerequisiteFailure {
                reason: "ambient_rustflags",
                message: format!(
                    "PGO was rejected because {variable} is set and Temper cannot prove flag composition."
                ),
                remediation: None,
            });
        }
    }
    for variable in [
        "CARGO_BUILD_RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        if std::env::var_os(variable).is_some() {
            return Err(PgoPrerequisiteFailure {
                reason: "ambient_compiler_override",
                message: format!(
                    "PGO was rejected because {variable} is set and Temper cannot prove the active rustc toolchain."
                ),
                remediation: None,
            });
        }
    }

    let (preserved_target_rustflags, inspected_config_sources) =
        effective_target_rustflags(workspace_root)?;
    let target_libdir =
        rustc_target_libdir(workspace_root).map_err(|message| PgoPrerequisiteFailure {
            reason: "rustc_target_libdir_failed",
            message,
            remediation: None,
        })?;
    let llvm_profdata_path = target_libdir
        .parent()
        .map(|target_root| target_root.join("bin").join("llvm-profdata"))
        .ok_or_else(|| PgoPrerequisiteFailure {
            reason: "llvm_profdata_unavailable",
            message: "rustc target-libdir has no toolchain target parent directory.".to_owned(),
            remediation: Some(LLVM_TOOLS_HINT),
        })?;
    let metadata = fs::metadata(&llvm_profdata_path).map_err(|_| PgoPrerequisiteFailure {
        reason: "llvm_profdata_unavailable",
        message: format!(
            "The active rustc has no adjacent executable llvm-profdata at {}.",
            llvm_profdata_path.display()
        ),
        remediation: Some(LLVM_TOOLS_HINT),
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(PgoPrerequisiteFailure {
            reason: "llvm_profdata_unavailable",
            message: format!(
                "The llvm-profdata adjacent to the active rustc is not executable: {}.",
                llvm_profdata_path.display()
            ),
            remediation: Some(LLVM_TOOLS_HINT),
        });
    }
    let llvm_profdata_path =
        fs::canonicalize(&llvm_profdata_path).map_err(|error| PgoPrerequisiteFailure {
            reason: "llvm_profdata_unavailable",
            message: format!("Could not canonicalize toolchain llvm-profdata: {error}"),
            remediation: Some(LLVM_TOOLS_HINT),
        })?;
    let cargo_version = tool_version(cargo_program(), &["-Vv"], "Cargo").map_err(|message| {
        PgoPrerequisiteFailure {
            reason: "cargo_identity_failed",
            message,
            remediation: None,
        }
    })?;
    let rustc_version = tool_version(rustc_program(), &["-Vv"], "rustc").map_err(|message| {
        PgoPrerequisiteFailure {
            reason: "rustc_identity_failed",
            message,
            remediation: None,
        }
    })?;
    let llvm_profdata_version = tool_version(
        llvm_profdata_path.as_os_str().to_owned(),
        &["--version"],
        "llvm-profdata",
    )
    .map_err(|message| PgoPrerequisiteFailure {
        reason: "llvm_profdata_identity_failed",
        message,
        remediation: Some(LLVM_TOOLS_HINT),
    })?;
    let llvm_profdata_sha256 =
        sha256_file(&llvm_profdata_path).map_err(|error| PgoPrerequisiteFailure {
            reason: "llvm_profdata_identity_failed",
            message: error.to_string(),
            remediation: Some(LLVM_TOOLS_HINT),
        })?;

    Ok(PgoPrerequisites {
        llvm_profdata_path: llvm_profdata_path.clone(),
        target_libdir,
        preserved_target_rustflags,
        inspected_config_sources,
        tool_identity: ToolIdentity {
            cargo_version,
            rustc_version,
            llvm_profdata_path,
            llvm_profdata_sha256,
            llvm_profdata_version,
        },
    })
}

fn tool_version(program: OsString, arguments: &[&str], name: &str) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|_| format!("{name} identity probe could not start."))?;
    if !output.status.success() {
        return Err(format!("{name} identity probe failed."));
    }
    if output.stdout.len() > TOOL_OUTPUT_LIMIT {
        return Err(format!("{name} identity output exceeded 64 KiB."));
    }
    String::from_utf8(output.stdout)
        .map(|output| output.trim().to_owned())
        .map_err(|_| format!("{name} identity output was not valid UTF-8."))
}

fn rustc_target_libdir(workspace_root: &Path) -> Result<PathBuf, String> {
    let output = Command::new(rustc_program())
        .args(["--print", "target-libdir", "--target", SUPPORTED_HOST])
        .current_dir(workspace_root)
        .output()
        .map_err(|_| "The active rustc target-libdir probe could not start.".to_owned())?;
    if !output.status.success() {
        return Err("The active rustc could not report its target-libdir.".to_owned());
    }
    if output.stdout.len() > TOOL_OUTPUT_LIMIT {
        return Err("rustc target-libdir output exceeded 64 KiB.".to_owned());
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "rustc target-libdir was not valid UTF-8.".to_owned())?;
    fs::canonicalize(path.trim())
        .map_err(|error| format!("rustc target-libdir could not be canonicalized: {error}"))
}

fn effective_target_rustflags(
    workspace_root: &Path,
) -> Result<(Vec<String>, Vec<ConfigSourceRecord>), PgoPrerequisiteFailure> {
    let paths = cargo_config_paths(workspace_root).map_err(|message| PgoPrerequisiteFailure {
        reason: "cargo_config_inspection_failed",
        message,
        remediation: None,
    })?;
    let mut rustflags = Vec::new();
    let mut target_sources = 0_usize;
    let mut inspected_sources = Vec::new();
    for path in &paths {
        let contents = fs::read_to_string(path).map_err(|error| PgoPrerequisiteFailure {
            reason: "cargo_config_read_failed",
            message: format!("Cargo config {} could not be read: {error}", path.display()),
            remediation: None,
        })?;
        let sha256 = sha256_file(path).map_err(|error| PgoPrerequisiteFailure {
            reason: "cargo_config_hash_failed",
            message: error.to_string(),
            remediation: None,
        })?;
        inspected_sources.push(ConfigSourceRecord {
            path: path.clone(),
            sha256,
        });
        let config: toml::Value =
            toml::from_str(&contents).map_err(|error| PgoPrerequisiteFailure {
                reason: "cargo_config_parse_failed",
                message: format!(
                    "Cargo config {} could not be parsed: {error}",
                    path.display()
                ),
                remediation: None,
            })?;
        if let Some(build) = config.get("build") {
            if build.get("rustflags").is_some() {
                return Err(PgoPrerequisiteFailure {
                    reason: "unproven_build_rustflags",
                    message: format!(
                        "PGO was rejected because effective build.rustflags may come from {}.",
                        path.display()
                    ),
                    remediation: None,
                });
            }
            for key in ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"] {
                if build.get(key).is_some() {
                    return Err(PgoPrerequisiteFailure {
                        reason: "unproven_compiler_override",
                        message: format!(
                            "PGO was rejected because effective build.{key} may come from {}.",
                            path.display()
                        ),
                        remediation: None,
                    });
                }
            }
        }
        let Some(targets) = config.get("target").and_then(toml::Value::as_table) else {
            continue;
        };
        for (target, settings) in targets {
            let Some(value) = settings.get("rustflags") else {
                continue;
            };
            if target == SUPPORTED_HOST {
                target_sources += 1;
                if target_sources > 1 {
                    return Err(PgoPrerequisiteFailure {
                        reason: "ambiguous_target_rustflags",
                        message:
                            "PGO was rejected because multiple target-specific rustflags sources make composition ambiguous."
                                .to_owned(),
                        remediation: None,
                    });
                }
                rustflags = rustflags_value(value, path)?;
            } else if target.trim_start().starts_with("cfg(") {
                return Err(PgoPrerequisiteFailure {
                    reason: "unproven_cfg_rustflags",
                    message: format!(
                        "PGO was rejected because cfg-selected target rustflags in {} cannot be proven composable.",
                        path.display()
                    ),
                    remediation: None,
                });
            }
        }
    }
    Ok((rustflags, inspected_sources))
}

fn rustflags_value(
    value: &toml::Value,
    path: &Path,
) -> Result<Vec<String>, PgoPrerequisiteFailure> {
    if let Some(flags) = value.as_str() {
        if flags.contains('\u{1f}') {
            return Err(PgoPrerequisiteFailure {
                reason: "unsupported_target_rustflags_encoding",
                message: format!(
                    "Target rustflags in {} contain Cargo's encoded-rustflags separator.",
                    path.display()
                ),
                remediation: None,
            });
        }
        if flags
            .chars()
            .any(|character| matches!(character, '"' | '\'' | '\\'))
        {
            return Err(PgoPrerequisiteFailure {
                reason: "unsupported_string_rustflags_boundaries",
                message: format!(
                    "Target rustflags in {} use quoting or escaping that Temper cannot preserve without reinterpreting Cargo's argument boundaries.",
                    path.display()
                ),
                remediation: None,
            });
        }
        return Ok(flags.split_ascii_whitespace().map(str::to_owned).collect());
    }
    let flags = value.as_array().ok_or_else(|| PgoPrerequisiteFailure {
        reason: "invalid_target_rustflags",
        message: format!(
            "Target rustflags in {} must be a string or an array of strings.",
            path.display()
        ),
        remediation: None,
    })?;
    flags
        .iter()
        .map(|flag| {
            let flag = flag.as_str().ok_or_else(|| PgoPrerequisiteFailure {
                reason: "invalid_target_rustflags",
                message: format!(
                    "Target rustflags in {} contain a non-string value.",
                    path.display()
                ),
                remediation: None,
            })?;
            if flag.contains('\u{1f}') {
                return Err(PgoPrerequisiteFailure {
                    reason: "unsupported_target_rustflags_encoding",
                    message: format!(
                        "Target rustflags in {} contain Cargo's encoded-rustflags separator.",
                        path.display()
                    ),
                    remediation: None,
                });
            }
            Ok(flag.to_owned())
        })
        .collect()
}

fn cargo_config_paths(workspace_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    if let Some(cargo_home) = cargo_home(workspace_root)
        && let Some(path) = active_config_path(&cargo_home)
    {
        paths.push(path);
    }
    let mut ancestors: Vec<&Path> = workspace_root.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if let Some(path) = active_config_path(&ancestor.join(".cargo"))
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn cargo_home(workspace_root: &Path) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("CARGO_HOME") {
        let configured = PathBuf::from(configured);
        return Some(if configured.is_absolute() {
            configured
        } else {
            workspace_root.join(configured)
        });
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".cargo"))
}

fn active_config_path(directory: &Path) -> Option<PathBuf> {
    let legacy = directory.join("config");
    if legacy.is_file() {
        Some(legacy)
    } else {
        let toml = directory.join("config.toml");
        toml.is_file().then_some(toml)
    }
}

fn path_flag(prefix: &str, path: &Path) -> Result<String, String> {
    path.to_str()
        .map(|path| format!("{prefix}{path}"))
        .ok_or_else(|| {
            format!(
                "PGO requires a UTF-8 run path for target-scoped rustflags: {}.",
                path.display()
            )
        })
}

fn collect_profile_files(root: &Path) -> Result<Vec<ProfileFileRecord>, String> {
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("Could not inspect PGO profile root: {error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("PGO profile root must be a regular directory.".to_owned());
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("Could not canonicalize PGO profile root: {error}"))?;
    let mut pending = vec![canonical_root.clone()];
    let mut profiles = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            format!(
                "Could not inspect PGO profile directory {}: {error}",
                directory.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not inspect PGO profile: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect PGO profile type: {error}"))?;
            let path = entry.path();
            if file_type.is_symlink() {
                return Err("PGO profile discovery rejected a symbolic link.".to_owned());
            }
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !path
                .extension()
                .is_some_and(|extension| extension == "profraw")
            {
                continue;
            }
            if !file_type.is_file() {
                return Err("PGO raw profiles must be regular files.".to_owned());
            }
            let canonical_path = fs::canonicalize(&path)
                .map_err(|error| format!("Could not canonicalize PGO raw profile: {error}"))?;
            if !canonical_path.starts_with(&canonical_root) {
                return Err("PGO raw profile escaped the run profile directory.".to_owned());
            }
            let metadata = fs::metadata(&canonical_path)
                .map_err(|error| format!("Could not inspect PGO raw profile: {error}"))?;
            if !metadata.is_file() {
                return Err("PGO raw profiles must be regular files.".to_owned());
            }
            let sha256 = sha256_file(&canonical_path)
                .map_err(|error| format!("Could not hash PGO raw profile: {error}"))?;
            profiles.push(ProfileFileRecord {
                path: canonical_path,
                size_bytes: metadata.len(),
                sha256,
            });
            if profiles.len() > PROFILE_FILE_LIMIT {
                return Err(format!(
                    "PGO training produced more than {PROFILE_FILE_LIMIT} profile files."
                ));
            }
        }
    }
    profiles.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(profiles)
}

fn merge_profiles(
    llvm_profdata: &Path,
    profiles: &[ProfileFileRecord],
    profdata_path: &Path,
    pgo_directory: &Path,
) -> ProfileMergeRecord {
    let mut arguments: Vec<OsString> =
        vec!["merge".into(), "--failure-mode=any".into(), "-o".into()];
    arguments.push(profdata_path.as_os_str().to_owned());
    arguments.extend(
        profiles
            .iter()
            .map(|profile| profile.path.as_os_str().to_owned()),
    );
    let serialized_arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let mut record = ProfileMergeRecord {
        outcome: ProfileMergeOutcome::Rejected,
        llvm_profdata_path: llvm_profdata.to_path_buf(),
        arguments: serialized_arguments,
        profraw_files: profiles.to_vec(),
        profdata_path: profdata_path.to_path_buf(),
        profdata_size_bytes: None,
        profdata_sha256: None,
        bounded_diagnostics: String::new(),
        diagnostics_truncated: false,
        message: None,
    };
    let canonical_pgo_directory = match fs::canonicalize(pgo_directory) {
        Ok(directory) => directory,
        Err(error) => {
            record.message = Some(format!("PGO directory could not be canonicalized: {error}"));
            return record;
        }
    };
    let Some(profdata_parent) = profdata_path.parent() else {
        record.message = Some("Merged PGO profile has no parent directory.".to_owned());
        return record;
    };
    let canonical_parent = match fs::canonicalize(profdata_parent) {
        Ok(parent) => parent,
        Err(error) => {
            record.message = Some(format!(
                "Merged PGO profile parent could not be canonicalized: {error}"
            ));
            return record;
        }
    };
    if canonical_parent != canonical_pgo_directory {
        record.message = Some("Merged PGO profile path escaped the run PGO directory.".to_owned());
        return record;
    }
    match fs::symlink_metadata(profdata_path) {
        Ok(_) => {
            record.message =
                Some("Merged PGO profile output already exists before merge.".to_owned());
            return record;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            record.message = Some(format!(
                "Merged PGO profile output could not be inspected before merge: {error}"
            ));
            return record;
        }
    }
    let mut child = match Command::new(llvm_profdata)
        .args(&arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            record.message = Some(format!("llvm-profdata merge could not start: {error}"));
            return record;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ignored = child.kill();
        let _ignored = child.wait();
        record.message = Some("llvm-profdata merge stdout was unavailable.".to_owned());
        return record;
    };
    let Some(stderr) = child.stderr.take() else {
        let _ignored = child.kill();
        let _ignored = child.wait();
        record.message = Some("llvm-profdata merge stderr was unavailable.".to_owned());
        return record;
    };
    let stdout_reader = thread::spawn(move || drain_tool_stream(stdout));
    let stderr_reader = thread::spawn(move || drain_tool_stream(stderr));
    let status = child.wait();
    let stdout = match stdout_reader.join() {
        Ok(output) => output,
        Err(_) => {
            record.message =
                Some("llvm-profdata merge stdout reader terminated unexpectedly.".to_owned());
            return record;
        }
    };
    let stderr = match stderr_reader.join() {
        Ok(output) => output,
        Err(_) => {
            record.message =
                Some("llvm-profdata merge stderr reader terminated unexpectedly.".to_owned());
            return record;
        }
    };
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            record.message = Some(format!("llvm-profdata merge wait failed: {error}"));
            return record;
        }
    };
    let (diagnostics, combined_truncated) = bounded_diagnostics(&stdout.bytes, &stderr.bytes);
    record.bounded_diagnostics = diagnostics;
    record.diagnostics_truncated = stdout.truncated || stderr.truncated || combined_truncated;
    if !status.success() {
        record.message = Some("llvm-profdata merge failed.".to_owned());
        return record;
    }
    if !record.bounded_diagnostics.trim().is_empty() {
        record.message = Some("llvm-profdata merge emitted warnings.".to_owned());
        return record;
    }
    let metadata = match fs::symlink_metadata(profdata_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            record.message = Some("Merged PGO profile must not be a symbolic link.".to_owned());
            return record;
        }
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        Ok(_) => {
            record.message =
                Some("Merged PGO profile must be one non-empty regular file.".to_owned());
            return record;
        }
        Err(error) => {
            record.message = Some(format!(
                "Merged PGO profile could not be inspected: {error}"
            ));
            return record;
        }
    };
    let canonical_profdata = match fs::canonicalize(profdata_path) {
        Ok(path) if path.starts_with(&canonical_pgo_directory) => path,
        Ok(_) => {
            record.message = Some("Merged PGO profile escaped the run PGO directory.".to_owned());
            return record;
        }
        Err(error) => {
            record.message = Some(format!(
                "Merged PGO profile could not be canonicalized: {error}"
            ));
            return record;
        }
    };
    match sha256_file(&canonical_profdata) {
        Ok(sha256) => {
            record.outcome = ProfileMergeOutcome::Merged;
            record.profdata_path = canonical_profdata;
            record.profdata_size_bytes = Some(metadata.len());
            record.profdata_sha256 = Some(sha256);
        }
        Err(error) => {
            record.message = Some(format!("Merged PGO profile could not be recorded: {error}"));
        }
    }
    record
}

struct BoundedToolStream {
    bytes: Vec<u8>,
    truncated: bool,
}

fn drain_tool_stream(mut reader: impl Read) -> BoundedToolStream {
    let mut bytes = Vec::with_capacity(TOOL_OUTPUT_LIMIT);
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = TOOL_OUTPUT_LIMIT.saturating_sub(bytes.len());
                let retained = remaining.min(read);
                bytes.extend_from_slice(&buffer[..retained]);
                truncated |= retained < read;
            }
            Err(error) => {
                let message = format!("Could not read tool diagnostics: {error}");
                let remaining = TOOL_OUTPUT_LIMIT.saturating_sub(bytes.len());
                let retained = remaining.min(message.len());
                bytes.extend_from_slice(&message.as_bytes()[..retained]);
                truncated |= retained < message.len();
                break;
            }
        }
    }
    BoundedToolStream { bytes, truncated }
}

fn bounded_diagnostics(stdout: &[u8], stderr: &[u8]) -> (String, bool) {
    let mut diagnostics = String::new();
    let mut truncated = false;
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        if bytes.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        let prefix = format!("{label}: ");
        let separator_length = usize::from(!diagnostics.is_empty());
        let available =
            TOOL_OUTPUT_LIMIT.saturating_sub(diagnostics.len() + separator_length + prefix.len());
        if available == 0 {
            truncated = true;
            continue;
        }
        if separator_length == 1 {
            diagnostics.push('\n');
        }
        diagnostics.push_str(&prefix);
        let mut boundary = available.min(text.len());
        while !text.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        diagnostics.push_str(&text[..boundary]);
        truncated |= boundary < text.len();
    }
    (diagnostics, truncated)
}

pub(crate) fn lowest_median_strategy(measurements: &[(Strategy, u64)]) -> Option<Strategy> {
    measurements
        .iter()
        .min_by_key(|(strategy, median)| (*median, strategy_order(*strategy)))
        .map(|(strategy, _)| *strategy)
}

fn strategy_order(strategy: Strategy) -> u8 {
    match strategy {
        Strategy::Baseline => 0,
        Strategy::ThinLto => 1,
        Strategy::FatLtoCgu1 => 2,
        Strategy::Pgo => 3,
    }
}

pub(crate) fn selected_candidate(measurements: &[(Strategy, u64)]) -> Option<Strategy> {
    measurements
        .iter()
        .filter(|(strategy, _)| *strategy != Strategy::Baseline)
        .min_by_key(|(strategy, median)| (*median, strategy_order(*strategy)))
        .map(|(strategy, _)| *strategy)
}

#[cfg(test)]
mod tests {
    use super::{
        BuildPlan, BuildStage, ConfigSourceRecord, PgoParityRecord, PgoPhaseInputs, Strategy,
        ToolIdentity, effective_target_rustflags, lowest_median_strategy, selected_candidate,
    };
    use crate::cargo::BuildEnvironmentOverride;
    use std::fs;

    #[test]
    fn fixed_strategies_have_canonical_overrides_and_directories() {
        let root = std::path::Path::new("/run/target");
        let baseline = BuildPlan::baseline(root);
        assert_eq!(baseline.target_directory, root.join("baseline"));
        assert!(baseline.cargo_config_overrides.is_empty());
        assert!(matches!(baseline.stage, BuildStage::Baseline));

        let thin = BuildPlan::candidate(Strategy::ThinLto, root);
        assert_eq!(thin.target_directory, root.join("thin-lto"));
        assert_eq!(
            thin.cargo_config_overrides,
            ["profile.release.lto=\"thin\""]
        );

        let fat = BuildPlan::candidate(Strategy::FatLtoCgu1, root);
        assert_eq!(fat.target_directory, root.join("fat-lto-cgu1"));
        assert_eq!(
            fat.cargo_config_overrides,
            [
                "profile.release.lto=\"fat\"",
                "profile.release.codegen-units=1"
            ]
        );

        let confirmation = BuildPlan::confirmation(
            Strategy::FatLtoCgu1,
            root,
            fat.cargo_config_overrides.clone(),
            Vec::new(),
        );
        assert_eq!(
            confirmation.target_directory,
            root.join("confirmation/fat-lto-cgu1")
        );
        assert!(matches!(confirmation.stage, BuildStage::Confirmation));
        assert_eq!(
            confirmation.cargo_config_overrides,
            fat.cargo_config_overrides
        );
    }

    #[test]
    fn selection_is_deterministic_and_baseline_can_seed_pgo() {
        let measurements = [
            (Strategy::ThinLto, 110),
            (Strategy::Baseline, 100),
            (Strategy::FatLtoCgu1, 105),
        ];
        assert_eq!(
            lowest_median_strategy(&measurements),
            Some(Strategy::Baseline)
        );
        assert_eq!(
            selected_candidate(&measurements),
            Some(Strategy::FatLtoCgu1)
        );
    }

    #[test]
    fn preserves_exact_target_flags_and_rejects_build_flags() {
        let fixture = tempfile::tempdir().expect("fixture");
        let cargo = fixture.path().join(".cargo");
        fs::create_dir(&cargo).expect("cargo directory");
        fs::write(
            cargo.join("config.toml"),
            "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"-C\", \"target-cpu=native\"]\n",
        )
        .expect("target config");
        let (flags, sources) =
            effective_target_rustflags(fixture.path()).expect("compatible target flags");
        assert_eq!(flags, ["-C", "target-cpu=native"]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, cargo.join("config.toml"));
        assert_eq!(sources[0].sha256.len(), 64);

        fs::write(
            cargo.join("config.toml"),
            "[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\nrustc = \"/other/rustc\"\n",
        )
        .expect("build config");
        let error = effective_target_rustflags(fixture.path())
            .expect_err("build rustflags must fail closed");
        assert_eq!(error.reason, "unproven_build_rustflags");
        assert!(error.message.contains("effective build.rustflags"));

        fs::write(
            cargo.join("config.toml"),
            "[build]\nrustc = \"/other/rustc\"\n",
        )
        .expect("compiler config");
        let error = effective_target_rustflags(fixture.path())
            .expect_err("compiler override must fail closed");
        assert_eq!(error.reason, "unproven_compiler_override");
        assert!(error.message.contains("effective build.rustc"));
    }

    #[test]
    fn pgo_owns_one_ordered_environment_channel() {
        let root = std::path::Path::new("/run/target");
        let project = ["--cfg".to_owned(), "temper_project".to_owned()];
        let phase = ["-Cprofile-generate=/run/profiles".to_owned()];
        let plan = BuildPlan::pgo_instrumentation(Strategy::ThinLto, root, &project, &phase);
        assert_eq!(
            plan.cargo_config_overrides,
            ["profile.release.lto=\"thin\""]
        );
        assert_eq!(plan.environment_overrides.len(), 1);
        assert_eq!(
            plan.environment_overrides[0].name,
            "CARGO_ENCODED_RUSTFLAGS"
        );
        assert_eq!(
            plan.environment_overrides[0].arguments,
            [
                "--cfg",
                "temper_project",
                "-Cprofile-generate=/run/profiles"
            ]
        );
    }

    #[test]
    fn string_rustflags_with_ambiguous_boundaries_are_rejected_stably() {
        let fixture = tempfile::tempdir().expect("fixture");
        let cargo = fixture.path().join(".cargo");
        fs::create_dir(&cargo).expect("cargo directory");
        fs::write(
            cargo.join("config.toml"),
            "[target.x86_64-unknown-linux-gnu]\nrustflags = '--cfg \"temper label\"'\n",
        )
        .expect("target config");
        let error = effective_target_rustflags(fixture.path())
            .expect_err("quoted string rustflags must fail closed");
        assert_eq!(error.reason, "unsupported_string_rustflags_boundaries");
    }

    #[test]
    fn cfg_and_multiple_target_sources_remain_fail_closed() {
        let fixture = tempfile::tempdir().expect("fixture");
        let workspace = fixture.path().join("workspace");
        fs::create_dir_all(workspace.join(".cargo")).expect("workspace Cargo directory");
        fs::write(
            workspace.join(".cargo/config.toml"),
            "[target.'cfg(unix)']\nrustflags = [\"--cfg\", \"temper_cfg\"]\n",
        )
        .expect("cfg target config");
        let error = effective_target_rustflags(&workspace)
            .expect_err("cfg-selected rustflags must fail closed");
        assert_eq!(error.reason, "unproven_cfg_rustflags");

        fs::create_dir_all(fixture.path().join(".cargo")).expect("parent Cargo directory");
        fs::write(
            fixture.path().join(".cargo/config.toml"),
            "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", \"parent\"]\n",
        )
        .expect("parent target config");
        fs::write(
            workspace.join(".cargo/config.toml"),
            "[target.x86_64-unknown-linux-gnu]\nrustflags = [\"--cfg\", \"workspace\"]\n",
        )
        .expect("workspace target config");
        let error = effective_target_rustflags(&workspace)
            .expect_err("multiple target rustflags must fail closed");
        assert_eq!(error.reason, "ambiguous_target_rustflags");
    }

    #[test]
    fn parity_allows_only_phase_differences_and_names_every_other_field() {
        let instrumentation = parity_inputs("/run/instrumented", "-Cprofile-generate=/run/raw");
        let optimization = parity_inputs("/run/optimized", "-Cprofile-use=/run/merged.profdata");
        let parity = PgoParityRecord::compare(instrumentation.clone(), optimization.clone());
        assert!(parity.matched);
        assert!(parity.unexpected_differences.is_empty());

        let mut changed = optimization;
        changed.package_id = "changed-package".to_owned();
        changed.binary_target = "changed-binary".to_owned();
        changed.target_triple = "changed-target";
        changed.cargo_arguments.push("--changed".to_owned());
        changed
            .base_profile_overrides
            .push("profile.release.debug=1".to_owned());
        changed
            .project_rustflags
            .push("--changed-project-flag".to_owned());
        changed.config_sources[0].sha256 = "changed-config-hash".to_owned();
        changed.cargo_identity = "changed-cargo".to_owned();
        changed.rustc_identity = "changed-rustc".to_owned();
        changed.llvm_profdata_identity.llvm_profdata_sha256 = "changed-profdata".to_owned();
        changed.environment_overrides.clear();
        let parity = PgoParityRecord::compare(instrumentation, changed);
        assert_eq!(
            parity.unexpected_differences,
            [
                "package_id",
                "binary_target",
                "target_triple",
                "cargo_arguments",
                "base_profile_overrides",
                "project_rustflags",
                "config_sources",
                "cargo_identity",
                "rustc_identity",
                "llvm_profdata_identity",
                "pgo_environment_channel",
            ]
        );
    }

    fn parity_inputs(target_directory: &str, phase_flag: &str) -> PgoPhaseInputs {
        let project_rustflags = vec!["--cfg".to_owned(), "temper_project".to_owned()];
        let tool_identity = ToolIdentity {
            cargo_version: "cargo identity".to_owned(),
            rustc_version: "rustc identity".to_owned(),
            llvm_profdata_path: "/toolchain/llvm-profdata".into(),
            llvm_profdata_sha256: "profdata hash".to_owned(),
            llvm_profdata_version: "llvm-profdata identity".to_owned(),
        };
        PgoPhaseInputs {
            package_id: "package 0.1.0 (path+file:///workspace)".to_owned(),
            binary_target: "binary".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu",
            cargo_arguments: vec![
                "build".to_owned(),
                "--target-dir".to_owned(),
                target_directory.to_owned(),
            ],
            base_profile_overrides: vec!["profile.release.lto=\"thin\"".to_owned()],
            project_rustflags: project_rustflags.clone(),
            config_sources: vec![ConfigSourceRecord {
                path: "/workspace/.cargo/config.toml".into(),
                sha256: "config hash".to_owned(),
            }],
            cargo_identity: tool_identity.cargo_version.clone(),
            rustc_identity: tool_identity.rustc_version.clone(),
            llvm_profdata_identity: tool_identity,
            target_directory: target_directory.into(),
            environment_overrides: vec![BuildEnvironmentOverride::cargo_encoded_rustflags(
                project_rustflags
                    .into_iter()
                    .chain([phase_flag.to_owned()])
                    .collect(),
            )],
        }
    }
}
