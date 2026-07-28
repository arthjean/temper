#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

#[test]
fn completed_schema_v1_fixture_satisfies_the_checked_in_json_schema() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let schema: Value = serde_json::from_slice(
        &fs::read(repository.join("docs/schema-v1.json")).expect("read schema v1"),
    )
    .expect("parse schema v1");
    let manifest: Value = serde_json::from_slice(
        &fs::read(repository.join("tests/fixtures/schema-v1/run.json"))
            .expect("read completed schema-v1 fixture"),
    )
    .expect("parse completed schema-v1 fixture");

    validate(&manifest, &schema, &schema, "$").expect("completed schema-v1 fixture must validate");
    validate_terminal_invariants(&manifest).expect("terminal schema-v1 invariants");
    for variant in terminal_variants(&manifest) {
        validate(&variant, &schema, &schema, "$")
            .expect("every terminal schema-v1 outcome must validate");
        validate_terminal_invariants(&variant).expect("terminal schema-v1 invariants");
    }

    let mut wrong_version = manifest;
    wrong_version["schema_version"] = Value::from(2);
    assert!(
        validate(&wrong_version, &schema, &schema, "$")
            .expect_err("schema 2 must not validate as schema 1")
            .contains("const")
    );
}

fn validate(instance: &Value, schema: &Value, root: &Value, path: &str) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("{path}: only local schema references are supported"))?;
        let referenced = root
            .pointer(pointer)
            .ok_or_else(|| format!("{path}: unresolved schema reference {reference}"))?;
        return validate(instance, referenced, root, path);
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = branches
            .iter()
            .filter(|branch| validate(instance, branch, root, path).is_ok())
            .count();
        return if matches == 1 {
            Ok(())
        } else {
            Err(format!(
                "{path}: expected exactly one oneOf branch, got {matches}"
            ))
        };
    }
    if let Some(expected) = schema.get("const")
        && instance != expected
    {
        return Err(format!("{path}: value does not match const"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(instance)
    {
        return Err(format!("{path}: value is outside enum"));
    }
    if let Some(types) = schema.get("type") {
        let matches = match types {
            Value::String(name) => matches_type(instance, name),
            Value::Array(names) => names
                .iter()
                .filter_map(Value::as_str)
                .any(|name| matches_type(instance, name)),
            _ => false,
        };
        if !matches {
            return Err(format!("{path}: value has the wrong type"));
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let object = instance
            .as_object()
            .ok_or_else(|| format!("{path}: required fields need an object"))?;
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(format!("{path}: missing required field {field}"));
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && let Some(object) = instance.as_object()
    {
        for (name, property_schema) in properties {
            if let Some(value) = object.get(name) {
                validate(value, property_schema, root, &format!("{path}.{name}"))?;
            }
        }
    }
    if let Some(item_schema) = schema.get("items")
        && let Some(items) = instance.as_array()
    {
        for (index, item) in items.iter().enumerate() {
            validate(item, item_schema, root, &format!("{path}[{index}]"))?;
        }
    }
    if let Some(items) = instance.as_array() {
        if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
            && u64::try_from(items.len()).is_ok_and(|length| length < minimum)
        {
            return Err(format!("{path}: array is shorter than minItems"));
        }
        if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
            && u64::try_from(items.len()).is_ok_and(|length| length > maximum)
        {
            return Err(format!("{path}: array is longer than maxItems"));
        }
    }
    Ok(())
}

fn matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn validate_terminal_invariants(manifest: &Value) -> Result<(), String> {
    let status = manifest["status"]
        .as_str()
        .ok_or_else(|| "terminal status is missing".to_owned())?;
    if manifest["final_decision"] != status {
        return Err("status and final_decision differ".to_owned());
    }
    match status {
        "confirmed"
            if !manifest["promotion"].is_object()
                || !manifest["failure"].is_null()
                || manifest["confirmation"]["outcome"] != "accepted"
                || !manifest["completed_phases"]
                    .as_array()
                    .is_some_and(|phases| phases.iter().any(|phase| phase == "promotion")) =>
        {
            Err("confirmed run needs promotion and no failure".to_owned())
        }
        "failed" | "interrupted" if !manifest["failure"].is_object() => {
            Err("failed or interrupted run needs failure evidence".to_owned())
        }
        "no_improvement"
            if !manifest["promotion"].is_null()
                || !manifest["failure"].is_null()
                || manifest["confirmation"]["outcome"] != "rejected" =>
        {
            Err("no-improvement run needs a rejected confirmation only".to_owned())
        }
        "confirmed" | "failed" | "interrupted" | "no_improvement" => Ok(()),
        _ => Err("manifest is not terminal".to_owned()),
    }
}

fn terminal_variants(base: &Value) -> [Value; 3] {
    let mut confirmed = base.clone();
    confirmed["status"] = json!("confirmed");
    confirmed["final_decision"] = json!("confirmed");
    confirmed["selected_candidate"] = json!("thin-lto");
    confirmed["completed_phases"]
        .as_array_mut()
        .expect("completed phases")
        .push(json!("promotion"));
    let mut confirmation_baseline = confirmed["baseline"].clone();
    confirmation_baseline["stage"] = json!("confirmation");
    let mut confirmation_candidate = confirmation_baseline.clone();
    confirmation_candidate["strategy"] = json!("thin-lto");
    confirmed["confirmation"] = json!({
        "strategy": "thin-lto",
        "outcome": "accepted",
        "baseline_build": confirmation_baseline,
        "baseline_build_failure": null,
        "candidate_build": confirmation_candidate,
        "candidate_build_failure": null,
        "measurement": {
            "baseline_durations_ns": vec![1_000_000; 20],
            "candidate_durations_ns": vec![950_000; 20],
            "median_ratio": 0.95,
            "baseline_relative_mad": 0.0,
            "candidate_relative_mad": 0.0,
            "confidence_interval_95": {
                "lower": 0.95,
                "upper": 0.95
            },
            "threshold_ratio": 0.98,
            "outcome": "accepted"
        },
        "workload_failure": null,
        "rejection_reason": null
    });
    confirmed["promotion"] = json!({
        "source_strategy": "thin-lto",
        "source_path": "/fixture/candidate",
        "source_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
        "promoted_path": "/fixture/.temper/runs/schema-v1-fixture/best/artifact",
        "promoted_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
        "size_bytes": 1,
        "permissions_mode": 493
    });

    let mut failed = base.clone();
    failed["status"] = json!("failed");
    failed["final_decision"] = json!("failed");
    failed["failure"] = failure_record(None, "promotion");

    let mut interrupted = base.clone();
    interrupted["status"] = json!("interrupted");
    interrupted["final_decision"] = json!("interrupted");
    interrupted["failure"] = failure_record(Some("interrupted"), "promotion");

    [confirmed, failed, interrupted]
}

fn failure_record(outcome: Option<&str>, phase: &str) -> Value {
    json!({
        "phase": phase,
        "outcome": outcome,
        "message": "synthetic terminal fixture",
        "bounded_diagnostics": "",
        "diagnostics_truncated": false,
        "build_failure": null
    })
}
