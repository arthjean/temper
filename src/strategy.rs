use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use serde::Serialize;

use crate::cargo::{self, BuildFailure, BuildRecord, TargetSelection};
use crate::hash::sha256_file;
use crate::interposition::{
    self, Injection, InterpositionPlan, InterpositionRecord, MISSING_FUNCTION_FLAG,
    PROFILE_GENERATE_FLAG, PROFILE_USE_FLAG, ShimStage, ShimTools,
};
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
    PgoReference,
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
    pub(crate) interposition: Option<InterpositionPlan>,
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
            interposition: None,
        }
    }

    pub(crate) fn confirmation(
        strategy: Strategy,
        target_root: &Path,
        cargo_config_overrides: Vec<String>,
        interposition: Option<InterpositionPlan>,
    ) -> Self {
        let target_directory = match &interposition {
            Some(plan) => plan.target_directory.clone(),
            None => target_root
                .join("confirmation")
                .join(strategy.canonical_identity()),
        };
        Self {
            strategy,
            stage: BuildStage::Confirmation,
            target_directory,
            cargo_config_overrides,
            interposition,
        }
    }

    /// Interposed PGO stage. Cargo keeps ownership of every compiler input;
    /// the shim appends the phase controls after Cargo has resolved them.
    fn pgo(base_strategy: Strategy, stage: BuildStage, interposition: InterpositionPlan) -> Self {
        Self {
            strategy: Strategy::Pgo,
            stage,
            target_directory: interposition.target_directory.clone(),
            cargo_config_overrides: base_strategy.profile_overrides(),
            interposition: Some(interposition),
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
    real_rustc: PathBuf,
    shim_executable: PathBuf,
    shim_protocol: &'static str,
    inspected_config_sources: Vec<ConfigSourceRecord>,
    tool_identity: ToolIdentity,
}

impl PgoPrerequisites {
    fn tools(&self) -> ShimTools {
        ShimTools {
            shim_executable: self.shim_executable.clone(),
            real_rustc: self.real_rustc.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct PgoPhaseInputs {
    package_id: String,
    binary_target: String,
    target_triple: &'static str,
    stage: ShimStage,
    cargo_arguments: Vec<String>,
    base_profile_overrides: Vec<String>,
    config_sources: Vec<ConfigSourceRecord>,
    cargo_identity: String,
    rustc_identity: String,
    llvm_profdata_identity: ToolIdentity,
    real_rustc: PathBuf,
    shim_executable: PathBuf,
    injected_flags: Vec<&'static str>,
    target_directory: PathBuf,
    capture_directory: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PgoParityRecord {
    permitted_differences: [&'static str; 5],
    reference: Option<PgoPhaseInputs>,
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
    Reference,
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
    reference_build: Option<BuildRecord>,
    reference_failure: Option<BuildFailure>,
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
    let mut state = TrainingState::new(base_strategy);
    let prerequisites = match prove_pgo_prerequisites(&selection.workspace_root) {
        Ok(prerequisites) => prerequisites,
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Prerequisites,
                failure.reason,
                failure.message,
                failure.remediation,
            ));
        }
    };
    let tools = prerequisites.tools();
    let llvm_profdata_path = prerequisites.llvm_profdata_path.clone();
    let reference_prerequisites = prerequisites.clone();
    state.prerequisites = Some(prerequisites);

    // A fresh pass-through reference stage observes the compiler inputs Cargo
    // resolves with no PGO control at all. It is the boundary the instrumented
    // and optimized stages are compared against.
    let reference_plan = match interposition::plan_stage(
        &tools,
        run_directory,
        target_root,
        ShimStage::Reference,
        Injection::None,
    ) {
        Ok(plan) => BuildPlan::pgo(base_strategy, BuildStage::PgoReference, plan),
        Err(message) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Reference,
                "pgo_stage_isolation_failed",
                message,
                None,
            ));
        }
    };
    state.phase_parity.reference = Some(phase_inputs(
        selection,
        &reference_plan,
        &reference_prerequisites,
    ));
    match cargo::build(selection, &reference_plan) {
        Ok(build) => state.reference_build = Some(build),
        Err(failure) => {
            let message = failure.message.clone();
            let reason = pgo_build_rejection(&failure, "pgo_reference_failed");
            state.reference_failure = Some(failure);
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Reference,
                reason,
                message,
                None,
            ));
        }
    }

    let raw_profile_directory = match prepare_profile_directory(run_directory) {
        Ok(directory) => directory,
        Err(message) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Instrumentation,
                "pgo_profile_directory_failed",
                message,
                None,
            ));
        }
    };
    let instrumentation_plan = match interposition::plan_stage(
        &tools,
        run_directory,
        target_root,
        ShimStage::Generate,
        Injection::Generate(raw_profile_directory.clone()),
    ) {
        Ok(plan) => BuildPlan::pgo(base_strategy, BuildStage::PgoInstrumentation, plan),
        Err(message) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Instrumentation,
                "pgo_stage_isolation_failed",
                message,
                None,
            ));
        }
    };
    state.phase_parity.instrumentation = Some(phase_inputs(
        selection,
        &instrumentation_plan,
        &reference_prerequisites,
    ));
    let instrumentation_build = match cargo::build(selection, &instrumentation_plan) {
        Ok(build) => build,
        Err(failure) => {
            let message = failure.message.clone();
            let reason = pgo_build_rejection(&failure, "pgo_instrumentation_failed");
            state.instrumentation_failure = Some(failure);
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Instrumentation,
                reason,
                message,
                None,
            ));
        }
    };
    let instrumented_executable = instrumentation_build.executable_path.clone();
    state.instrumentation_build = Some(instrumentation_build);

    let profile_pattern = raw_profile_directory.join("temper-%m-%p.profraw");
    let training = match workload.invoke_with_environment(
        &instrumented_executable,
        &[("LLVM_PROFILE_FILE", profile_pattern.as_os_str())],
    ) {
        Ok(training) => training,
        Err(failure) if failure.kind == WorkloadFailureKind::Interrupted => {
            let record = state.reject(
                PgoFailureStage::Training,
                "pgo_training_interrupted",
                failure.message.clone(),
                None,
            );
            return PgoTrainingOutcome::Interrupted(record, failure);
        }
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Training,
                "pgo_training_failed",
                failure.message,
                None,
            ));
        }
    };
    state.training_duration_ns = Some(training.duration_ns);

    match collect_profile_files(&raw_profile_directory) {
        Ok(files) if !files.is_empty() => state.raw_profile_files = files,
        Ok(_) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::ProfileDiscovery,
                "pgo_training_produced_no_profraw_files",
                "PGO training produced no raw profile files.",
                None,
            ));
        }
        Err(message) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::ProfileDiscovery,
                "pgo_profile_discovery_failed",
                message,
                None,
            ));
        }
    }

    let profdata_path = run_directory.join("pgo").join("merged.profdata");
    let merge = merge_profiles(
        &llvm_profdata_path,
        &state.raw_profile_files,
        &profdata_path,
        &run_directory.join("pgo"),
    );
    let merge_rejected = matches!(merge.outcome, ProfileMergeOutcome::Rejected);
    let merge_message = merge.message.clone();
    state.merge = Some(merge);
    if merge_rejected {
        return PgoTrainingOutcome::Rejected(
            state.reject(
                PgoFailureStage::ProfileMerge,
                "pgo_profile_merge_failed",
                merge_message
                    .unwrap_or_else(|| "llvm-profdata merge rejected the profile data.".to_owned()),
                None,
            ),
        );
    }
    let merged_profile = match fs::canonicalize(&profdata_path) {
        Ok(profile) => profile,
        Err(error) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::ProfileMerge,
                "pgo_merged_profile_unavailable",
                format!("Merged PGO profile could not be canonicalized: {error}"),
                None,
            ));
        }
    };

    // The optimized stage re-proves its own prerequisites so a compiler,
    // wrapper or config source that changed during training is observed.
    let optimization_prerequisites = match prove_pgo_prerequisites(&selection.workspace_root) {
        Ok(prerequisites) => prerequisites,
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Prerequisites,
                failure.reason,
                failure.message,
                failure.remediation,
            ));
        }
    };
    let optimized_plan = match interposition::plan_stage(
        &optimization_prerequisites.tools(),
        run_directory,
        target_root,
        ShimStage::Use,
        Injection::Use(merged_profile),
    ) {
        Ok(plan) => BuildPlan::pgo(base_strategy, BuildStage::PgoOptimized, plan),
        Err(message) => {
            return PgoTrainingOutcome::Rejected(state.reject(
                PgoFailureStage::Prerequisites,
                "pgo_stage_isolation_failed",
                message,
                None,
            ));
        }
    };
    state.phase_parity.optimization = Some(phase_inputs(
        selection,
        &optimized_plan,
        &optimization_prerequisites,
    ));
    state.phase_parity.decide();
    if !state.phase_parity.matched {
        return PgoTrainingOutcome::Rejected(state.reject(
            PgoFailureStage::Prerequisites,
            "pgo_phase_parity_mismatch",
            "PGO optimization inputs differed from the instrumentation inputs outside the explicit allowlist.",
            None,
        ));
    }
    PgoTrainingOutcome::Trained(PgoTrainingSuccess {
        record: state.trained(),
        optimized_plan,
    })
}

/// Accumulates every durable PGO training fact so each rejection persists the
/// evidence gathered so far instead of rebuilding a record at every exit.
struct TrainingState {
    base_strategy: Strategy,
    prerequisites: Option<PgoPrerequisites>,
    reference_build: Option<BuildRecord>,
    reference_failure: Option<BuildFailure>,
    instrumentation_build: Option<BuildRecord>,
    instrumentation_failure: Option<BuildFailure>,
    training_duration_ns: Option<u64>,
    raw_profile_files: Vec<ProfileFileRecord>,
    merge: Option<ProfileMergeRecord>,
    phase_parity: PgoParityRecord,
}

impl TrainingState {
    fn new(base_strategy: Strategy) -> Self {
        Self {
            base_strategy,
            prerequisites: None,
            reference_build: None,
            reference_failure: None,
            instrumentation_build: None,
            instrumentation_failure: None,
            training_duration_ns: None,
            raw_profile_files: Vec::new(),
            merge: None,
            phase_parity: PgoParityRecord::pending(),
        }
    }

    fn reject(
        mut self,
        stage: PgoFailureStage,
        reason: impl Into<String>,
        message: impl Into<String>,
        remediation: Option<&'static str>,
    ) -> PgoTrainingRecord {
        self.phase_parity.decide();
        PgoTrainingRecord {
            outcome: PgoTrainingOutcomeKind::Rejected,
            base_strategy: self.base_strategy,
            prerequisites: self.prerequisites,
            reference_build: self.reference_build,
            reference_failure: self.reference_failure,
            instrumentation_build: self.instrumentation_build,
            instrumentation_failure: self.instrumentation_failure,
            training_duration_ns: self.training_duration_ns,
            raw_profile_files: self.raw_profile_files,
            merge: self.merge,
            phase_parity: self.phase_parity,
            failure_stage: Some(stage),
            rejection_reason: Some(reason.into()),
            message: Some(message.into()),
            remediation,
        }
    }

    fn trained(mut self) -> PgoTrainingRecord {
        self.phase_parity.decide();
        PgoTrainingRecord {
            outcome: PgoTrainingOutcomeKind::Trained,
            base_strategy: self.base_strategy,
            prerequisites: self.prerequisites,
            reference_build: self.reference_build,
            reference_failure: self.reference_failure,
            instrumentation_build: self.instrumentation_build,
            instrumentation_failure: self.instrumentation_failure,
            training_duration_ns: self.training_duration_ns,
            raw_profile_files: self.raw_profile_files,
            merge: self.merge,
            phase_parity: self.phase_parity,
            failure_stage: None,
            rejection_reason: None,
            message: None,
            remediation: None,
        }
    }
}

fn pgo_build_rejection(failure: &BuildFailure, fallback: &'static str) -> &'static str {
    if interposition::COMPILER_INPUT_REASONS.contains(&failure.reason) {
        failure.reason
    } else {
        fallback
    }
}

fn prepare_profile_directory(run_directory: &Path) -> Result<PathBuf, String> {
    let raw_profile_directory = run_directory.join("pgo").join("raw");
    fs::create_dir_all(&raw_profile_directory).map_err(|error| {
        format!(
            "Could not create the run-scoped PGO profile directory {}: {error}",
            raw_profile_directory.display()
        )
    })?;
    fs::canonicalize(&raw_profile_directory)
        .map_err(|error| format!("Could not canonicalize the PGO profile directory: {error}"))
}

impl PgoParityRecord {
    const PERMITTED_DIFFERENCES: [&'static str; 5] = [
        "interposition_stage",
        "target_directory",
        "capture_directory",
        "profile_generate_vs_use",
        "pgo_warn_missing_function_in_use",
    ];

    fn pending() -> Self {
        Self {
            permitted_differences: Self::PERMITTED_DIFFERENCES,
            reference: None,
            instrumentation: None,
            optimization: None,
            matched: false,
            unexpected_differences: Vec::new(),
        }
    }

    /// Compares the projected Temper-side inputs of the two PGO phases.
    /// Observed compiler evidence is compared separately once every stage has
    /// executed.
    fn decide(&mut self) {
        self.unexpected_differences.clear();
        match (&self.instrumentation, &self.optimization) {
            (None, _) => self
                .unexpected_differences
                .push("instrumentation_inputs_unavailable".to_owned()),
            (Some(_), None) => self
                .unexpected_differences
                .push("optimization_inputs_unavailable".to_owned()),
            (Some(instrumentation), Some(optimization)) => {
                compare_phase_inputs(
                    instrumentation,
                    optimization,
                    &mut self.unexpected_differences,
                );
            }
        }
        self.matched = self.unexpected_differences.is_empty();
    }
}

fn compare_phase_inputs(
    instrumentation: &PgoPhaseInputs,
    optimization: &PgoPhaseInputs,
    differences: &mut Vec<String>,
) {
    compare_field(
        "package_id",
        &instrumentation.package_id,
        &optimization.package_id,
        differences,
    );
    compare_field(
        "binary_target",
        &instrumentation.binary_target,
        &optimization.binary_target,
        differences,
    );
    compare_field(
        "target_triple",
        &instrumentation.target_triple,
        &optimization.target_triple,
        differences,
    );
    compare_field(
        "cargo_arguments",
        &normalized_cargo_arguments(&instrumentation.cargo_arguments),
        &normalized_cargo_arguments(&optimization.cargo_arguments),
        differences,
    );
    compare_field(
        "base_profile_overrides",
        &instrumentation.base_profile_overrides,
        &optimization.base_profile_overrides,
        differences,
    );
    compare_field(
        "config_sources",
        &instrumentation.config_sources,
        &optimization.config_sources,
        differences,
    );
    compare_field(
        "cargo_identity",
        &instrumentation.cargo_identity,
        &optimization.cargo_identity,
        differences,
    );
    compare_field(
        "rustc_identity",
        &instrumentation.rustc_identity,
        &optimization.rustc_identity,
        differences,
    );
    compare_field(
        "llvm_profdata_identity",
        &instrumentation.llvm_profdata_identity,
        &optimization.llvm_profdata_identity,
        differences,
    );
    compare_field(
        "real_rustc",
        &instrumentation.real_rustc,
        &optimization.real_rustc,
        differences,
    );
    compare_field(
        "shim_executable",
        &instrumentation.shim_executable,
        &optimization.shim_executable,
        differences,
    );
    if instrumentation.injected_flags != [PROFILE_GENERATE_FLAG]
        || optimization.injected_flags != [PROFILE_USE_FLAG, MISSING_FUNCTION_FLAG]
    {
        differences.push("pgo_injection_channel".to_owned());
    }
}

fn phase_inputs(
    selection: &TargetSelection,
    plan: &BuildPlan,
    prerequisites: &PgoPrerequisites,
) -> PgoPhaseInputs {
    let invocation = cargo::planned_invocation(selection, plan);
    let interposition: Option<InterpositionRecord> = invocation.interposition;
    PgoPhaseInputs {
        package_id: selection.package_id.clone(),
        binary_target: selection.binary_name.clone(),
        target_triple: SUPPORTED_HOST,
        stage: interposition
            .as_ref()
            .map_or(ShimStage::Reference, |record| record.stage),
        cargo_arguments: invocation.cargo_arguments,
        base_profile_overrides: invocation.cargo_config_overrides,
        config_sources: prerequisites.inspected_config_sources.clone(),
        cargo_identity: prerequisites.tool_identity.cargo_version.clone(),
        rustc_identity: prerequisites.tool_identity.rustc_version.clone(),
        llvm_profdata_identity: prerequisites.tool_identity.clone(),
        real_rustc: prerequisites.real_rustc.clone(),
        shim_executable: prerequisites.shim_executable.clone(),
        injected_flags: interposition
            .as_ref()
            .map(|record| record.injected_flags.clone())
            .unwrap_or_default(),
        target_directory: invocation.target_directory,
        capture_directory: interposition
            .as_ref()
            .map(|record| record.capture_directory.clone())
            .unwrap_or_default(),
    }
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

/// Proves the PGO boundary before any compiler interposition starts.
///
/// v0.0.3 no longer reconstructs project rustflags: Cargo resolves every
/// compiler input and the shim appends the phase controls after it. What must
/// still be proven is that no compiler wrapper or replacement compiler sits
/// between Cargo and the real rustc, because an outer cache can key an artifact
/// before the phase control exists.
fn prove_pgo_prerequisites(
    workspace_root: &Path,
) -> Result<PgoPrerequisites, PgoPrerequisiteFailure> {
    for variable in [
        "CARGO_BUILD_RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        if std::env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            return Err(PgoPrerequisiteFailure {
                reason: "ambient_compiler_override",
                message: format!(
                    "PGO was rejected because {variable} is set and Temper cannot prove that an outer compiler cache keys its PGO phase controls."
                ),
                remediation: None,
            });
        }
    }

    let inspected_config_sources = inspect_config_sources(workspace_root)?;
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
    // The real compiler is resolved and identified before any process-local
    // `RUSTC` override, so the shim can never resolve itself.
    let tools =
        interposition::resolve_tools(&rustc_version).map_err(|message| PgoPrerequisiteFailure {
            reason: "compiler_interposition_unavailable",
            message,
            remediation: None,
        })?;

    Ok(PgoPrerequisites {
        llvm_profdata_path: llvm_profdata_path.clone(),
        target_libdir,
        real_rustc: tools.real_rustc,
        shim_executable: tools.shim_executable,
        shim_protocol: interposition::PROTOCOL,
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

/// Records the directly discovered Cargo config sources and rejects a compiler
/// override declared in any of them. Temper never merges config values; Cargo
/// keeps ownership of effective rustflags. Recursive `include` provenance
/// arrives with the schema-3 source graph.
fn inspect_config_sources(
    workspace_root: &Path,
) -> Result<Vec<ConfigSourceRecord>, PgoPrerequisiteFailure> {
    let paths = cargo_config_paths(workspace_root).map_err(|message| PgoPrerequisiteFailure {
        reason: "cargo_config_inspection_failed",
        message,
        remediation: None,
    })?;
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
        let Some(build) = config.get("build") else {
            continue;
        };
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
    Ok(inspected_sources)
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
        ToolIdentity, inspect_config_sources, lowest_median_strategy, selected_candidate,
    };
    use crate::interposition::{
        Injection, InterpositionPlan, MISSING_FUNCTION_FLAG, PROFILE_GENERATE_FLAG,
        PROFILE_USE_FLAG, ShimStage,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn fixed_strategies_have_canonical_overrides_and_directories() {
        let root = Path::new("/run/target");
        let baseline = BuildPlan::baseline(root);
        assert_eq!(baseline.target_directory, root.join("baseline"));
        assert!(baseline.cargo_config_overrides.is_empty());
        assert!(matches!(baseline.stage, BuildStage::Baseline));
        assert!(baseline.interposition.is_none());

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
            None,
        );
        assert_eq!(
            confirmation.target_directory,
            root.join("confirmation/fat-lto-cgu1")
        );
        assert!(matches!(confirmation.stage, BuildStage::Confirmation));
        assert!(confirmation.interposition.is_none());
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
    fn config_inspection_records_sources_without_reconstructing_rustflags() {
        let fixture = tempfile::tempdir().expect("fixture");
        let cargo = fixture.path().join(".cargo");
        fs::create_dir(&cargo).expect("cargo directory");
        // Every rustflags shape v0.0.2 rejected now stays with Cargo.
        fs::write(
            cargo.join("config.toml"),
            "[build]\nrustflags = [\"--cfg\", \"build_flag\"]\n\n[target.x86_64-unknown-linux-gnu]\nrustflags = '--cfg \"quoted label\"'\n\n[target.'cfg(unix)']\nrustflags = [\"--cfg\", \"cfg_flag\"]\n",
        )
        .expect("rustflags config");
        let sources = inspect_config_sources(fixture.path()).expect("rustflags stay with Cargo");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, cargo.join("config.toml"));
        assert_eq!(sources[0].sha256.len(), 64);
    }

    #[test]
    fn config_inspection_rejects_every_declared_compiler_override() {
        let fixture = tempfile::tempdir().expect("fixture");
        let cargo = fixture.path().join(".cargo");
        fs::create_dir(&cargo).expect("cargo directory");
        for key in ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"] {
            fs::write(
                cargo.join("config.toml"),
                format!("[build]\n{key} = \"/other/compiler\"\n"),
            )
            .expect("compiler override config");
            let error =
                inspect_config_sources(fixture.path()).expect_err("compiler override fails closed");
            assert_eq!(error.reason, "unproven_compiler_override");
            assert!(error.message.contains(&format!("build.{key}")));
        }
    }

    #[test]
    fn interposed_pgo_plans_own_no_rustflags_channel() {
        let plan = BuildPlan::pgo(
            Strategy::ThinLto,
            BuildStage::PgoInstrumentation,
            interposition_plan(
                ShimStage::Generate,
                Injection::Generate(PathBuf::from("/run/pgo/raw")),
            ),
        );
        assert_eq!(
            plan.cargo_config_overrides,
            ["profile.release.lto=\"thin\""]
        );
        let interposition = plan.interposition.expect("interposed stage");
        assert_eq!(plan.target_directory, interposition.target_directory);
        assert_eq!(
            interposition.record().injected_flags,
            [PROFILE_GENERATE_FLAG]
        );
    }

    #[test]
    fn parity_allows_only_phase_differences_and_names_every_other_field() {
        let instrumentation = parity_inputs(ShimStage::Generate);
        let optimization = parity_inputs(ShimStage::Use);
        let mut parity = parity_record(Some(instrumentation.clone()), Some(optimization.clone()));
        parity.decide();
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
        changed.config_sources[0].sha256 = "changed-config-hash".to_owned();
        changed.cargo_identity = "changed-cargo".to_owned();
        changed.rustc_identity = "changed-rustc".to_owned();
        changed.llvm_profdata_identity.llvm_profdata_sha256 = "changed-profdata".to_owned();
        changed.real_rustc = PathBuf::from("/other/rustc");
        changed.shim_executable = PathBuf::from("/other/cargo-temper");
        changed.injected_flags = vec![PROFILE_GENERATE_FLAG];
        let mut parity = parity_record(Some(instrumentation), Some(changed));
        parity.decide();
        assert_eq!(
            parity.unexpected_differences,
            [
                "package_id",
                "binary_target",
                "target_triple",
                "cargo_arguments",
                "base_profile_overrides",
                "config_sources",
                "cargo_identity",
                "rustc_identity",
                "llvm_profdata_identity",
                "real_rustc",
                "shim_executable",
                "pgo_injection_channel",
            ]
        );
    }

    #[test]
    fn parity_can_never_match_without_both_phases() {
        let mut empty = parity_record(None, None);
        empty.decide();
        assert!(!empty.matched);
        assert_eq!(
            empty.unexpected_differences,
            ["instrumentation_inputs_unavailable"]
        );

        let mut instrumentation_only =
            parity_record(Some(parity_inputs(ShimStage::Generate)), None);
        instrumentation_only.decide();
        assert!(!instrumentation_only.matched);
        assert_eq!(
            instrumentation_only.unexpected_differences,
            ["optimization_inputs_unavailable"]
        );
    }

    fn parity_record(
        instrumentation: Option<PgoPhaseInputs>,
        optimization: Option<PgoPhaseInputs>,
    ) -> PgoParityRecord {
        let mut record = PgoParityRecord::pending();
        record.instrumentation = instrumentation;
        record.optimization = optimization;
        record
    }

    fn interposition_plan(stage: ShimStage, injection: Injection) -> InterpositionPlan {
        InterpositionPlan {
            stage,
            shim_executable: PathBuf::from("/toolchain/cargo-temper"),
            real_rustc: PathBuf::from("/toolchain/rustc"),
            run_root: PathBuf::from("/run"),
            target_directory: PathBuf::from("/run/target").join(stage.label()),
            capture_directory: PathBuf::from("/run/captures").join(stage.label()),
            injection,
        }
    }

    fn parity_inputs(stage: ShimStage) -> PgoPhaseInputs {
        let injection = match stage {
            ShimStage::Generate => Injection::Generate(PathBuf::from("/run/pgo/raw")),
            _ => Injection::Use(PathBuf::from("/run/pgo/merged.profdata")),
        };
        let plan = interposition_plan(stage, injection);
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
            stage,
            cargo_arguments: vec![
                "build".to_owned(),
                "--target-dir".to_owned(),
                plan.target_directory.to_string_lossy().into_owned(),
            ],
            base_profile_overrides: vec!["profile.release.lto=\"thin\"".to_owned()],
            config_sources: vec![ConfigSourceRecord {
                path: "/workspace/.cargo/config.toml".into(),
                sha256: "config hash".to_owned(),
            }],
            cargo_identity: tool_identity.cargo_version.clone(),
            rustc_identity: tool_identity.rustc_version.clone(),
            llvm_profdata_identity: tool_identity,
            real_rustc: plan.real_rustc.clone(),
            shim_executable: plan.shim_executable.clone(),
            injected_flags: match stage {
                ShimStage::Generate => vec![PROFILE_GENERATE_FLAG],
                _ => vec![PROFILE_USE_FLAG, MISSING_FUNCTION_FLAG],
            },
            target_directory: plan.target_directory.clone(),
            capture_directory: plan.capture_directory.clone(),
        }
    }
}
