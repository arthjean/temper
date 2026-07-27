use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::cargo::{self, BuildFailure, BuildRecord, TargetSelection};
use crate::hash::sha256_file;
use crate::preflight::{SUPPORTED_HOST, rustc_program};
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

#[derive(Debug)]
pub(crate) struct BuildPlan {
    pub(crate) strategy: Strategy,
    pub(crate) stage: BuildStage,
    pub(crate) target_directory: PathBuf,
    pub(crate) cargo_config_overrides: Vec<String>,
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
        }
    }

    pub(crate) fn confirmation(
        strategy: Strategy,
        target_root: &Path,
        cargo_config_overrides: Vec<String>,
    ) -> Self {
        Self {
            strategy,
            stage: BuildStage::Confirmation,
            target_directory: target_root
                .join("confirmation")
                .join(strategy.canonical_identity()),
            cargo_config_overrides,
        }
    }

    fn pgo_instrumentation(
        base_strategy: Strategy,
        target_root: &Path,
        rustflags: &[String],
    ) -> Self {
        Self {
            strategy: Strategy::Pgo,
            stage: BuildStage::PgoInstrumentation,
            target_directory: target_root.join("pgo-instrumented"),
            cargo_config_overrides: pgo_overrides(base_strategy, rustflags),
        }
    }

    fn pgo_optimized(base_strategy: Strategy, target_root: &Path, rustflags: &[String]) -> Self {
        Self {
            strategy: Strategy::Pgo,
            stage: BuildStage::PgoOptimized,
            target_directory: target_root.join(Strategy::Pgo.canonical_identity()),
            cargo_config_overrides: pgo_overrides(base_strategy, rustflags),
        }
    }
}

fn pgo_overrides(base_strategy: Strategy, rustflags: &[String]) -> Vec<String> {
    let mut overrides = base_strategy.profile_overrides();
    let encoded_flags = serde_json::Value::Array(
        rustflags
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
    .to_string();
    overrides.push(format!("target.{SUPPORTED_HOST}.rustflags={encoded_flags}"));
    overrides
}

#[derive(Debug, Serialize)]
pub(crate) struct PgoPrerequisites {
    llvm_profdata_path: PathBuf,
    target_libdir: PathBuf,
    preserved_target_rustflags: Vec<String>,
    inspected_config_sources: Vec<PathBuf>,
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

#[derive(Debug, Serialize)]
pub(crate) struct ProfileMergeRecord {
    outcome: ProfileMergeOutcome,
    llvm_profdata_path: PathBuf,
    arguments: Vec<String>,
    profraw_files: Vec<PathBuf>,
    profdata_path: PathBuf,
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
    raw_profile_files: Vec<PathBuf>,
    merge: Option<ProfileMergeRecord>,
    failure_stage: Option<PgoFailureStage>,
    rejection_reason: Option<String>,
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
                failure.message,
                failure.remediation,
            ));
        }
    };
    let raw_profile_directory = run_directory.join("pgo").join("raw");
    if let Err(error) = fs::create_dir_all(&raw_profile_directory) {
        return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
            base_strategy,
            PgoFailureStage::Instrumentation,
            format!(
                "Could not create the run-scoped PGO profile directory {}: {error}",
                raw_profile_directory.display()
            ),
            None,
        ));
    }
    let raw_profile_directory = match fs::canonicalize(&raw_profile_directory) {
        Ok(directory) => directory,
        Err(error) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord::rejected(
                base_strategy,
                PgoFailureStage::Instrumentation,
                format!("Could not canonicalize the PGO profile directory: {error}"),
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
                message,
                None,
            ));
        }
    };
    let generate_rustflags = [generate_flag];
    let instrumentation_plan =
        BuildPlan::pgo_instrumentation(base_strategy, target_root, &generate_rustflags);
    let instrumentation_build = match cargo::build(selection, &instrumentation_plan) {
        Ok(build) => build,
        Err(failure) => {
            return PgoTrainingOutcome::Rejected(PgoTrainingRecord {
                outcome: PgoTrainingOutcomeKind::Rejected,
                base_strategy,
                prerequisites: Some(prerequisites),
                instrumentation_build: None,
                instrumentation_failure: Some(failure),
                training_duration_ns: None,
                raw_profile_files: Vec::new(),
                merge: None,
                failure_stage: Some(PgoFailureStage::Instrumentation),
                rejection_reason: Some("pgo_instrumentation_failed".to_owned()),
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
                failure_stage: Some(PgoFailureStage::Training),
                rejection_reason: Some("pgo_training_interrupted".to_owned()),
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
                failure_stage: Some(PgoFailureStage::Training),
                rejection_reason: Some(format!("pgo_training_failed: {}", failure.message)),
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
                failure_stage: Some(PgoFailureStage::ProfileDiscovery),
                rejection_reason: Some("pgo_training_produced_no_profraw_files".to_owned()),
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
                failure_stage: Some(PgoFailureStage::ProfileDiscovery),
                rejection_reason: Some(message),
                remediation: None,
            });
        }
    };
    let profdata_path = run_directory.join("pgo").join("merged.profdata");
    let merge = merge_profiles(
        &prerequisites.llvm_profdata_path,
        &raw_profile_files,
        &profdata_path,
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
            failure_stage: Some(PgoFailureStage::ProfileMerge),
            rejection_reason: Some(reason),
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
                failure_stage: Some(PgoFailureStage::ProfileMerge),
                rejection_reason: Some(format!(
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
                failure_stage: Some(PgoFailureStage::ProfileMerge),
                rejection_reason: Some(message),
                remediation: None,
            });
        }
    };
    let use_rustflags = [use_flag];
    let optimized_plan = BuildPlan::pgo_optimized(base_strategy, target_root, &use_rustflags);
    let record = PgoTrainingRecord {
        outcome: PgoTrainingOutcomeKind::Trained,
        base_strategy,
        prerequisites: Some(prerequisites),
        instrumentation_build: Some(instrumentation_build),
        instrumentation_failure: None,
        training_duration_ns: Some(training.duration_ns),
        raw_profile_files,
        merge: Some(merge),
        failure_stage: None,
        rejection_reason: None,
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
        reason: String,
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
            failure_stage: Some(stage),
            rejection_reason: Some(reason),
            remediation,
        }
    }
}

struct PgoPrerequisiteFailure {
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
                message: format!(
                    "PGO was rejected because {variable} is set and Temper cannot prove the active rustc toolchain."
                ),
                remediation: None,
            });
        }
    }

    let (preserved_target_rustflags, inspected_config_sources) =
        effective_target_rustflags(workspace_root).map_err(|message| PgoPrerequisiteFailure {
            message,
            remediation: None,
        })?;
    let target_libdir =
        rustc_target_libdir(workspace_root).map_err(|message| PgoPrerequisiteFailure {
            message,
            remediation: None,
        })?;
    let llvm_profdata_path = target_libdir
        .parent()
        .map(|target_root| target_root.join("bin").join("llvm-profdata"))
        .ok_or_else(|| PgoPrerequisiteFailure {
            message: "rustc target-libdir has no toolchain target parent directory.".to_owned(),
            remediation: Some(LLVM_TOOLS_HINT),
        })?;
    let metadata = fs::metadata(&llvm_profdata_path).map_err(|_| PgoPrerequisiteFailure {
        message: format!(
            "The active rustc has no adjacent executable llvm-profdata at {}.",
            llvm_profdata_path.display()
        ),
        remediation: Some(LLVM_TOOLS_HINT),
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(PgoPrerequisiteFailure {
            message: format!(
                "The llvm-profdata adjacent to the active rustc is not executable: {}.",
                llvm_profdata_path.display()
            ),
            remediation: Some(LLVM_TOOLS_HINT),
        });
    }
    let llvm_profdata_path =
        fs::canonicalize(&llvm_profdata_path).map_err(|error| PgoPrerequisiteFailure {
            message: format!("Could not canonicalize toolchain llvm-profdata: {error}"),
            remediation: Some(LLVM_TOOLS_HINT),
        })?;

    Ok(PgoPrerequisites {
        llvm_profdata_path,
        target_libdir,
        preserved_target_rustflags,
        inspected_config_sources,
    })
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
) -> Result<(Vec<String>, Vec<PathBuf>), String> {
    let paths = cargo_config_paths(workspace_root)?;
    let mut rustflags = Vec::new();
    let mut target_sources = 0_usize;
    for path in &paths {
        let contents = fs::read_to_string(path).map_err(|error| {
            format!("Cargo config {} could not be read: {error}", path.display())
        })?;
        let config: toml::Value = toml::from_str(&contents).map_err(|error| {
            format!(
                "Cargo config {} could not be parsed: {error}",
                path.display()
            )
        })?;
        if let Some(build) = config.get("build") {
            if build.get("rustflags").is_some() {
                return Err(format!(
                    "PGO was rejected because effective build.rustflags may come from {}.",
                    path.display()
                ));
            }
            for key in ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"] {
                if build.get(key).is_some() {
                    return Err(format!(
                        "PGO was rejected because effective build.{key} may come from {}.",
                        path.display()
                    ));
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
                    return Err(
                        "PGO was rejected because multiple target-specific rustflags sources make composition ambiguous."
                            .to_owned(),
                    );
                }
                rustflags = rustflags_value(value, path)?;
            } else if target.trim_start().starts_with("cfg(") {
                return Err(format!(
                    "PGO was rejected because cfg-selected target rustflags in {} cannot be proven composable.",
                    path.display()
                ));
            }
        }
    }
    Ok((rustflags, paths))
}

fn rustflags_value(value: &toml::Value, path: &Path) -> Result<Vec<String>, String> {
    if let Some(flags) = value.as_str() {
        return Ok(flags.split_ascii_whitespace().map(str::to_owned).collect());
    }
    let flags = value.as_array().ok_or_else(|| {
        format!(
            "Target rustflags in {} must be a string or an array of strings.",
            path.display()
        )
    })?;
    flags
        .iter()
        .map(|flag| {
            flag.as_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "Target rustflags in {} contain a non-string value.",
                    path.display()
                )
            })
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

fn collect_profile_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
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
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_symlink() {
                return Err("PGO profile discovery rejected a symbolic link.".to_owned());
            } else if file_type.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "profraw")
            {
                profiles.push(entry.path());
                if profiles.len() > PROFILE_FILE_LIMIT {
                    return Err(format!(
                        "PGO training produced more than {PROFILE_FILE_LIMIT} profile files."
                    ));
                }
            }
        }
    }
    profiles.sort();
    Ok(profiles)
}

fn merge_profiles(
    llvm_profdata: &Path,
    profiles: &[PathBuf],
    profdata_path: &Path,
) -> ProfileMergeRecord {
    let mut arguments: Vec<OsString> = vec!["merge".into(), "-o".into()];
    arguments.push(profdata_path.as_os_str().to_owned());
    arguments.extend(
        profiles
            .iter()
            .map(|profile| profile.as_os_str().to_owned()),
    );
    let serialized_arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let output = Command::new(llvm_profdata).args(&arguments).output();
    let mut record = ProfileMergeRecord {
        outcome: ProfileMergeOutcome::Rejected,
        llvm_profdata_path: llvm_profdata.to_path_buf(),
        arguments: serialized_arguments,
        profraw_files: profiles.to_vec(),
        profdata_path: profdata_path.to_path_buf(),
        profdata_sha256: None,
        bounded_diagnostics: String::new(),
        diagnostics_truncated: false,
        message: None,
    };
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            record.message = Some(format!("llvm-profdata merge could not start: {error}"));
            return record;
        }
    };
    let (diagnostics, truncated) = bounded_diagnostics(&output.stdout, &output.stderr);
    record.bounded_diagnostics = diagnostics;
    record.diagnostics_truncated = truncated;
    if !output.status.success() {
        record.message = Some("llvm-profdata merge failed.".to_owned());
        return record;
    }
    if !record.bounded_diagnostics.trim().is_empty() {
        record.message = Some("llvm-profdata merge emitted warnings.".to_owned());
        return record;
    }
    match sha256_file(profdata_path) {
        Ok(sha256) => {
            record.outcome = ProfileMergeOutcome::Merged;
            record.profdata_sha256 = Some(sha256);
        }
        Err(error) => {
            record.message = Some(format!("Merged PGO profile could not be recorded: {error}"));
        }
    }
    record
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
        BuildPlan, BuildStage, Strategy, effective_target_rustflags, lowest_median_strategy,
        selected_candidate,
    };
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
        assert_eq!(sources, [cargo.join("config.toml")]);

        fs::write(
            cargo.join("config.toml"),
            "[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\nrustc = \"/other/rustc\"\n",
        )
        .expect("build config");
        let error = effective_target_rustflags(fixture.path())
            .expect_err("build rustflags must fail closed");
        assert!(error.contains("effective build.rustflags"));

        fs::write(
            cargo.join("config.toml"),
            "[build]\nrustc = \"/other/rustc\"\n",
        )
        .expect("compiler config");
        let error = effective_target_rustflags(fixture.path())
            .expect_err("compiler override must fail closed");
        assert!(error.contains("effective build.rustc"));
    }
}
