#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CORPUS_PATH: &str = "benchmarks/corpus/v1";
const STATUS_PATH: &str = "tasks/prd-temper-v0.0.2-status.json";
const REFERENCE_DATE: &str = "2026-07-28";
const GATE_ERROR: &str = "Benchmark corpus work is blocked until PGO hardening is DONE.";
const MAX_CASE_SOURCE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_CORPUS_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusManifest {
    corpus_schema_version: u64,
    corpus_id: String,
    changelog_version: String,
    cases: Vec<CorpusCase>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    id: String,
    classification: String,
    workload_class: String,
    source: SourceRecord,
    cargo: CargoRecord,
    license: LicenseRecord,
    inputs: Vec<InputRecord>,
    scenarios: Vec<Scenario>,
    oracle: OracleRecord,
    resource_bounds: ResourceBounds,
    expected_evidence_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    upstream_url: Option<String>,
    revision: String,
    snapshot_path: String,
    snapshot_sha256: String,
    snapshot_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CargoRecord {
    manifest_path: String,
    package: String,
    binary: String,
    lockfile_path: String,
    lockfile_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LicenseRecord {
    spdx_expression: String,
    notice_paths: Vec<String>,
    redistribution_determination_path: String,
    manual_review: ManualReview,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualReview {
    date: String,
    secrets_found: bool,
    personal_data_found: bool,
    automatic_network_operation_found: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputRecord {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    id: String,
    weight: u64,
    arguments: Vec<String>,
    expected_output_sha256: String,
    rationale: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleRecord {
    workload_path: String,
    workload_sha256: String,
    method: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceBounds {
    timeout_seconds: u64,
    output_limit_bytes: u64,
    maximum_source_bytes: u64,
    curation_baseline_median_ms: u64,
}

#[test]
fn corpus_gate_is_open_in_the_authoritative_tracker() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    require_corpus_gate(repository).expect("EP-001 and EP-002 gate should be open");
}

#[test]
fn closed_gate_prevents_setup_before_any_corpus_file_is_created() {
    let temporary = tempfile::tempdir().expect("temporary gate root");
    let root = temporary.path();
    fs::create_dir_all(root.join("tasks")).expect("temporary tasks directory");
    let mut status = repository_status();
    status["epics"][0]["status"] = json!("IN_REVIEW");
    fs::write(
        root.join(STATUS_PATH),
        serde_json::to_vec_pretty(&status).expect("serialize temporary status"),
    )
    .expect("write temporary status");

    let error = load_repository_manifest(root).expect_err("closed gate must reject setup");
    assert_eq!(error, GATE_ERROR);
    assert!(!root.join(CORPUS_PATH).exists());
}

#[test]
fn runner_refuses_a_closed_gate_even_when_corpus_files_exist() {
    let temporary = tempfile::tempdir().expect("temporary copied worktree");
    let root = temporary.path();
    fs::create_dir_all(root.join("tasks")).expect("temporary tasks directory");
    fs::create_dir_all(root.join(CORPUS_PATH)).expect("copied corpus directory");
    fs::write(root.join(CORPUS_PATH).join("manifest.json"), b"{}")
        .expect("copied manifest placeholder");
    let mut status = repository_status();
    status["epics"][1]["status"] = json!("TODO");
    fs::write(
        root.join(STATUS_PATH),
        serde_json::to_vec_pretty(&status).expect("serialize temporary status"),
    )
    .expect("write temporary status");

    let error = load_repository_manifest(root).expect_err("closed gate must reject execution");
    assert_eq!(error, GATE_ERROR);
}

#[test]
fn manifest_and_checked_in_corpus_satisfy_schema_one() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = load_repository_manifest(repository).expect("valid checked-in corpus");
    assert_eq!(manifest.cases.len(), 4);
}

#[test]
fn manifest_rejects_duplicate_ids_and_schema_or_field_errors() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let original = repository_manifest();

    let mut duplicate = original.clone();
    let duplicate_case = duplicate["cases"][0].clone();
    duplicate["cases"]
        .as_array_mut()
        .expect("cases array")
        .push(duplicate_case);
    assert_error_contains(repository, duplicate, "duplicate corpus case ID");

    let mut unknown_schema = original.clone();
    unknown_schema["corpus_schema_version"] = json!(2);
    assert_error_contains(
        repository,
        unknown_schema,
        "unsupported corpus schema version",
    );

    let mut missing_field = original.clone();
    missing_field["cases"][0]
        .as_object_mut()
        .expect("case object")
        .remove("cargo");
    assert_error_contains(repository, missing_field, "missing field `cargo`");

    let mut invalid_hash = original.clone();
    invalid_hash["cases"][0]["source"]["snapshot_sha256"] = json!("not-a-sha256");
    assert_error_contains(repository, invalid_hash, "invalid snapshot SHA-256");

    let mut zero_weight = original.clone();
    zero_weight["cases"][0]["scenarios"][0]["weight"] = json!(0);
    assert_error_contains(
        repository,
        zero_weight,
        "scenario weight must be from 1 through 100",
    );

    let mut escaped_path = original.clone();
    escaped_path["cases"][0]["source"]["snapshot_path"] = json!("../outside");
    assert_error_contains(
        repository,
        escaped_path,
        "path must stay within its case root",
    );

    let mut escaped_id = original;
    escaped_id["cases"][0]["id"] = json!("/tmp/escape");
    assert_error_contains(repository, escaped_id, "invalid corpus case ID");
}

#[test]
fn contained_outputs_reject_symlinks_and_parent_components() {
    let temporary = tempfile::tempdir().expect("contained output root");
    let root = temporary.path();
    let safe = create_contained_directory(root, Path::new("safe/nested"))
        .expect("create safe contained output");
    assert!(safe.starts_with(fs::canonicalize(root).expect("canonical output root")));

    fs::create_dir(root.join("external")).expect("symlink target directory");
    symlink(root.join("external"), root.join("link")).expect("output symlink");
    assert!(create_contained_directory(root, Path::new("link/nested")).is_err());
    assert!(create_contained_directory(root, Path::new("../escape")).is_err());
}

#[test]
fn post_run_source_enumeration_rejects_added_files_outside_temper_evidence() {
    let temporary = tempfile::tempdir().expect("source fingerprint root");
    let source = temporary.path();
    fs::write(source.join("Cargo.toml"), b"[package]\nname='fixture'\n").expect("source manifest");
    let mut before = Vec::new();
    collect_tree_paths(source, source, &mut before).expect("initial source paths");
    before.sort();

    let generated = source.join(".temper");
    fs::create_dir(&generated).expect("generated evidence directory");
    fs::write(generated.join("run.json"), b"{}").expect("generated evidence");
    fs::write(source.join("added.rs"), b"fn added() {}\n").expect("unexpected source file");
    let mut after = Vec::new();
    collect_tree_paths_except(source, source, Some(&generated), &mut after)
        .expect("post-run source paths");
    after.sort();

    assert_ne!(after, before);
}

#[test]
#[ignore = "runs twelve full Temper searches and writes immutable reference evidence"]
fn collect_corpus_v1_reference_evidence() -> Result<(), String> {
    assert_eq!(
        std::env::var("TEMPER_CORPUS_COLLECT").as_deref(),
        Ok("1"),
        "TEMPER_CORPUS_COLLECT=1 is required"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    require_corpus_gate(repository)?;

    let corpus_root = repository.join(CORPUS_PATH);
    let results_relative = Path::new("results/reference").join(REFERENCE_DATE);
    let results_root = corpus_root.join(&results_relative);
    assert!(
        fs::symlink_metadata(&results_root).is_err(),
        "refusing to overwrite immutable corpus-v1 reference evidence"
    );
    let results_root = create_contained_directory(&corpus_root, &results_relative)?;

    let manifest_value = repository_manifest();
    let manifest = match validate_manifest_value(repository, manifest_value) {
        Ok(manifest) => manifest,
        Err(error) => {
            write_json(
                &results_root.join("failure.json"),
                &json!({"stage": "manifest_validation", "error": error}),
            );
            return Err("corpus manifest invalid; diagnostics retained".to_owned());
        }
    };
    let manifest_sha256 = sha256_file(&corpus_root.join("manifest.json"))
        .map_err(|error| format!("hash corpus manifest before collection: {error}"))?;
    let identity = ReferenceIdentity::capture(repository);
    let mut completed_runs = Vec::new();

    for case in &manifest.cases {
        for run_number in 1..=3 {
            let run_directory = create_contained_directory(
                &results_root,
                &Path::new(&case.id).join(format!("run-{run_number}")),
            )?;
            match collect_one_run(&corpus_root, case, run_number, &run_directory, &identity) {
                Ok(summary) => completed_runs.push(summary),
                Err(error) => {
                    write_json(
                        &run_directory.join("failure.json"),
                        &json!({"case_id": case.id, "run_number": run_number, "error": error}),
                    );
                    return Err(format!(
                        "{} run {} invalid; diagnostics retained in {}",
                        case.id,
                        run_number,
                        run_directory.display()
                    ));
                }
            }
        }
    }

    assert_eq!(completed_runs.len(), 12);
    let manifest_after = sha256_file(&corpus_root.join("manifest.json"))
        .map_err(|error| format!("hash corpus manifest after collection: {error}"))?;
    assert_eq!(
        manifest_after, manifest_sha256,
        "corpus manifest changed during collection"
    );
    write_json(
        &results_root.join("summary.json"),
        &json!({
            "reference_date": REFERENCE_DATE,
            "corpus_id": manifest.corpus_id,
            "changelog_version": manifest.changelog_version,
            "manifest_sha256": manifest_sha256,
            "temper": identity,
            "runs": completed_runs,
            "representative_case_count": manifest
                .cases
                .iter()
                .filter(|case| case.classification == "real")
                .count(),
            "synthetic_control_count": manifest
                .cases
                .iter()
                .filter(|case| case.classification == "synthetic")
                .count(),
            "aggregate_cross_application_score": Value::Null,
        }),
    );
    Ok(())
}

fn repository_status() -> Value {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    serde_json::from_slice(
        &fs::read(repository.join(STATUS_PATH)).expect("read repository status tracker"),
    )
    .expect("parse repository status tracker")
}

fn repository_manifest() -> Value {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    serde_json::from_slice(
        &fs::read(repository.join(CORPUS_PATH).join("manifest.json"))
            .expect("read corpus manifest"),
    )
    .expect("parse corpus manifest")
}

fn assert_error_contains(repository: &Path, value: Value, expected: &str) {
    let error = validate_manifest_value(repository, value).expect_err("manifest should reject");
    assert!(
        error.contains(expected),
        "expected {expected:?} in {error:?}"
    );
}

fn load_repository_manifest(repository: &Path) -> Result<CorpusManifest, String> {
    require_corpus_gate(repository)?;
    let bytes = fs::read(repository.join(CORPUS_PATH).join("manifest.json"))
        .map_err(|error| format!("read corpus manifest: {error}"))?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse corpus manifest: {error}"))?;
    validate_manifest_value(repository, value)
}

fn require_corpus_gate(repository: &Path) -> Result<(), String> {
    let bytes = fs::read(repository.join(STATUS_PATH)).map_err(|_| GATE_ERROR.to_owned())?;
    let status: Value = serde_json::from_slice(&bytes).map_err(|_| GATE_ERROR.to_owned())?;
    let epics = status["epics"]
        .as_array()
        .ok_or_else(|| GATE_ERROR.to_owned())?;
    for epic_id in ["EP-001", "EP-002"] {
        let done = epics
            .iter()
            .any(|epic| epic["id"] == epic_id && epic["status"] == "DONE");
        if !done {
            return Err(GATE_ERROR.to_owned());
        }
    }
    let stories = status["stories"]
        .as_array()
        .ok_or_else(|| GATE_ERROR.to_owned())?;
    for number in 1..=9 {
        let story_id = format!("US-{number:03}");
        let done = stories
            .iter()
            .any(|story| story["id"] == story_id && story["status"] == "DONE");
        if !done {
            return Err(GATE_ERROR.to_owned());
        }
    }
    Ok(())
}

fn validate_manifest_value(repository: &Path, value: Value) -> Result<CorpusManifest, String> {
    let manifest: CorpusManifest =
        serde_json::from_value(value).map_err(|error| format!("manifest schema: {error}"))?;
    if manifest.corpus_schema_version != 1 {
        return Err("unsupported corpus schema version".to_owned());
    }
    if manifest.corpus_id.trim().is_empty() || manifest.changelog_version.trim().is_empty() {
        return Err("corpus ID and changelog version are required".to_owned());
    }
    let corpus_root = repository.join(CORPUS_PATH);
    let mut ids = HashSet::new();
    let mut real_classes = HashSet::new();
    let mut real_count = 0_u64;
    let mut synthetic_count = 0_u64;
    let mut corpus_bytes = 0_u64;

    for case in &manifest.cases {
        if !is_corpus_id(&case.id) {
            return Err(format!("invalid corpus case ID: {}", case.id));
        }
        if !ids.insert(case.id.clone()) {
            return Err(format!("duplicate corpus case ID: {}", case.id));
        }
        validate_case_paths(&corpus_root, case)?;
        validate_sha256(&case.source.snapshot_sha256, "snapshot SHA-256")?;
        validate_sha256(&case.cargo.lockfile_sha256, "lockfile SHA-256")?;
        validate_sha256(&case.oracle.workload_sha256, "workload SHA-256")?;
        let source_path = corpus_root.join(&case.source.snapshot_path);
        let (snapshot_hash, snapshot_bytes) = sha256_tree(&source_path)?;
        if snapshot_hash != case.source.snapshot_sha256 {
            return Err(format!("{} source snapshot checksum mismatch", case.id));
        }
        if snapshot_bytes != case.source.snapshot_bytes {
            return Err(format!("{} source snapshot byte length mismatch", case.id));
        }
        if snapshot_bytes > MAX_CASE_SOURCE_BYTES
            || snapshot_bytes > case.resource_bounds.maximum_source_bytes
        {
            return Err(format!("{} source snapshot exceeds 25 MiB", case.id));
        }
        corpus_bytes = corpus_bytes
            .checked_add(snapshot_bytes)
            .ok_or_else(|| "corpus byte count overflow".to_owned())?;

        validate_sha256_file(
            &corpus_root.join(&case.cargo.lockfile_path),
            &case.cargo.lockfile_sha256,
            "lockfile",
        )?;
        validate_sha256_file(
            &corpus_root.join(&case.oracle.workload_path),
            &case.oracle.workload_sha256,
            "workload",
        )?;
        let workload_mode = fs::metadata(corpus_root.join(&case.oracle.workload_path))
            .map_err(|error| format!("{} workload metadata: {error}", case.id))?
            .permissions()
            .mode();
        if workload_mode & 0o111 == 0 {
            return Err(format!("{} workload is not executable", case.id));
        }
        if case.oracle.method != "exit-status-and-output-sha256" {
            return Err(format!("{} has an unsupported oracle method", case.id));
        }

        let mut input_paths = HashSet::new();
        for input in &case.inputs {
            if !input_paths.insert(input.path.clone()) {
                return Err(format!("{} has a duplicate input path", case.id));
            }
            validate_sha256(&input.sha256, "input SHA-256")?;
            let input_path = corpus_root.join(&input.path);
            validate_sha256_file(&input_path, &input.sha256, "input")?;
            if !input_path.starts_with(&source_path) {
                corpus_bytes = corpus_bytes
                    .checked_add(
                        fs::metadata(&input_path)
                            .map_err(|error| format!("input metadata: {error}"))?
                            .len(),
                    )
                    .ok_or_else(|| "corpus byte count overflow".to_owned())?;
            }
        }

        if case.scenarios.is_empty() {
            return Err(format!("{} has no scenarios", case.id));
        }
        let mut scenario_ids = HashSet::new();
        let mut total_weight = 0_u64;
        for scenario in &case.scenarios {
            if scenario.id.is_empty() || !scenario_ids.insert(scenario.id.clone()) {
                return Err(format!("{} has a duplicate scenario ID", case.id));
            }
            if !(1..=100).contains(&scenario.weight) {
                return Err("scenario weight must be from 1 through 100".to_owned());
            }
            total_weight += scenario.weight;
            validate_sha256(&scenario.expected_output_sha256, "scenario output SHA-256")?;
            if scenario.arguments.is_empty() || scenario.rationale.trim().is_empty() {
                return Err(format!("{} scenario is incomplete", case.id));
            }
        }
        if total_weight != 100 {
            return Err(format!("{} scenario weights must total 100", case.id));
        }
        if !(100..=10_000).contains(&case.resource_bounds.curation_baseline_median_ms) {
            return Err(format!(
                "{} curation median is outside 100 ms to 10 s",
                case.id
            ));
        }
        if case.resource_bounds.timeout_seconds == 0
            || case.resource_bounds.output_limit_bytes == 0
            || case.resource_bounds.maximum_source_bytes != MAX_CASE_SOURCE_BYTES
        {
            return Err(format!("{} resource bounds are invalid", case.id));
        }
        if !is_cargo_name(&case.cargo.package) || !is_cargo_name(&case.cargo.binary) {
            return Err(format!("{} Cargo selection is incomplete", case.id));
        }
        if case.license.spdx_expression.trim().is_empty()
            || case.license.notice_paths.is_empty()
            || case.license.manual_review.date != REFERENCE_DATE
            || case.license.manual_review.secrets_found
            || case.license.manual_review.personal_data_found
            || case.license.manual_review.automatic_network_operation_found
        {
            return Err(format!(
                "{} redistribution or manual review is incomplete",
                case.id
            ));
        }
        if case.expected_evidence_paths.len() != 3 {
            return Err(format!("{} must declare three evidence paths", case.id));
        }
        for (index, path) in case.expected_evidence_paths.iter().enumerate() {
            let expected = format!(
                "results/reference/{REFERENCE_DATE}/{}/run-{}/run.json",
                case.id,
                index + 1
            );
            if path != &expected {
                return Err(format!(
                    "{} evidence path is not versioned as expected",
                    case.id
                ));
            }
        }

        match case.classification.as_str() {
            "real" => {
                real_count += 1;
                if case
                    .source
                    .upstream_url
                    .as_deref()
                    .is_none_or(str::is_empty)
                    || !is_lower_hex(&case.source.revision, 40)
                    || case.scenarios.len() < 2
                {
                    return Err(format!("{} real-source provenance is incomplete", case.id));
                }
                real_classes.insert(case.workload_class.as_str());
            }
            "synthetic" => {
                synthetic_count += 1;
                if case.source.upstream_url.is_some() {
                    return Err("synthetic control must not claim an upstream".to_owned());
                }
            }
            _ => return Err(format!("{} has an unknown classification", case.id)),
        }
    }

    if real_count != 3
        || synthetic_count != 1
        || !["compute-heavy", "parsing-transformation", "streaming"]
            .iter()
            .all(|class| real_classes.contains(class))
    {
        return Err("corpus requires three real classes and one synthetic control".to_owned());
    }
    if corpus_bytes > MAX_CORPUS_BYTES {
        return Err("corpus source plus inputs exceeds 100 MiB".to_owned());
    }
    Ok(manifest)
}

fn validate_case_paths(corpus_root: &Path, case: &CorpusCase) -> Result<(), String> {
    let case_prefix = Path::new("cases").join(&case.id);
    for path in [
        &case.source.snapshot_path,
        &case.cargo.manifest_path,
        &case.cargo.lockfile_path,
        &case.license.redistribution_determination_path,
        &case.oracle.workload_path,
    ]
    .into_iter()
    .chain(case.license.notice_paths.iter())
    .chain(case.inputs.iter().map(|input| &input.path))
    {
        if !Path::new(path).starts_with(&case_prefix) {
            return Err(format!(
                "{} path must stay within its case root: {path}",
                case.id
            ));
        }
        validate_relative_path(corpus_root, path, true)?;
    }
    for path in &case.expected_evidence_paths {
        validate_relative_path(corpus_root, path, false)?;
    }
    Ok(())
}

fn validate_relative_path(
    corpus_root: &Path,
    relative: &str,
    must_exist: bool,
) -> Result<(), String> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("path must stay within corpus root: {relative}"));
    }
    if must_exist {
        let canonical_root = fs::canonicalize(corpus_root)
            .map_err(|error| format!("canonicalize corpus root: {error}"))?;
        let canonical = fs::canonicalize(corpus_root.join(path))
            .map_err(|error| format!("corpus path {relative}: {error}"))?;
        if !canonical.starts_with(canonical_root) {
            return Err(format!("path must stay within corpus root: {relative}"));
        }
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if is_lower_hex(value, 64) {
        Ok(())
    } else {
        Err(format!("invalid {label}"))
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_corpus_id(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_cargo_name(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn validate_sha256_file(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let actual = sha256_file(path).map_err(|error| format!("hash {label}: {error}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} checksum mismatch: {}", path.display()))
    }
}

fn sha256_tree(root: &Path) -> Result<(String, u64), String> {
    let mut paths = Vec::new();
    collect_tree_paths(root, root, &mut paths)?;
    paths.sort();
    sha256_tree_paths(root, &paths)
}

fn sha256_tree_paths(root: &Path, paths: &[PathBuf]) -> Result<(String, u64), String> {
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    for relative in paths {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("source file metadata: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err(format!("source file changed type: {}", path.display()));
        }
        let file_hash = sha256_file(&path).map_err(|error| error.to_string())?;
        let length = metadata.len();
        bytes = bytes
            .checked_add(length)
            .ok_or_else(|| "snapshot byte count overflow".to_owned())?;
        digest.update(file_hash.as_bytes());
        digest.update(b"  ./");
        digest.update(relative.as_os_str().as_encoded_bytes());
        digest.update(b"\n");
    }
    Ok((format!("{:x}", digest.finalize()), bytes))
}

fn collect_tree_paths(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), String> {
    collect_tree_paths_except(root, directory, None, paths)
}

fn collect_tree_paths_except(
    root: &Path,
    directory: &Path,
    excluded_directory: Option<&Path>,
    paths: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("read snapshot directory: {error}"))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| format!("read snapshot entries: {error}"))?;
    for entry in entries {
        if excluded_directory.is_some_and(|excluded| entry.path() == excluded) {
            if !entry
                .file_type()
                .map_err(|error| format!("excluded file type: {error}"))?
                .is_dir()
            {
                return Err("excluded generated evidence is not a directory".to_owned());
            }
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("snapshot file type: {error}"))?;
        if file_type.is_symlink() {
            return Err(format!(
                "source snapshot contains a symlink: {}",
                entry.path().display()
            ));
        }
        if file_type.is_dir() {
            collect_tree_paths_except(root, &entry.path(), excluded_directory, paths)?;
        } else if file_type.is_file() {
            paths.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| format!("snapshot relative path: {error}"))?
                    .to_path_buf(),
            );
        } else {
            return Err(format!(
                "source snapshot contains a non-regular file: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(serde::Serialize)]
struct ReferenceIdentity {
    temper_commit: String,
    temper_worktree_dirty: bool,
    temper_implementation_sha256: String,
    kernel: String,
    cpu: String,
    logical_cores: usize,
    cargo: String,
    rustc: String,
    llvm: String,
    llvm_profdata_path: String,
}

impl ReferenceIdentity {
    fn capture(repository: &Path) -> Self {
        let target_libdir = command_text(
            repository,
            "rustc",
            &[
                "--print",
                "target-libdir",
                "--target",
                "x86_64-unknown-linux-gnu",
            ],
        );
        let llvm_profdata = Path::new(target_libdir.trim())
            .parent()
            .expect("target libdir parent")
            .join("bin/llvm-profdata");
        Self {
            temper_commit: command_text(repository, "git", &["rev-parse", "HEAD"]),
            temper_worktree_dirty: !command_text(repository, "git", &["status", "--porcelain"])
                .is_empty(),
            temper_implementation_sha256: temper_implementation_sha256(repository),
            kernel: command_text(repository, "uname", &["-srmo"]),
            cpu: cpu_model(),
            logical_cores: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(0),
            cargo: command_text(repository, "cargo", &["-Vv"]),
            rustc: command_text(repository, "rustc", &["-Vv"]),
            llvm: command_text(
                repository,
                llvm_profdata.to_str().expect("LLVM path is UTF-8"),
                &["--version"],
            ),
            llvm_profdata_path: llvm_profdata.display().to_string(),
        }
    }
}

fn collect_one_run(
    corpus_root: &Path,
    case: &CorpusCase,
    run_number: u64,
    output_directory: &Path,
    identity: &ReferenceIdentity,
) -> Result<Value, String> {
    let staging = tempfile::tempdir().map_err(|error| format!("create staging root: {error}"))?;
    let staged_case = staging.path().join(&case.id);
    let cases_root =
        fs::canonicalize(corpus_root.join("cases")).map_err(|error| error.to_string())?;
    let source_case = corpus_root.join("cases").join(&case.id);
    let source_case_type = fs::symlink_metadata(&source_case).map_err(|error| error.to_string())?;
    if source_case_type.file_type().is_symlink() || !source_case_type.is_dir() {
        return Err(format!(
            "invalid source case directory: {}",
            source_case.display()
        ));
    }
    let canonical_source_case =
        fs::canonicalize(&source_case).map_err(|error| error.to_string())?;
    if !canonical_source_case.starts_with(&cases_root) {
        return Err("source case escaped corpus cases root".to_owned());
    }
    copy_directory(&canonical_source_case, &staged_case)?;
    initialize_git(&staged_case)?;

    let source = staged_case_path(&staged_case, case, &case.source.snapshot_path)?;
    let cargo_manifest = staged_case_path(&staged_case, case, &case.cargo.manifest_path)?;
    let lockfile = staged_case_path(&staged_case, case, &case.cargo.lockfile_path)?;
    let workload = staged_case_path(&staged_case, case, &case.oracle.workload_path)?;
    let mut source_paths = Vec::new();
    collect_tree_paths(&source, &source, &mut source_paths)?;
    source_paths.sort();
    let source_before = sha256_tree_paths(&source, &source_paths)?.0;
    let lockfile_before = sha256_file(&lockfile).map_err(|error| error.to_string())?;
    for input in &case.inputs {
        let input_path = staged_case_path(&staged_case, case, &input.path)?;
        validate_sha256_file(&input_path, &input.sha256, "staged input")?;
    }

    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-temper"));
    command
        .current_dir(&staged_case)
        .args(["temper", "optimize", "--json", "--manifest-path"])
        .arg(&cargo_manifest)
        .args([
            "--package",
            &case.cargo.package,
            "--bin",
            &case.cargo.binary,
        ])
        .args([
            "--timeout",
            &case.resource_bounds.timeout_seconds.to_string(),
        ])
        .arg("--")
        .arg(&workload)
        .env("CARGO_NET_OFFLINE", "true")
        .env("NO_COLOR", "1")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    let output = command
        .output()
        .map_err(|error| format!("launch Temper: {error}"))?;
    fs::write(output_directory.join("temper.stdout"), &output.stdout)
        .map_err(|error| format!("retain Temper stdout: {error}"))?;
    fs::write(output_directory.join("temper.stderr"), &output.stderr)
        .map_err(|error| format!("retain Temper stderr: {error}"))?;
    if !output.status.success() {
        copy_any_run_json(&staged_case, output_directory)?;
        return Err(format!(
            "Temper exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let run: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse Temper JSON output: {error}"))?;
    if run["schema_version"] != 3 {
        return Err("Temper did not emit schema-v3 evidence".to_owned());
    }
    let run_id = run["run_id"]
        .as_str()
        .ok_or_else(|| "Temper output has no run ID".to_owned())?;
    let workspace_root = run["target"]["workspace_root"]
        .as_str()
        .ok_or_else(|| "Temper output has no workspace root".to_owned())?;
    let generated_evidence = fs::canonicalize(Path::new(workspace_root).join(".temper"))
        .map_err(|error| format!("canonicalize generated Temper evidence: {error}"))?;
    if !generated_evidence.starts_with(&source) {
        return Err("generated Temper evidence escaped source root".to_owned());
    }
    let run_json = Path::new(workspace_root)
        .join(".temper/runs")
        .join(run_id)
        .join("run.json");
    if !run_json.is_file() {
        return Err(format!("raw run manifest missing: {}", run_json.display()));
    }
    fs::copy(&run_json, output_directory.join("run.json"))
        .map_err(|error| format!("retain raw run manifest: {error}"))?;

    let mut source_paths_after = Vec::new();
    collect_tree_paths_except(
        &source,
        &source,
        Some(&generated_evidence),
        &mut source_paths_after,
    )?;
    source_paths_after.sort();
    if source_paths_after != source_paths {
        return Err("source file set changed during run".to_owned());
    }
    let source_after = sha256_tree_paths(&source, &source_paths_after)?.0;
    let lockfile_after = sha256_file(&lockfile).map_err(|error| error.to_string())?;
    if source_after != source_before || lockfile_after != lockfile_before {
        return Err("source snapshot or lockfile changed during run".to_owned());
    }
    for input in &case.inputs {
        let input_path = staged_case_path(&staged_case, case, &input.path)?;
        validate_sha256_file(&input_path, &input.sha256, "post-run input")?;
    }
    let git_diff = run_command(&staged_case, "git", &["diff", "--exit-code"])?;
    if !git_diff.status.success() {
        return Err("tracked case inputs changed during run".to_owned());
    }

    let audit = recheck_run_checksums(&run)?;
    write_json(&output_directory.join("audit.json"), &audit);
    let metadata = json!({
        "reference_date": REFERENCE_DATE,
        "case_id": case.id,
        "run_number": run_number,
        "corpus_version": "1.0.0",
        "temper": identity,
        "source_revision": case.source.revision,
        "source_sha256": case.source.snapshot_sha256,
        "input_sha256": case
            .inputs
            .iter()
            .map(|input| json!({"path": input.path, "sha256": input.sha256}))
            .collect::<Vec<_>>(),
        "workload_argv": [case.oracle.workload_path.clone()],
        "raw_run_identifier": run_id,
        "schema_version": run["schema_version"],
        "status": run["status"],
        "selected_candidate": run["selected_candidate"],
        "baseline_screening_median_ns": run["baseline_measurement"]["median_duration_ns"],
        "confirmation_median_ratio": run["confirmation"]["measurement"]["median_ratio"],
        "confidence_interval_95": run["confirmation"]["measurement"]["confidence_interval_95"],
        "decision": run["confirmation"]["measurement"]["outcome"],
    });
    write_json(&output_directory.join("metadata.json"), &metadata);
    Ok(metadata)
}

fn staged_case_path(
    staged_case: &Path,
    case: &CorpusCase,
    corpus_relative: &str,
) -> Result<PathBuf, String> {
    let case_prefix = Path::new("cases").join(&case.id);
    let relative = Path::new(corpus_relative)
        .strip_prefix(&case_prefix)
        .map_err(|_| format!("case path is outside its case root: {corpus_relative}"))?;
    let staged = staged_case.join(relative);
    let canonical_case = fs::canonicalize(staged_case)
        .map_err(|error| format!("canonicalize staged case: {error}"))?;
    let canonical =
        fs::canonicalize(&staged).map_err(|error| format!("canonicalize staged path: {error}"))?;
    if !canonical.starts_with(canonical_case) {
        return Err(format!("staged path escaped case root: {corpus_relative}"));
    }
    Ok(canonical)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("create staging directory: {error}"))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("read corpus case: {error}"))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| format!("read corpus entries: {error}"))?
    {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("corpus file type: {error}"))?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(format!(
                "corpus case contains symlink: {}",
                entry.path().display()
            ));
        }
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target).map_err(|error| format!("copy corpus file: {error}"))?;
        } else {
            return Err(format!(
                "corpus case contains non-regular file: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn create_contained_directory(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("output path must stay within its root".to_owned());
    }
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("canonicalize output root: {error}"))?;
    let mut current = canonical_root.clone();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("output path must contain normal components".to_owned());
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "output component is not a directory: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|error| format!("create output directory: {error}"))?;
            }
            Err(error) => return Err(format!("inspect output directory: {error}")),
        }
        let canonical = fs::canonicalize(&current)
            .map_err(|error| format!("canonicalize output directory: {error}"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err("output directory escaped its root".to_owned());
        }
        current = canonical;
    }
    Ok(current)
}

fn initialize_git(root: &Path) -> Result<(), String> {
    for arguments in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Temper Corpus"],
        vec!["config", "user.email", "temper-corpus@example.invalid"],
        vec!["add", "."],
        vec!["commit", "--quiet", "-m", "corpus case"],
    ] {
        let output = run_command(root, "git", &arguments)?;
        if !output.status.success() {
            return Err(format!(
                "git {:?} failed: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

fn copy_any_run_json(staged_case: &Path, output_directory: &Path) -> Result<(), String> {
    let mut manifests = Vec::new();
    collect_named_files(staged_case, OsStr::new("run.json"), &mut manifests)?;
    manifests.sort();
    if let Some(manifest) = manifests.last() {
        fs::copy(manifest, output_directory.join("run.json"))
            .map_err(|error| format!("retain failed run manifest: {error}"))?;
    }
    Ok(())
}

fn collect_named_files(
    directory: &Path,
    name: &OsStr,
    matches: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("read staged directory: {error}"))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| format!("read staged entries: {error}"))?
    {
        let file_type = entry
            .file_type()
            .map_err(|error| format!("staged file type: {error}"))?;
        if file_type.is_dir() {
            collect_named_files(&entry.path(), name, matches)?;
        } else if file_type.is_file() && entry.file_name() == name {
            matches.push(entry.path());
        }
    }
    Ok(())
}

fn recheck_run_checksums(run: &Value) -> Result<Value, String> {
    let mut artifact_count = 0_u64;
    let mut raw_profile_count = 0_u64;
    let mut merged_profile_count = 0_u64;
    let mut mismatch = false;
    walk_records(run, &mut |object| {
        if let (Some(path), Some(expected)) = (
            object.get("executable_path").and_then(Value::as_str),
            object.get("sha256").and_then(Value::as_str),
        ) {
            if sha256_file(Path::new(path)).ok().as_deref() == Some(expected) {
                artifact_count += 1;
            } else {
                mismatch = true;
            }
        }
        if let (Some(path), Some(expected)) = (
            object.get("path").and_then(Value::as_str),
            object.get("sha256").and_then(Value::as_str),
        ) && Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "profraw")
        {
            if sha256_file(Path::new(path)).ok().as_deref() == Some(expected) {
                raw_profile_count += 1;
            } else {
                mismatch = true;
            }
        }
        if let (Some(path), Some(expected)) = (
            object.get("profdata_path").and_then(Value::as_str),
            object.get("profdata_sha256").and_then(Value::as_str),
        ) {
            if sha256_file(Path::new(path)).ok().as_deref() == Some(expected) {
                merged_profile_count += 1;
            } else {
                mismatch = true;
            }
        }
    });
    if mismatch || artifact_count < 3 {
        return Err("artifact or profile checksum recheck failed".to_owned());
    }
    Ok(json!({
        "artifact_checksums_rechecked": artifact_count,
        "raw_profile_checksums_rechecked": raw_profile_count,
        "merged_profile_checksums_rechecked": merged_profile_count,
        "all_available_rechecks_matched": true,
    }))
}

fn walk_records(value: &Value, inspect: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match value {
        Value::Object(object) => {
            inspect(object);
            for child in object.values() {
                walk_records(child, inspect);
            }
        }
        Value::Array(array) => {
            for child in array {
                walk_records(child, inspect);
            }
        }
        _ => {}
    }
}

fn temper_implementation_sha256(repository: &Path) -> String {
    let mut paths = vec![PathBuf::from("Cargo.lock"), PathBuf::from("Cargo.toml")];
    collect_regular_paths(repository, &repository.join("src"), &mut paths);
    paths.sort();
    let mut digest = Sha256::new();
    for path in paths {
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update(fs::read(repository.join(path)).expect("read Temper implementation"));
    }
    format!("{:x}", digest.finalize())
}

fn collect_regular_paths(repository: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .expect("read implementation directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("implementation entries")
    {
        if entry
            .file_type()
            .expect("implementation file type")
            .is_dir()
        {
            collect_regular_paths(repository, &entry.path(), paths);
        } else {
            paths.push(
                entry
                    .path()
                    .strip_prefix(repository)
                    .expect("repository-relative implementation path")
                    .to_path_buf(),
            );
        }
    }
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|line| line.split_once(':'))
                    .map(|(_, value)| value.trim().to_owned())
            })
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn command_text(directory: &Path, program: &str, arguments: &[&str]) -> String {
    let output = run_command(directory, program, arguments).expect("run identity command");
    assert!(
        output.status.success(),
        "{program} identity failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("identity output is UTF-8")
        .trim()
        .to_owned()
}

fn run_command(directory: &Path, program: &str, arguments: &[&str]) -> Result<Output, String> {
    Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()
        .map_err(|error| format!("run {program}: {error}"))
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize evidence JSON"),
    )
    .expect("write evidence JSON");
}
