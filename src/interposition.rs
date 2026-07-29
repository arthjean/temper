//! Private compiler interposition for the wrapper-free PGO boundary.
//!
//! This module owns both sides of one private protocol. The child side runs
//! inside the same installed executable before any CLI parsing, classifies the
//! argv Cargo resolved, appends phase controls to real target compilations only,
//! writes one bounded evidence record and replaces itself with the real rustc.
//! The parent side prepares isolated stage directories, applies the process
//! local protocol and validates the captured evidence after Cargo exits.
//!
//! It stays independent of strategy selection and measurement (NFR-018).

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::preflight::{SUPPORTED_HOST, rustc_program};

/// Versioned private protocol. It is internal and is never a public contract.
pub(crate) const PROTOCOL: &str = "temper-rustc-shim-1";
/// Version of the record shape and digest construction.
pub(crate) const NORMALIZATION_VERSION: u32 = 1;

const PROTOCOL_VARIABLE: &str = "TEMPER_SHIM_PROTOCOL";
const RUN_ROOT_VARIABLE: &str = "TEMPER_SHIM_RUN_ROOT";
const REAL_RUSTC_VARIABLE: &str = "TEMPER_SHIM_REAL_RUSTC";
const STAGE_VARIABLE: &str = "TEMPER_SHIM_STAGE";
const TARGET_VARIABLE: &str = "TEMPER_SHIM_TARGET";
const TARGET_DIRECTORY_VARIABLE: &str = "TEMPER_SHIM_TARGET_DIR";
const CAPTURE_DIRECTORY_VARIABLE: &str = "TEMPER_SHIM_CAPTURE_DIR";
const INJECTION_VARIABLE: &str = "TEMPER_SHIM_INJECTION";

const SHIM_VARIABLES: [&str; 8] = [
    PROTOCOL_VARIABLE,
    RUN_ROOT_VARIABLE,
    REAL_RUSTC_VARIABLE,
    STAGE_VARIABLE,
    TARGET_VARIABLE,
    TARGET_DIRECTORY_VARIABLE,
    CAPTURE_DIRECTORY_VARIABLE,
    INJECTION_VARIABLE,
];

/// Exit status of a private protocol failure. It never reaches the public CLI.
pub(crate) const PROTOCOL_FAILURE_EXIT: i32 = 97;
/// Exit status of an observed compiler input that rejects only PGO.
pub(crate) const REJECTION_EXIT: i32 = 98;

pub(crate) const RECORD_LIMIT: usize = 10_000;
pub(crate) const CAPTURE_BYTE_LIMIT: u64 = 32 * 1024 * 1024;
const RECORD_BYTE_LIMIT: u64 = 64 * 1024;
const FIELD_LIMIT: usize = 512;
const CRATE_TYPE_LIMIT: usize = 16;
const NAME_ATTEMPT_LIMIT: u32 = 64;
const DIAGNOSTIC_LIMIT: usize = 512;

/// Bounded argument count of one comparable target compilation. A longer argv
/// is unsupported rather than partially compared.
pub(crate) const ARGUMENT_LIMIT: usize = 2_048;
/// Hex length of one per-argument normalized digest. It keeps a record inside
/// its byte budget while staying far beyond accidental collision.
const ARGUMENT_DIGEST_LENGTH: usize = 16;
/// Replacement of every stage-target-root substring in a normalized argument.
const STAGE_ROOT_PLACEHOLDER: &[u8] = b"<STAGE_ROOT>";
/// File-name prefix of a shim protocol-failure marker.
const PROTOCOL_FAILURE_PREFIX: &str = "protocol-failure-";

pub(crate) const MISSING_FUNCTION_WARNING: &str = "-Cllvm-args=-pgo-warn-missing-function";
pub(crate) const PROFILE_GENERATE_FLAG: &str = "profile-generate";
pub(crate) const PROFILE_USE_FLAG: &str = "profile-use";
pub(crate) const MISSING_FUNCTION_FLAG: &str = "pgo-warn-missing-function";

const CONFLICTING_CONTROLS: [&str; 3] = ["profile-generate", "profile-use", "instrument-coverage"];

pub(crate) const INPUT_CONFLICT_REASON: &str = "pgo_compiler_input_conflict";
pub(crate) const INPUT_AMBIGUITY_REASON: &str = "pgo_compiler_input_ambiguous";
pub(crate) const CAPTURE_MISSING_REASON: &str = "compiler_capture_missing";
pub(crate) const CAPTURE_CORRUPT_REASON: &str = "compiler_capture_corrupt";
pub(crate) const CAPTURE_LIMIT_REASON: &str = "compiler_capture_limit";
pub(crate) const INJECTION_UNEXPECTED_REASON: &str = "compiler_injection_unexpected";
pub(crate) const PROTOCOL_FAILURE_REASON: &str = "compiler_protocol_failure";

/// Compiler-input reasons a build failure must surface unchanged, so an
/// interposition or evidence defect is never reported as a plain build failure.
pub(crate) const COMPILER_INPUT_REASONS: [&str; 7] = [
    INPUT_CONFLICT_REASON,
    INPUT_AMBIGUITY_REASON,
    CAPTURE_MISSING_REASON,
    CAPTURE_CORRUPT_REASON,
    CAPTURE_LIMIT_REASON,
    INJECTION_UNEXPECTED_REASON,
    PROTOCOL_FAILURE_REASON,
];

/// One interposed build stage. Every stage owns a fresh target and capture root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShimStage {
    #[serde(rename = "pgo_reference")]
    Reference,
    #[serde(rename = "pgo_generate")]
    Generate,
    #[serde(rename = "pgo_use")]
    Use,
    #[serde(rename = "pgo_confirmation")]
    Confirmation,
}

impl ShimStage {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Reference => "pgo_reference",
            Self::Generate => "pgo_generate",
            Self::Use => "pgo_use",
            Self::Confirmation => "pgo_confirmation",
        }
    }

    /// Stage directory name. The confirmation stage keeps Cargo's historical
    /// `confirmation/<strategy>` layout so the promotion anchor still proves a
    /// confirmed artifact came from a confirmation build.
    const fn directory_name(self) -> &'static str {
        match self {
            Self::Reference => "pgo_reference",
            Self::Generate => "pgo_generate",
            Self::Use => "pgo_use",
            Self::Confirmation => "confirmation/pgo",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pgo_reference" => Some(Self::Reference),
            "pgo_generate" => Some(Self::Generate),
            "pgo_use" => Some(Self::Use),
            "pgo_confirmation" => Some(Self::Confirmation),
            _ => None,
        }
    }
}

/// Phase controls this stage appends to real target compilations.
#[derive(Clone, Debug)]
pub(crate) enum Injection {
    None,
    Generate(PathBuf),
    Use(PathBuf),
}

impl Injection {
    fn encode(&self) -> OsString {
        match self {
            Self::None => OsString::from("none"),
            Self::Generate(path) => prefixed(b"generate=", path),
            Self::Use(path) => prefixed(b"use=", path),
        }
    }

    fn parse(value: &OsStr) -> Result<Self, String> {
        let bytes = value.as_bytes();
        if bytes == b"none" {
            return Ok(Self::None);
        }
        let (mode, path) = split_once(bytes, b'=').ok_or_else(|| {
            format!("{INJECTION_VARIABLE} must be none, generate=<absolute> or use=<absolute>")
        })?;
        let path = PathBuf::from(OsString::from_vec(path.to_vec()));
        if !path.is_absolute() {
            return Err(format!(
                "{INJECTION_VARIABLE} requires an absolute profile path"
            ));
        }
        match mode {
            b"generate" => Ok(Self::Generate(path)),
            b"use" => Ok(Self::Use(path)),
            _ => Err(format!(
                "{INJECTION_VARIABLE} must be none, generate=<absolute> or use=<absolute>"
            )),
        }
    }

    /// Bounded identifiers of the appended controls. No path is published.
    pub(crate) fn flag_identifiers(&self) -> Vec<&'static str> {
        match self {
            Self::None => Vec::new(),
            Self::Generate(_) => vec![PROFILE_GENERATE_FLAG],
            Self::Use(_) => vec![PROFILE_USE_FLAG, MISSING_FUNCTION_FLAG],
        }
    }

    fn flags(&self) -> Vec<OsString> {
        match self {
            Self::None => Vec::new(),
            Self::Generate(path) => vec![prefixed(b"-Cprofile-generate=", path)],
            Self::Use(path) => vec![
                prefixed(b"-Cprofile-use=", path),
                OsString::from(MISSING_FUNCTION_WARNING),
            ],
        }
    }

    pub(crate) fn profile_path(&self) -> Option<&Path> {
        match self {
            Self::None => None,
            Self::Generate(path) | Self::Use(path) => Some(path),
        }
    }
}

/// Resolved compilers for one interposed run.
#[derive(Clone, Debug)]
pub(crate) struct ShimTools {
    pub(crate) shim_executable: PathBuf,
    pub(crate) shim_sha256: String,
    pub(crate) real_rustc: PathBuf,
    pub(crate) real_rustc_version: String,
}

/// Everything one interposed Cargo build needs to install the private protocol.
#[derive(Clone, Debug)]
pub(crate) struct InterpositionPlan {
    pub(crate) stage: ShimStage,
    pub(crate) tools: ShimTools,
    pub(crate) run_root: PathBuf,
    pub(crate) target_directory: PathBuf,
    pub(crate) capture_directory: PathBuf,
    pub(crate) injection: Injection,
}

impl InterpositionPlan {
    pub(crate) fn record(&self) -> InterpositionRecord {
        InterpositionRecord {
            protocol: PROTOCOL,
            normalization: NORMALIZATION_VERSION,
            stage: self.stage,
            shim_executable: self.tools.shim_executable.clone(),
            shim_sha256: self.tools.shim_sha256.clone(),
            real_rustc: self.tools.real_rustc.clone(),
            real_rustc_version: self.tools.real_rustc_version.clone(),
            capture_directory: self.capture_directory.clone(),
            profile_path: self.injection.profile_path().map(Path::to_path_buf),
            injected_flags: self.injection.flag_identifiers(),
        }
    }

    /// Applies the process-local protocol to one Cargo child.
    pub(crate) fn apply(&self, command: &mut Command) {
        command.env("RUSTC", &self.tools.shim_executable);
        command.env(PROTOCOL_VARIABLE, PROTOCOL);
        command.env(RUN_ROOT_VARIABLE, &self.run_root);
        command.env(REAL_RUSTC_VARIABLE, &self.tools.real_rustc);
        command.env(STAGE_VARIABLE, self.stage.label());
        command.env(TARGET_VARIABLE, SUPPORTED_HOST);
        command.env(TARGET_DIRECTORY_VARIABLE, &self.target_directory);
        command.env(CAPTURE_DIRECTORY_VARIABLE, &self.capture_directory);
        command.env(INJECTION_VARIABLE, self.injection.encode());
    }
}

/// Bounded interposition identity persisted with every interposed build.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct InterpositionRecord {
    pub(crate) protocol: &'static str,
    pub(crate) normalization: u32,
    pub(crate) stage: ShimStage,
    pub(crate) shim_executable: PathBuf,
    pub(crate) shim_sha256: String,
    pub(crate) real_rustc: PathBuf,
    pub(crate) real_rustc_version: String,
    pub(crate) capture_directory: PathBuf,
    pub(crate) profile_path: Option<PathBuf>,
    pub(crate) injected_flags: Vec<&'static str>,
}

/// Classification of one observed rustc invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InvocationClass {
    TargetCompile,
    HostCompile,
    Probe,
    Other,
}

/// One bounded evidence record. It never contains unrestricted raw argv.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CaptureRecord {
    pub(crate) protocol: String,
    pub(crate) normalization: u32,
    pub(crate) record_id: String,
    pub(crate) stage: ShimStage,
    pub(crate) class: InvocationClass,
    pub(crate) exact_target: bool,
    pub(crate) emit: bool,
    pub(crate) print: bool,
    pub(crate) crate_name: Option<String>,
    pub(crate) crate_types: Vec<String>,
    pub(crate) metadata: Option<String>,
    pub(crate) extra_filename: Option<String>,
    pub(crate) argument_count: usize,
    pub(crate) pre_digest: String,
    pub(crate) post_digest: String,
    /// Ordered digest of the pre-injection argv after stage-root normalization.
    pub(crate) normalized_digest: String,
    /// Per-argument normalized digests. Only a target compilation carries them,
    /// because only target compilations are compared across stages.
    pub(crate) argument_digests: Vec<String>,
    pub(crate) injected_flags: Vec<String>,
    pub(crate) rejection: Option<String>,
}

/// Validated aggregate of one stage's compiler evidence.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CaptureAggregate {
    pub(crate) protocol: &'static str,
    pub(crate) normalization: u32,
    pub(crate) stage: ShimStage,
    pub(crate) record_count: usize,
    pub(crate) capture_bytes: u64,
    pub(crate) target_compilations: usize,
    pub(crate) host_compilations: usize,
    pub(crate) probes: usize,
    pub(crate) other_invocations: usize,
    pub(crate) injected_invocations: usize,
    pub(crate) injected_flags: Vec<&'static str>,
    pub(crate) capture_digest: String,
    #[serde(skip)]
    pub(crate) records: Vec<CaptureRecord>,
}

/// Stable reason and bounded message of one rejected capture set.
#[derive(Clone, Debug)]
pub(crate) struct CaptureFailure {
    pub(crate) reason: &'static str,
    pub(crate) message: String,
}

impl CaptureFailure {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: bounded(&message.into()),
        }
    }
}

/// Canonical target and capture directories of one interposed stage.
#[derive(Clone, Debug)]
pub(crate) struct StageDirectories {
    pub(crate) target: PathBuf,
    pub(crate) capture: PathBuf,
}

/// Dispatches the private shim protocol before any public CLI parsing.
pub(crate) fn dispatch() {
    if std::env::var_os(PROTOCOL_VARIABLE).is_none() {
        return;
    }
    run_shim()
}

fn run_shim() -> ! {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let context = match ShimContext::from_environment() {
        Ok(context) => context,
        Err(message) => protocol_failure(&message),
    };
    let classification = classify(&arguments, &context.target);
    let rejection = classification.rejection();
    let injected = if rejection.is_none() && classification.class == InvocationClass::TargetCompile
    {
        context.injection.flags()
    } else {
        Vec::new()
    };
    let mut final_arguments = arguments.clone();
    final_arguments.extend(injected.iter().cloned());

    let record = build_record(
        &context,
        &classification,
        &arguments,
        &final_arguments,
        rejection,
    );
    if let Err(message) = write_record(&context.capture_directory, &record) {
        protocol_failure(&message);
    }

    if let Some(reason) = rejection {
        eprintln!(
            "temper compiler input rejected ({reason}): the resolved rustc arguments already carry a profiling or coverage control, so PGO cannot append its phase control."
        );
        std::process::exit(REJECTION_EXIT)
    }

    let mut command = Command::new(&context.real_rustc);
    command.args(&final_arguments);
    for name in SHIM_VARIABLES {
        command.env_remove(name);
    }
    let error = command.exec();
    protocol_failure(&format!(
        "could not replace the shim with the real rustc: {error}"
    ))
}

struct ShimContext {
    stage: ShimStage,
    target: String,
    real_rustc: PathBuf,
    target_directory: PathBuf,
    capture_directory: PathBuf,
    injection: Injection,
}

impl ShimContext {
    fn from_environment() -> Result<Self, String> {
        reject_duplicate_values()?;
        let protocol = required_string(PROTOCOL_VARIABLE)?;
        if protocol != PROTOCOL {
            return Err(format!(
                "unsupported private protocol version {}",
                bounded(&protocol)
            ));
        }
        let run_root = canonical_directory(RUN_ROOT_VARIABLE)?;
        let capture_directory = canonical_directory(CAPTURE_DIRECTORY_VARIABLE)?;
        let target_directory = canonical_directory(TARGET_DIRECTORY_VARIABLE)?;
        for (name, path) in [
            (CAPTURE_DIRECTORY_VARIABLE, &capture_directory),
            (TARGET_DIRECTORY_VARIABLE, &target_directory),
        ] {
            if !path.starts_with(&run_root) {
                return Err(format!("{name} escaped the canonical run root"));
            }
        }
        let stage = ShimStage::parse(&required_string(STAGE_VARIABLE)?)
            .ok_or_else(|| format!("{STAGE_VARIABLE} is not a supported interposition stage"))?;
        let target = required_string(TARGET_VARIABLE)?;
        if target != SUPPORTED_HOST {
            return Err(format!("{TARGET_VARIABLE} is not the supported target"));
        }
        let real_rustc = required_path(REAL_RUSTC_VARIABLE)?;
        if !real_rustc.is_absolute() {
            return Err(format!("{REAL_RUSTC_VARIABLE} must be an absolute path"));
        }
        let resolved = absolute_program(&real_rustc)
            .map_err(|error| format!("{REAL_RUSTC_VARIABLE} is unavailable: {error}"))?;
        if resolved != real_rustc {
            return Err(format!(
                "{REAL_RUSTC_VARIABLE} is not a canonical compiler path"
            ));
        }
        let injection = Injection::parse(&required_value(INJECTION_VARIABLE)?)?;
        if let Some(path) = injection.profile_path()
            && !path.starts_with(&run_root)
        {
            return Err(format!(
                "{INJECTION_VARIABLE} requires a profile path inside the current run"
            ));
        }
        Ok(Self {
            stage,
            target,
            real_rustc,
            target_directory,
            capture_directory,
            injection,
        })
    }
}

/// Resolves the directory a protocol-failure marker may be written to.
///
/// It is only consulted on the failure path, and returns a directory only when
/// the private run root and capture directory are both canonical and nested, so
/// a malformed protocol can never create a file outside the current run.
fn protocol_failure_directory() -> Option<PathBuf> {
    let run_root = canonical_directory(RUN_ROOT_VARIABLE).ok()?;
    let capture_directory = canonical_directory(CAPTURE_DIRECTORY_VARIABLE).ok()?;
    capture_directory
        .starts_with(&run_root)
        .then_some(capture_directory)
}

fn reject_duplicate_values() -> Result<(), String> {
    for name in SHIM_VARIABLES {
        let occurrences = std::env::vars_os()
            .filter(|(key, _)| key.as_os_str() == OsStr::new(name))
            .count();
        if occurrences > 1 {
            return Err(format!("duplicate private protocol value {name}"));
        }
    }
    Ok(())
}

fn required_value(name: &str) -> Result<OsString, String> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("missing private protocol value {name}")),
    }
}

fn required_string(name: &str) -> Result<String, String> {
    required_value(name)?
        .into_string()
        .map_err(|_| format!("{name} must be valid UTF-8"))
}

fn required_path(name: &str) -> Result<PathBuf, String> {
    required_value(name).map(PathBuf::from)
}

fn canonical_directory(name: &str) -> Result<PathBuf, String> {
    let path = required_path(name)?;
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    let path =
        fs::canonicalize(&path).map_err(|error| format!("{name} is unavailable: {error}"))?;
    if !path.is_dir() {
        return Err(format!("{name} must be an existing directory"));
    }
    Ok(path)
}

struct Classification {
    class: InvocationClass,
    exact_target: bool,
    emit: bool,
    print: bool,
    crate_name: Option<String>,
    crate_types: Vec<String>,
    metadata: Option<String>,
    extra_filename: Option<String>,
    conflict: bool,
    ambiguous: bool,
}

impl Classification {
    fn rejection(&self) -> Option<&'static str> {
        if self.class != InvocationClass::TargetCompile {
            return None;
        }
        if self.conflict {
            Some(INPUT_CONFLICT_REASON)
        } else if self.ambiguous {
            Some(INPUT_AMBIGUITY_REASON)
        } else {
            None
        }
    }
}

/// Classifies one resolved rustc argv.
///
/// EP-001 proved the discriminator: a real target compilation carries the exact
/// supported `--target` and a compilation `--emit`, while a target-info probe
/// may carry the same triple with `--print` and must pass through.
fn classify(arguments: &[OsString], target: &str) -> Classification {
    let mut targets: HashSet<Vec<u8>> = HashSet::new();
    let mut emit = false;
    let mut print = false;
    let mut crate_name = None;
    let mut crate_name_lost = false;
    let mut crate_types = Vec::new();
    let mut metadata = None;
    let mut extra_filename = None;
    let mut conflict = false;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_os_str();
        let next = arguments.get(index + 1).map(OsString::as_os_str);
        if argument == OsStr::new("--target") {
            if let Some(value) = next {
                targets.insert(value.as_bytes().to_vec());
            }
        } else if let Some(value) = suffix(argument, "--target=") {
            targets.insert(value.to_vec());
        } else if argument == OsStr::new("--emit") || suffix(argument, "--emit=").is_some() {
            emit = true;
        } else if argument == OsStr::new("--print") || suffix(argument, "--print=").is_some() {
            print = true;
        } else if argument == OsStr::new("--crate-name") {
            match next.and_then(lossless) {
                Some(value) => crate_name = Some(value),
                None => crate_name_lost = true,
            }
        } else if let Some(value) = suffix(argument, "--crate-name=") {
            match lossless(OsStr::from_bytes(value)) {
                Some(value) => crate_name = Some(value),
                None => crate_name_lost = true,
            }
        } else if argument == OsStr::new("--crate-type") {
            if let Some(value) = next.and_then(lossless) {
                crate_types.push(value);
            }
        } else if let Some(value) = suffix(argument, "--crate-type=") {
            if let Some(value) = lossless(OsStr::from_bytes(value)) {
                crate_types.push(value);
            }
        } else if let Some(value) = codegen_option(argument, next) {
            let (key, value) = match split_once(value.as_bytes(), b'=') {
                Some((key, value)) => (key, Some(value)),
                None => (value.as_bytes(), None),
            };
            conflict |= CONFLICTING_CONTROLS
                .iter()
                .any(|control| key == control.as_bytes());
            if key == b"metadata" {
                metadata = value.and_then(|value| lossless(OsStr::from_bytes(value)));
            } else if key == b"extra-filename" {
                extra_filename = value.and_then(|value| lossless(OsStr::from_bytes(value)));
            }
        }
        index += 1;
    }

    let exact_target = targets.contains(target.as_bytes());
    let class = if print {
        InvocationClass::Probe
    } else if exact_target && emit {
        InvocationClass::TargetCompile
    } else if emit {
        InvocationClass::HostCompile
    } else {
        InvocationClass::Other
    };
    let ambiguous = targets.len() > 1
        || (class == InvocationClass::TargetCompile
            && (crate_name.is_none() || crate_name_lost || arguments.len() > ARGUMENT_LIMIT));

    crate_types.truncate(CRATE_TYPE_LIMIT);
    Classification {
        class,
        exact_target,
        emit,
        print,
        crate_name,
        crate_types,
        metadata,
        extra_filename,
        conflict,
        ambiguous,
    }
}

fn codegen_option<'a>(argument: &'a OsStr, next: Option<&'a OsStr>) -> Option<&'a OsStr> {
    if argument == OsStr::new("-C") {
        return next;
    }
    suffix(argument, "-C").map(OsStr::from_bytes)
}

fn suffix<'a>(argument: &'a OsStr, prefix: &str) -> Option<&'a [u8]> {
    argument.as_bytes().strip_prefix(prefix.as_bytes())
}

fn lossless(value: &OsStr) -> Option<String> {
    std::str::from_utf8(value.as_bytes()).ok().map(bounded)
}

fn build_record(
    context: &ShimContext,
    classification: &Classification,
    arguments: &[OsString],
    final_arguments: &[OsString],
    rejection: Option<&'static str>,
) -> CaptureRecord {
    let injected_flags =
        if classification.class == InvocationClass::TargetCompile && rejection.is_none() {
            context
                .injection
                .flag_identifiers()
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
    let stage_root = context.target_directory.as_os_str().as_bytes();
    let normalized: Vec<Vec<u8>> = arguments
        .iter()
        .map(|argument| normalize_argument(argument, stage_root))
        .collect();
    // Only a target compilation is compared across stages, so only it pays the
    // per-argument digest cost inside the bounded record.
    let argument_digests = if classification.class == InvocationClass::TargetCompile {
        normalized
            .iter()
            .map(|argument| short_digest(argument))
            .collect()
    } else {
        Vec::new()
    };
    CaptureRecord {
        protocol: PROTOCOL.to_owned(),
        normalization: NORMALIZATION_VERSION,
        record_id: String::new(),
        stage: context.stage,
        class: classification.class,
        exact_target: classification.exact_target,
        emit: classification.emit,
        print: classification.print,
        crate_name: classification.crate_name.clone(),
        crate_types: classification.crate_types.clone(),
        metadata: classification.metadata.clone(),
        extra_filename: classification.extra_filename.clone(),
        argument_count: arguments.len(),
        pre_digest: argument_digest(arguments),
        post_digest: argument_digest(final_arguments),
        normalized_digest: framed_digest(
            &normalized.iter().map(Vec::as_slice).collect::<Vec<&[u8]>>(),
        ),
        argument_digests,
        injected_flags,
        rejection: rejection.map(str::to_owned),
    }
}

/// Replaces every stage-target-root substring with the documented placeholder.
///
/// EP-001 observed exactly three argv positions that depend on the stage root:
/// `--out-dir`, `-L dependency=` and `--extern <crate>=`. Normalizing the root
/// itself covers all three without interpreting Cargo's argument grammar.
fn normalize_argument(argument: &OsStr, stage_root: &[u8]) -> Vec<u8> {
    let bytes = argument.as_bytes();
    if stage_root.is_empty() || bytes.len() < stage_root.len() {
        return bytes.to_vec();
    }
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(stage_root) {
            normalized.extend_from_slice(STAGE_ROOT_PLACEHOLDER);
            index += stage_root.len();
        } else {
            normalized.push(bytes[index]);
            index += 1;
        }
    }
    normalized
}

fn short_digest(value: &[u8]) -> String {
    let mut digest = framed_digest(&[value]);
    digest.truncate(ARGUMENT_DIGEST_LENGTH);
    digest
}

fn write_record(directory: &Path, record: &CaptureRecord) -> Result<(), String> {
    let nanoseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    for attempt in 0..NAME_ATTEMPT_LIMIT {
        let mut candidate = record.clone();
        candidate.record_id = record_identity(record, nanoseconds, attempt);
        let path = directory.join(format!("{}.json", candidate.record_id));
        let serialized = serde_json::to_vec(&candidate)
            .map_err(|error| format!("could not serialize the shim capture record: {error}"))?;
        if serialized.len() as u64 > RECORD_BYTE_LIMIT {
            return Err("the shim capture record exceeded its bounded size".to_owned());
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                return file
                    .write_all(&serialized)
                    .map_err(|error| format!("could not write the shim capture record: {error}"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("could not create the shim capture record: {error}"));
            }
        }
    }
    Err("exhausted the bounded capture-name retry policy".to_owned())
}

fn record_identity(record: &CaptureRecord, nanoseconds: u128, attempt: u32) -> String {
    let process = std::process::id().to_be_bytes();
    let nanoseconds = nanoseconds.to_be_bytes();
    let attempt = attempt.to_be_bytes();
    framed_digest(&[
        record.stage.label().as_bytes(),
        record.pre_digest.as_bytes(),
        record.post_digest.as_bytes(),
        &process,
        &nanoseconds,
        &attempt,
    ])
}

fn argument_digest(arguments: &[OsString]) -> String {
    let framed: Vec<&[u8]> = arguments
        .iter()
        .map(|argument| argument.as_bytes())
        .collect();
    framed_digest(&framed)
}

/// Length-framed digest over ordered text values, sharing the byte framing so
/// derived aggregates cannot be forged by concatenation.
pub(crate) fn framed_text_digest(values: &[String]) -> String {
    let framed: Vec<&[u8]> = values.iter().map(|value| value.as_bytes()).collect();
    framed_digest(&framed)
}

/// Length-framed digest over raw bytes. Framing keeps concatenation unambiguous.
fn framed_digest(values: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((values.len() as u64).to_be_bytes());
    for value in values {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    format!("{:x}", hasher.finalize())
}

fn prefixed(prefix: &[u8], path: &Path) -> OsString {
    let mut bytes = prefix.to_vec();
    bytes.extend_from_slice(path.as_os_str().as_bytes());
    OsString::from_vec(bytes)
}

fn split_once(value: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    value
        .iter()
        .position(|byte| *byte == separator)
        .map(|index| (&value[..index], &value[index + 1..]))
}

fn bounded(value: &str) -> String {
    let mut boundary = FIELD_LIMIT.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}

/// One bounded protocol-failure marker.
///
/// It is deliberately not a [`CaptureRecord`]: a protocol failure has no
/// classified invocation, and the parent must be able to tell an aborted shim
/// apart from an absent capture set without matching diagnostic text.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProtocolFailureMarker {
    protocol: String,
    reason: String,
    message: String,
}

fn protocol_failure(message: &str) -> ! {
    let message = bounded_diagnostic(message);
    // The marker lets the parent tell an aborted shim apart from an absent
    // capture set. It sharpens the reason; it never makes the failure closed.
    if let Some(directory) = protocol_failure_directory() {
        write_protocol_failure(&directory, message);
    }
    eprintln!("temper shim protocol failure ({PROTOCOL}): {message}");
    std::process::exit(PROTOCOL_FAILURE_EXIT)
}

/// Writes one create-new marker. Every failure here is deliberately ignored:
/// the marker only sharpens the parent's reason, and the shim must still exit
/// with its bounded diagnostic when the capture directory cannot accept it.
fn write_protocol_failure(directory: &Path, message: &str) {
    let marker = ProtocolFailureMarker {
        protocol: PROTOCOL.to_owned(),
        reason: PROTOCOL_FAILURE_REASON.to_owned(),
        message: message.to_owned(),
    };
    let Ok(serialized) = serde_json::to_vec(&marker) else {
        return;
    };
    let nanoseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    for attempt in 0..NAME_ATTEMPT_LIMIT {
        let identity = framed_digest(&[
            &std::process::id().to_be_bytes(),
            &nanoseconds.to_be_bytes(),
            &attempt.to_be_bytes(),
        ]);
        let path = directory.join(format!("{PROTOCOL_FAILURE_PREFIX}{identity}.json"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ignored = file.write_all(&serialized);
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return,
        }
    }
}

fn bounded_diagnostic(message: &str) -> &str {
    let mut boundary = DIAGNOSTIC_LIMIT.min(message.len());
    while !message.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    &message[..boundary]
}

/// Resolves the shim executable and the real rustc before any `RUSTC` override.
pub(crate) fn resolve_tools(expected_rustc_version: &str) -> Result<ShimTools, String> {
    let shim_executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("The Temper executable could not be resolved: {error}"))?;
    if !shim_executable.is_file() {
        return Err("The Temper executable is not a regular file.".to_owned());
    }
    let real_rustc = resolve_real_rustc()?;
    identified_tools(shim_executable, real_rustc, expected_rustc_version)
}

/// Revalidates a previously recorded shim and real rustc pair before reusing it
/// for the confirmation stage.
pub(crate) fn validated_tools(recorded: &InterpositionRecord) -> Result<ShimTools, String> {
    let shim = fs::canonicalize(&recorded.shim_executable)
        .map_err(|error| format!("The recorded Temper executable is unavailable: {error}"))?;
    let compiler = absolute_program(&recorded.real_rustc)
        .map_err(|error| format!("The recorded real rustc is unavailable: {error}"))?;
    if shim != recorded.shim_executable || compiler != recorded.real_rustc || !shim.is_file() {
        return Err("A recorded compiler path changed since the accepted PGO build.".to_owned());
    }
    let tools = identified_tools(shim, compiler, &recorded.real_rustc_version)?;
    if tools.shim_sha256 != recorded.shim_sha256 {
        return Err("The Temper executable changed since the accepted PGO build.".to_owned());
    }
    Ok(tools)
}

/// Hashes the shim and proves the real rustc identity before it is installed.
fn identified_tools(
    shim_executable: PathBuf,
    real_rustc: PathBuf,
    expected_rustc_version: &str,
) -> Result<ShimTools, String> {
    let shim_sha256 = crate::hash::sha256_file(&shim_executable)
        .map_err(|error| format!("The Temper executable could not be hashed: {error}"))?;
    let version = Command::new(&real_rustc)
        .arg("-Vv")
        .output()
        .map_err(|_| "The resolved real rustc identity probe could not start.".to_owned())?;
    if !version.status.success() {
        return Err("The resolved real rustc identity probe failed.".to_owned());
    }
    let real_rustc_version = String::from_utf8(version.stdout)
        .map_err(|_| "The resolved real rustc identity was not valid UTF-8.".to_owned())?
        .trim()
        .to_owned();
    if real_rustc_version != expected_rustc_version.trim() {
        return Err(
            "The resolved real rustc identity does not match the preflight compiler.".to_owned(),
        );
    }
    Ok(ShimTools {
        shim_executable,
        shim_sha256,
        real_rustc,
        real_rustc_version,
    })
}

/// Resolves a compiler program to an absolute path with a canonical parent.
///
/// The final component is deliberately not resolved through symlinks: a rustup
/// proxy dispatches on its own file name, so following the link would execute
/// `rustup` instead of the compiler it proxies.
fn absolute_program(program: &Path) -> Result<PathBuf, String> {
    let parent = program
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "a compiler path needs a parent directory".to_owned())?;
    let name = program
        .file_name()
        .ok_or_else(|| "a compiler path needs a file name".to_owned())?;
    let resolved = fs::canonicalize(parent)
        .map_err(|error| format!("its directory could not be canonicalized: {error}"))?
        .join(name);
    let metadata = fs::metadata(&resolved).map_err(|error| format!("{error}"))?;
    if !metadata.is_file() {
        return Err("it is not a regular file".to_owned());
    }
    Ok(resolved)
}

fn resolve_real_rustc() -> Result<PathBuf, String> {
    let program = PathBuf::from(rustc_program());
    if program.components().count() > 1 {
        absolute_program(&program)
            .map_err(|error| format!("The active rustc could not be resolved: {error}"))
    } else {
        search_path(program.as_os_str())
    }
}

fn search_path(program: &OsStr) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| "PATH is unset; the active rustc cannot be resolved.".to_owned())?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return absolute_program(&candidate)
                .map_err(|error| format!("The active rustc could not be resolved: {error}"));
        }
    }
    Err("The active rustc was not found on PATH.".to_owned())
}

/// Root of every stage capture directory of one run.
pub(crate) fn capture_root(run_directory: &Path) -> PathBuf {
    run_directory.join("captures")
}

/// Claims one interposed stage and returns its complete private protocol.
pub(crate) fn plan_stage(
    tools: &ShimTools,
    run_directory: &Path,
    target_root: &Path,
    stage: ShimStage,
    injection: Injection,
) -> Result<InterpositionPlan, String> {
    let run_root = fs::canonicalize(run_directory).map_err(|error| {
        format!(
            "Could not canonicalize the run directory {}: {error}",
            run_directory.display()
        )
    })?;
    let directories = claim_stage_directories(
        target_root,
        &capture_root(run_directory),
        stage.directory_name(),
    )?;
    for path in [&directories.target, &directories.capture] {
        if !path.starts_with(&run_root) {
            return Err(format!(
                "Interposed stage directory {} escaped the current run.",
                path.display()
            ));
        }
    }
    if let Some(profile) = injection.profile_path()
        && !profile.starts_with(&run_root)
    {
        return Err("Every PGO profile path must stay inside the current run.".to_owned());
    }
    Ok(InterpositionPlan {
        stage,
        tools: tools.clone(),
        run_root,
        target_directory: directories.target,
        capture_directory: directories.capture,
        injection,
    })
}

/// Claims one unique, initially absent target and capture directory pair.
pub(crate) fn claim_stage_directories(
    target_root: &Path,
    capture_root: &Path,
    name: &str,
) -> Result<StageDirectories, String> {
    let target_root = canonical_root(target_root)?;
    let capture_root = canonical_root(capture_root)?;
    Ok(StageDirectories {
        target: claim_directory(&target_root, name)?,
        capture: claim_directory(&capture_root, name)?,
    })
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|error| {
        format!(
            "Could not create the interposition root {}: {error}",
            root.display()
        )
    })?;
    fs::canonicalize(root).map_err(|error| {
        format!(
            "Could not canonicalize the interposition root {}: {error}",
            root.display()
        )
    })
}

fn claim_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = root.join(name);
    if fs::symlink_metadata(&path).is_ok() {
        return Err(format!(
            "Interposed stage directory {} already exists; every stage requires a unique initially absent directory.",
            path.display()
        ));
    }
    fs::create_dir_all(&path).map_err(|error| {
        format!(
            "Could not create the interposed stage directory {}: {error}",
            path.display()
        )
    })?;
    let canonical = fs::canonicalize(&path).map_err(|error| {
        format!(
            "Could not canonicalize the interposed stage directory {}: {error}",
            path.display()
        )
    })?;
    if canonical != path {
        return Err(format!(
            "Interposed stage directory {} aliases another canonical path.",
            path.display()
        ));
    }
    Ok(canonical)
}

/// Validates and aggregates one stage's capture set after Cargo exits.
pub(crate) fn collect(plan: &InterpositionPlan) -> Result<CaptureAggregate, CaptureFailure> {
    let root = fs::canonicalize(&plan.capture_directory).map_err(|error| {
        CaptureFailure::new(
            CAPTURE_MISSING_REASON,
            format!("The stage capture directory is unavailable: {error}"),
        )
    })?;
    if root != plan.capture_directory {
        return Err(CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            "The stage capture directory no longer matches its canonical claim.",
        ));
    }
    let entries = fs::read_dir(&root).map_err(|error| {
        CaptureFailure::new(
            CAPTURE_MISSING_REASON,
            format!("The stage capture directory could not be read: {error}"),
        )
    })?;

    let expected: Vec<String> = plan
        .injection
        .flag_identifiers()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut records = Vec::new();
    let mut identities = HashSet::new();
    let mut capture_bytes = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|error| {
            CaptureFailure::new(
                CAPTURE_MISSING_REASON,
                format!("A stage capture entry could not be read: {error}"),
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                format!("A stage capture entry could not be inspected: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                "A stage capture entry is a symlink or not a regular file.",
            ));
        }
        if !path.starts_with(&root) {
            return Err(CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                "A stage capture entry escaped its canonical capture root.",
            ));
        }
        if metadata.len() > RECORD_BYTE_LIMIT {
            return Err(CaptureFailure::new(
                CAPTURE_LIMIT_REASON,
                "A stage capture record exceeded its bounded size.",
            ));
        }
        capture_bytes = capture_bytes.saturating_add(metadata.len());
        if entry
            .file_name()
            .as_bytes()
            .starts_with(PROTOCOL_FAILURE_PREFIX.as_bytes())
        {
            return Err(read_protocol_failure(&path));
        }
        records.push(read_record(&path, &root)?);
        if records.len() > RECORD_LIMIT {
            return Err(CaptureFailure::new(
                CAPTURE_LIMIT_REASON,
                format!("The stage produced more than {RECORD_LIMIT} capture records."),
            ));
        }
        if capture_bytes > CAPTURE_BYTE_LIMIT {
            return Err(CaptureFailure::new(
                CAPTURE_LIMIT_REASON,
                "The stage capture set exceeded 32 MiB.",
            ));
        }
    }

    records.sort_by(|left, right| left.record_id.cmp(&right.record_id));
    for record in &records {
        if record.stage != plan.stage {
            return Err(CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                "A stage capture record belongs to another interposition stage.",
            ));
        }
        if !identities.insert(record.record_id.clone()) {
            return Err(CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                "Two stage capture records declare the same record identity.",
            ));
        }
    }
    if let Some(record) = records.iter().find(|record| record.rejection.is_some()) {
        return Err(match record.rejection.as_deref() {
            Some(INPUT_CONFLICT_REASON) => CaptureFailure::new(
                INPUT_CONFLICT_REASON,
                "A resolved target compilation already carried a profiling or coverage control.",
            ),
            Some(INPUT_AMBIGUITY_REASON) => CaptureFailure::new(
                INPUT_AMBIGUITY_REASON,
                "A resolved target compilation had an unsupported argument shape.",
            ),
            _ => CaptureFailure::new(
                CAPTURE_CORRUPT_REASON,
                "A stage capture record declares an unknown rejection reason.",
            ),
        });
    }

    let mut aggregate = CaptureAggregate {
        protocol: PROTOCOL,
        normalization: NORMALIZATION_VERSION,
        stage: plan.stage,
        record_count: records.len(),
        capture_bytes,
        target_compilations: 0,
        host_compilations: 0,
        probes: 0,
        other_invocations: 0,
        injected_invocations: 0,
        injected_flags: plan.injection.flag_identifiers(),
        capture_digest: String::new(),
        records,
    };
    for record in &aggregate.records {
        match record.class {
            InvocationClass::TargetCompile => {
                aggregate.target_compilations += 1;
                if record.injected_flags != expected {
                    return Err(CaptureFailure::new(
                        INJECTION_UNEXPECTED_REASON,
                        "A target compilation did not receive exactly the expected phase controls.",
                    ));
                }
                if !expected.is_empty() {
                    aggregate.injected_invocations += 1;
                }
            }
            other => {
                if !record.injected_flags.is_empty() {
                    return Err(CaptureFailure::new(
                        INJECTION_UNEXPECTED_REASON,
                        "A probe or host invocation received a Temper phase control.",
                    ));
                }
                match other {
                    InvocationClass::HostCompile => aggregate.host_compilations += 1,
                    InvocationClass::Probe => aggregate.probes += 1,
                    _ => aggregate.other_invocations += 1,
                }
            }
        }
    }
    if aggregate.target_compilations == 0 {
        return Err(CaptureFailure::new(
            CAPTURE_MISSING_REASON,
            "The stage produced no observed target compilation.",
        ));
    }

    let mut digests: Vec<&str> = aggregate
        .records
        .iter()
        .map(|record| record.post_digest.as_str())
        .collect();
    digests.sort_unstable();
    let digests: Vec<&[u8]> = digests.iter().map(|digest| digest.as_bytes()).collect();
    // The digest set is sorted, so a stage aggregate does not depend on the
    // order in which parallel shim processes happened to create their records.
    aggregate.capture_digest = framed_digest(&digests);
    Ok(aggregate)
}

fn read_protocol_failure(path: &Path) -> CaptureFailure {
    let marker = fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<ProtocolFailureMarker>(&contents).ok())
        .filter(|marker| marker.protocol == PROTOCOL && marker.reason == PROTOCOL_FAILURE_REASON);
    match marker {
        Some(marker) => CaptureFailure::new(
            PROTOCOL_FAILURE_REASON,
            format!(
                "The private compiler shim aborted before executing rustc: {}",
                marker.message
            ),
        ),
        None => CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            "A stage protocol-failure marker could not be read.",
        ),
    }
}

fn read_record(path: &Path, root: &Path) -> Result<CaptureRecord, CaptureFailure> {
    let contents = fs::read(path).map_err(|error| {
        CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            format!("A stage capture record could not be read: {error}"),
        )
    })?;
    let record: CaptureRecord = serde_json::from_slice(&contents).map_err(|error| {
        CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            format!("A stage capture record could not be parsed: {error}"),
        )
    })?;
    if record.protocol != PROTOCOL || record.normalization != NORMALIZATION_VERSION {
        return Err(CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            "A stage capture record used an unsupported protocol version.",
        ));
    }
    let expected = root.join(format!("{}.json", record.record_id));
    if expected != path {
        return Err(CaptureFailure::new(
            CAPTURE_CORRUPT_REASON,
            "A stage capture record identity does not match its file name.",
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{
        CAPTURE_CORRUPT_REASON, CAPTURE_LIMIT_REASON, CAPTURE_MISSING_REASON, CRATE_TYPE_LIMIT,
        CaptureRecord, Classification, INJECTION_UNEXPECTED_REASON, INPUT_AMBIGUITY_REASON,
        INPUT_CONFLICT_REASON, Injection, InterpositionPlan, InvocationClass,
        NORMALIZATION_VERSION, PROTOCOL, PROTOCOL_FAILURE_REASON, RECORD_LIMIT, ShimStage,
        ShimTools, argument_digest, claim_stage_directories, classify, collect, normalize_argument,
    };
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    const TARGET: &str = "x86_64-unknown-linux-gnu";

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn classification(values: &[&str]) -> Classification {
        classify(&arguments(values), TARGET)
    }

    #[test]
    fn classifies_target_compilations_by_target_and_emit() {
        let target = classification(&[
            "--crate-name",
            "app",
            "--target",
            TARGET,
            "--emit=dep-info,link",
        ]);
        assert_eq!(target.class, InvocationClass::TargetCompile);
        assert_eq!(target.crate_name.as_deref(), Some("app"));
        assert!(target.rejection().is_none());

        let equals_form = classification(&[
            "--crate-name=app",
            &format!("--target={TARGET}"),
            "--emit=link",
        ]);
        assert_eq!(equals_form.class, InvocationClass::TargetCompile);
    }

    #[test]
    fn passes_probes_and_host_units_through() {
        let probe = classification(&["--print=file-names", "--target", TARGET, "--emit=link"]);
        assert_eq!(probe.class, InvocationClass::Probe);
        assert!(probe.rejection().is_none());

        let host = classification(&["--crate-name", "build_script_build", "--emit=link"]);
        assert_eq!(host.class, InvocationClass::HostCompile);

        let foreign = classification(&[
            "--crate-name",
            "app",
            "--target",
            "aarch64-unknown-linux-gnu",
            "--emit=link",
        ]);
        assert_eq!(foreign.class, InvocationClass::HostCompile);

        let version = classification(&["-vV"]);
        assert_eq!(version.class, InvocationClass::Other);
    }

    #[test]
    fn a_host_path_containing_the_triple_is_not_a_target_compilation() {
        let host = classification(&[
            "--crate-name",
            "macro",
            "--out-dir",
            "/tmp/x86_64-unknown-linux-gnu/release",
            "--emit=link",
        ]);
        assert_eq!(host.class, InvocationClass::HostCompile);
    }

    #[test]
    fn rejects_existing_profiling_and_coverage_controls() {
        for control in [
            "-Cprofile-generate=/tmp/p",
            "-Cprofile-use=/tmp/p.profdata",
            "-Cinstrument-coverage",
        ] {
            let record = classification(&[
                "--crate-name",
                "app",
                "--target",
                TARGET,
                "--emit=link",
                control,
            ]);
            assert_eq!(record.rejection(), Some(INPUT_CONFLICT_REASON), "{control}");
        }
        let split = classification(&[
            "--crate-name",
            "app",
            "--target",
            TARGET,
            "--emit=link",
            "-C",
            "profile-use=/tmp/p.profdata",
        ]);
        assert_eq!(split.rejection(), Some(INPUT_CONFLICT_REASON));

        let host = classification(&["--crate-name", "build", "--emit=link", "-Cprofile-use=/p"]);
        assert!(host.rejection().is_none());
    }

    #[test]
    fn rejects_ambiguous_target_and_identity_shapes() {
        let conflicting_targets = classification(&[
            "--crate-name",
            "app",
            "--target",
            TARGET,
            "--target",
            "aarch64-unknown-linux-gnu",
            "--emit=link",
        ]);
        assert_eq!(
            conflicting_targets.rejection(),
            Some(INPUT_AMBIGUITY_REASON)
        );

        let anonymous = classification(&["--target", TARGET, "--emit=link"]);
        assert_eq!(anonymous.rejection(), Some(INPUT_AMBIGUITY_REASON));
    }

    #[test]
    fn bounds_recorded_crate_types() {
        let mut values = vec!["--crate-name", "app", "--target", TARGET, "--emit=link"];
        values.extend(std::iter::repeat_n(
            "--crate-type=lib",
            CRATE_TYPE_LIMIT + 4,
        ));
        assert_eq!(classification(&values).crate_types.len(), CRATE_TYPE_LIMIT);
    }

    #[test]
    fn digests_frame_raw_argument_bytes() {
        let split = vec![OsString::from("ab"), OsString::from("c")];
        let joined = vec![OsString::from("abc")];
        assert_ne!(argument_digest(&split), argument_digest(&joined));

        let invalid = vec![OsString::from_vec(vec![0xff, 0xfe])];
        assert_eq!(argument_digest(&invalid).len(), 64);
    }

    #[test]
    fn stage_root_normalization_covers_every_observed_position() {
        let stage_root = b"/run/target/pgo_generate".as_slice();
        for (argument, expected) in [
            (
                "/run/target/pgo_generate/release/deps",
                "<STAGE_ROOT>/release/deps",
            ),
            (
                "dependency=/run/target/pgo_generate/release/deps",
                "dependency=<STAGE_ROOT>/release/deps",
            ),
            (
                "unitlib=/run/target/pgo_generate/release/deps/libunitlib.rlib",
                "unitlib=<STAGE_ROOT>/release/deps/libunitlib.rlib",
            ),
            ("--emit=dep-info,link", "--emit=dep-info,link"),
        ] {
            assert_eq!(
                normalize_argument(OsStr::new(argument), stage_root),
                expected.as_bytes(),
                "{argument}"
            );
        }
        // A stage-root-free argument keeps its exact bytes, including invalid UTF-8.
        let invalid = OsString::from_vec(vec![0xff, 0xfe]);
        assert_eq!(normalize_argument(&invalid, stage_root), vec![0xff, 0xfe]);
    }

    #[test]
    fn an_unbounded_target_argv_is_unsupported_rather_than_compared() {
        let mut values = vec!["--crate-name", "app", "--target", TARGET, "--emit=link"];
        values.extend(std::iter::repeat_n("--cfg=pad", super::ARGUMENT_LIMIT));
        assert_eq!(
            classification(&values).rejection(),
            Some(INPUT_AMBIGUITY_REASON)
        );
    }

    #[test]
    fn injection_round_trips_through_the_private_protocol() {
        let generate = Injection::Generate(PathBuf::from("/run/raw"));
        let parsed = Injection::parse(&generate.encode()).expect("generate round trip");
        assert!(matches!(parsed, Injection::Generate(path) if path == Path::new("/run/raw")));
        assert!(Injection::parse(&Injection::None.encode()).is_ok());
        assert!(Injection::parse(&OsString::from("generate=relative")).is_err());
        assert!(Injection::parse(&OsString::from("unknown=/run")).is_err());
        assert_eq!(
            Injection::Use(PathBuf::from("/run/p")).flag_identifiers(),
            vec!["profile-use", "pgo-warn-missing-function"]
        );
    }

    #[test]
    fn stage_labels_round_trip() {
        for stage in [
            ShimStage::Reference,
            ShimStage::Generate,
            ShimStage::Use,
            ShimStage::Confirmation,
        ] {
            assert_eq!(ShimStage::parse(stage.label()), Some(stage));
        }
        assert_eq!(ShimStage::parse("baseline"), None);
    }

    /// Builds one valid record for the stage under test.
    fn capture_record(class: InvocationClass, injected: &[&str]) -> CaptureRecord {
        CaptureRecord {
            protocol: PROTOCOL.to_owned(),
            normalization: NORMALIZATION_VERSION,
            record_id: String::new(),
            stage: ShimStage::Generate,
            class,
            exact_target: class == InvocationClass::TargetCompile,
            emit: true,
            print: false,
            crate_name: Some("app".to_owned()),
            crate_types: vec!["bin".to_owned()],
            metadata: Some("metadata".to_owned()),
            extra_filename: Some("-extra".to_owned()),
            argument_count: 5,
            pre_digest: "pre".to_owned(),
            post_digest: "post".to_owned(),
            normalized_digest: "normalized".to_owned(),
            argument_digests: vec!["a".to_owned(), "b".to_owned()],
            injected_flags: injected.iter().map(|flag| (*flag).to_owned()).collect(),
            rejection: None,
        }
    }

    fn write_capture(directory: &Path, identity: &str, record: &CaptureRecord) {
        let mut stored = record.clone();
        stored.record_id = identity.to_owned();
        std::fs::write(
            directory.join(format!("{identity}.json")),
            serde_json::to_vec(&stored).expect("serialize record"),
        )
        .expect("write record");
    }

    fn generate_plan(root: &Path) -> InterpositionPlan {
        let directories =
            claim_stage_directories(&root.join("target"), &root.join("captures"), "pgo_generate")
                .expect("stage directories");
        InterpositionPlan {
            stage: ShimStage::Generate,
            tools: ShimTools {
                shim_executable: PathBuf::from("/toolchain/cargo-temper"),
                shim_sha256: "shim".to_owned(),
                real_rustc: PathBuf::from("/toolchain/rustc"),
                real_rustc_version: "rustc identity".to_owned(),
            },
            run_root: root.to_path_buf(),
            target_directory: directories.target,
            capture_directory: directories.capture,
            injection: Injection::Generate(root.join("pgo/raw")),
        }
    }

    #[test]
    fn a_complete_capture_set_aggregates_deterministically() {
        let root = tempfile::tempdir().expect("capture root");
        let root = std::fs::canonicalize(root.path()).expect("canonical root");
        let plan = generate_plan(&root);
        write_capture(
            &plan.capture_directory,
            "target-record",
            &capture_record(InvocationClass::TargetCompile, &["profile-generate"]),
        );
        write_capture(
            &plan.capture_directory,
            "probe-record",
            &capture_record(InvocationClass::Probe, &[]),
        );
        let aggregate = collect(&plan).expect("complete capture set");
        assert_eq!(aggregate.record_count, 2);
        assert_eq!(aggregate.target_compilations, 1);
        assert_eq!(aggregate.probes, 1);
        assert_eq!(aggregate.injected_invocations, 1);
        assert_eq!(aggregate.capture_digest.len(), 64);

        // The aggregate is keyed by content, not by creation order.
        let mirror = tempfile::tempdir().expect("mirror root");
        let mirror = std::fs::canonicalize(mirror.path()).expect("canonical mirror");
        let mirrored = generate_plan(&mirror);
        write_capture(
            &mirrored.capture_directory,
            "probe-record",
            &capture_record(InvocationClass::Probe, &[]),
        );
        write_capture(
            &mirrored.capture_directory,
            "target-record",
            &capture_record(InvocationClass::TargetCompile, &["profile-generate"]),
        );
        assert_eq!(
            collect(&mirrored)
                .expect("mirrored capture set")
                .capture_digest,
            aggregate.capture_digest
        );
    }

    /// One integrity defect: a label, its expected stable reason, and the way
    /// it corrupts a stage capture directory.
    type IntegrityDefect = (&'static str, &'static str, Box<dyn Fn(&Path)>);

    #[test]
    fn capture_validation_fails_closed_on_every_integrity_defect() {
        let cases: Vec<IntegrityDefect> = vec![
            (
                "no target compilation",
                CAPTURE_MISSING_REASON,
                Box::new(|directory: &Path| {
                    write_capture(
                        directory,
                        "probe-only",
                        &capture_record(InvocationClass::Probe, &[]),
                    );
                }),
            ),
            (
                "identity does not match its file name",
                CAPTURE_CORRUPT_REASON,
                Box::new(|directory: &Path| {
                    let mut record =
                        capture_record(InvocationClass::TargetCompile, &["profile-generate"]);
                    record.record_id = "declared-identity".to_owned();
                    std::fs::write(
                        directory.join("stored-identity.json"),
                        serde_json::to_vec(&record).expect("serialize record"),
                    )
                    .expect("write record");
                }),
            ),
            (
                "malformed record",
                CAPTURE_CORRUPT_REASON,
                Box::new(|directory: &Path| {
                    std::fs::write(directory.join("malformed.json"), b"{\"protocol\":")
                        .expect("write malformed record");
                }),
            ),
            (
                "foreign stage",
                CAPTURE_CORRUPT_REASON,
                Box::new(|directory: &Path| {
                    let mut record =
                        capture_record(InvocationClass::TargetCompile, &["profile-generate"]);
                    record.stage = ShimStage::Use;
                    write_capture(directory, "foreign-stage", &record);
                }),
            ),
            (
                "unexpected host injection",
                INJECTION_UNEXPECTED_REASON,
                Box::new(|directory: &Path| {
                    write_capture(
                        directory,
                        "target-record",
                        &capture_record(InvocationClass::TargetCompile, &["profile-generate"]),
                    );
                    write_capture(
                        directory,
                        "host-record",
                        &capture_record(InvocationClass::HostCompile, &["profile-generate"]),
                    );
                }),
            ),
            (
                "missing phase control",
                INJECTION_UNEXPECTED_REASON,
                Box::new(|directory: &Path| {
                    write_capture(
                        directory,
                        "target-record",
                        &capture_record(InvocationClass::TargetCompile, &[]),
                    );
                }),
            ),
            (
                "over-sized record",
                CAPTURE_LIMIT_REASON,
                Box::new(|directory: &Path| {
                    std::fs::write(directory.join("oversized.json"), vec![b' '; 65 * 1024])
                        .expect("write oversized record");
                }),
            ),
            (
                "protocol failure marker",
                PROTOCOL_FAILURE_REASON,
                Box::new(|directory: &Path| {
                    write_capture(
                        directory,
                        "target-record",
                        &capture_record(InvocationClass::TargetCompile, &["profile-generate"]),
                    );
                    std::fs::write(
                        directory.join("protocol-failure-abc.json"),
                        serde_json::to_vec(&super::ProtocolFailureMarker {
                            protocol: PROTOCOL.to_owned(),
                            reason: PROTOCOL_FAILURE_REASON.to_owned(),
                            message: "missing private protocol value".to_owned(),
                        })
                        .expect("serialize marker"),
                    )
                    .expect("write marker");
                }),
            ),
        ];
        for (label, expected, prepare) in cases {
            let root = tempfile::tempdir().expect("capture root");
            let root = std::fs::canonicalize(root.path()).expect("canonical root");
            let plan = generate_plan(&root);
            prepare(&plan.capture_directory);
            let failure = collect(&plan).expect_err(label);
            assert_eq!(failure.reason, expected, "{label}");
        }
    }

    #[test]
    fn more_records_than_the_stage_budget_fail_closed() {
        let root = tempfile::tempdir().expect("capture root");
        let root = std::fs::canonicalize(root.path()).expect("canonical root");
        let plan = generate_plan(&root);
        let record = capture_record(InvocationClass::TargetCompile, &["profile-generate"]);
        for index in 0..=RECORD_LIMIT {
            write_capture(
                &plan.capture_directory,
                &format!("record-{index:06}"),
                &record,
            );
        }
        let failure = collect(&plan).expect_err("record budget");
        assert_eq!(failure.reason, CAPTURE_LIMIT_REASON);
    }

    #[test]
    fn stage_directories_must_be_unique_and_initially_absent() {
        let root = tempfile::tempdir().expect("stage root");
        let target_root = root.path().join("target");
        let capture_root = root.path().join("captures");
        let claimed = claim_stage_directories(&target_root, &capture_root, "pgo_reference")
            .expect("first claim");
        assert!(claimed.target.is_dir());
        assert!(claimed.capture.is_dir());
        assert!(claim_stage_directories(&target_root, &capture_root, "pgo_reference").is_err());
    }
}
