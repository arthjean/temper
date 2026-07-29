//! Observed compiler-input parity for schema 3.
//!
//! Schema 2 compared the inputs Temper *planned*. Schema 3 compares the inputs
//! the compiler *received*, after every stage has executed. The normalization
//! allowlist is closed and was derived from the executed EP-001 matrix
//! (`docs/evidence/interposition/2026-07-28/README.md`): only the stage target
//! root and the documented PGO phase controls may differ. Cargo artifact
//! identity fields were proven byte-identical across phases and are therefore
//! compared, not normalized.
//!
//! Every unknown difference rejects PGO (FR-021). An incomplete comparison can
//! never serialize `matched: true` (NFR-006).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::cargo::BuildRecord;
use crate::interposition::{
    CaptureAggregate, InterpositionRecord, InvocationClass, NORMALIZATION_VERSION, ShimStage,
};

/// Stable reason of an observed compiler-input difference.
pub(crate) const MISMATCH_REASON: &str = "compiler_input_mismatch";

/// Bounded number of persisted difference entries. Truncation is explicit and
/// can never remove the stable reason.
const DIFFERENCE_LIMIT: usize = 64;

/// The closed normalization allowlist. Anything outside it rejects PGO.
const ALLOWLIST: [&str; 3] = [
    "stage_target_root_paths",
    "profile_generate_vs_profile_use",
    "pgo_warn_missing_function_in_use",
];

/// Bounded difference classes. Every rejection maps to exactly one of them.
pub(crate) const DIFFERENCE_CLASSES: [&str; 13] = [
    "evidence_unavailable",
    "tool_changed",
    "ambiguous_crate_identity",
    "crate_count",
    "crate_added",
    "crate_missing",
    "artifact_metadata_changed",
    "artifact_extra_filename_changed",
    "crate_kind_changed",
    "argument_count",
    "argument_added",
    "argument_removed",
    "argument_order",
];

/// Which comparison produced a parity record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ParityScope {
    /// Reference against generation and against optimization.
    PgoTraining,
    /// The accepted optimized build against its confirmation rebuild.
    PgoConfirmation,
}

/// One target compilation, reduced to its comparable observed inputs.
#[derive(Clone, Debug)]
struct UnitInputs {
    crate_name: String,
    metadata: Option<String>,
    crate_types: Vec<String>,
    extra_filename: Option<String>,
    argument_digests: Vec<String>,
    normalized_digest: String,
}

impl UnitInputs {
    /// Logical crate identity. `-Cmetadata` is Cargo's own per-unit hash, so it
    /// separates a package's library from its binary and two semver-distinct
    /// versions of one crate, while staying stable across stages.
    fn identity(&self) -> (&str, &str) {
        (
            self.crate_name.as_str(),
            self.metadata.as_deref().unwrap_or_default(),
        )
    }
}

/// One executed stage's observed compiler inputs.
#[derive(Clone, Debug)]
pub(crate) struct StageInputs {
    stage: ShimStage,
    real_rustc: PathBuf,
    real_rustc_version: String,
    shim_sha256: String,
    capture_digest: String,
    record_count: usize,
    units: Vec<UnitInputs>,
}

/// Bounded per-stage summary persisted in schema 3.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StageSummary {
    stage: ShimStage,
    record_count: usize,
    target_compilations: usize,
    capture_digest: String,
    /// Digest of the normalized target-compilation set. Two runs of the same
    /// workspace with fresh stage roots produce the same value (NFR-007).
    normalized_aggregate_digest: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Difference {
    pub(crate) class: &'static str,
    pub(crate) comparison: String,
    pub(crate) crate_name: Option<String>,
}

/// The schema-3 observed compiler parity object.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CompilerParityRecord {
    pub(crate) normalization: u32,
    pub(crate) scope: ParityScope,
    pub(crate) allowlist: [&'static str; 3],
    pub(crate) stages: Vec<StageSummary>,
    pub(crate) matched: bool,
    pub(crate) reason: Option<&'static str>,
    pub(crate) differences: Vec<Difference>,
    pub(crate) differences_truncated: bool,
}

impl CompilerParityRecord {
    /// Bounded difference classes, deduplicated, for a compact report line.
    pub(crate) fn difference_classes(&self) -> Vec<String> {
        let mut classes: Vec<String> = Vec::new();
        for difference in &self.differences {
            if !classes.iter().any(|known| known == difference.class) {
                classes.push(difference.class.to_owned());
            }
        }
        classes
    }

    /// The first affected logical crate, when one is safely available.
    pub(crate) fn affected_crate(&self) -> Option<String> {
        self.differences
            .iter()
            .find_map(|difference| difference.crate_name.clone())
    }
}

/// Extracts one stage's observed inputs from a completed interposed build.
pub(crate) fn stage_inputs(build: &BuildRecord) -> Option<StageInputs> {
    let interposition = build.invocation.interposition.as_ref()?;
    let aggregate = build.compiler_evidence.as_deref()?;
    Some(from_evidence(interposition, aggregate))
}

fn from_evidence(interposition: &InterpositionRecord, aggregate: &CaptureAggregate) -> StageInputs {
    let mut units: Vec<UnitInputs> = aggregate
        .records
        .iter()
        .filter(|record| record.class == InvocationClass::TargetCompile)
        .map(|record| UnitInputs {
            crate_name: record.crate_name.clone().unwrap_or_default(),
            metadata: record.metadata.clone(),
            crate_types: record.crate_types.clone(),
            extra_filename: record.extra_filename.clone(),
            argument_digests: record.argument_digests.clone(),
            normalized_digest: record.normalized_digest.clone(),
        })
        .collect();
    units.sort_by(|left, right| left.identity().cmp(&right.identity()));
    StageInputs {
        stage: interposition.stage,
        real_rustc: interposition.real_rustc.clone(),
        real_rustc_version: interposition.real_rustc_version.clone(),
        shim_sha256: interposition.shim_sha256.clone(),
        capture_digest: aggregate.capture_digest.clone(),
        record_count: aggregate.record_count,
        units,
    }
}

/// Decides training parity: the pass-through reference against both PGO phases.
pub(crate) fn decide_training(
    reference: Option<StageInputs>,
    generation: Option<StageInputs>,
    optimization: Option<StageInputs>,
) -> CompilerParityRecord {
    decide(
        ParityScope::PgoTraining,
        (ShimStage::Reference, reference),
        vec![
            (ShimStage::Generate, generation),
            (ShimStage::Use, optimization),
        ],
    )
}

/// Decides confirmation parity: the accepted optimized build against the fresh
/// confirmation rebuild, before promotion.
pub(crate) fn decide_confirmation(
    optimization: Option<StageInputs>,
    confirmation: Option<StageInputs>,
) -> CompilerParityRecord {
    decide(
        ParityScope::PgoConfirmation,
        (ShimStage::Use, optimization),
        vec![(ShimStage::Confirmation, confirmation)],
    )
}

fn decide(
    scope: ParityScope,
    anchor: (ShimStage, Option<StageInputs>),
    candidates: Vec<(ShimStage, Option<StageInputs>)>,
) -> CompilerParityRecord {
    let mut differences = Vec::new();
    let mut stages = Vec::new();
    let (anchor_stage, anchor_inputs) = anchor;
    if let Some(inputs) = &anchor_inputs {
        stages.push(summary(inputs));
    } else {
        differences.push(Difference {
            class: "evidence_unavailable",
            comparison: anchor_stage.label().to_owned(),
            crate_name: None,
        });
    }
    for (stage, inputs) in candidates {
        if let Some(inputs) = &inputs {
            stages.push(summary(inputs));
        }
        match (&anchor_inputs, &inputs) {
            (Some(left), Some(right)) => compare(left, right, &mut differences),
            _ => differences.push(Difference {
                class: "evidence_unavailable",
                comparison: stage.label().to_owned(),
                crate_name: None,
            }),
        }
    }
    let matched = differences.is_empty();
    let differences_truncated = differences.len() > DIFFERENCE_LIMIT;
    differences.truncate(DIFFERENCE_LIMIT);
    CompilerParityRecord {
        normalization: NORMALIZATION_VERSION,
        scope,
        allowlist: ALLOWLIST,
        stages,
        matched,
        reason: (!matched).then_some(MISMATCH_REASON),
        differences,
        differences_truncated,
    }
}

fn summary(inputs: &StageInputs) -> StageSummary {
    StageSummary {
        stage: inputs.stage,
        record_count: inputs.record_count,
        target_compilations: inputs.units.len(),
        capture_digest: inputs.capture_digest.clone(),
        normalized_aggregate_digest: aggregate_digest(inputs),
    }
}

fn aggregate_digest(inputs: &StageInputs) -> String {
    let mut values: Vec<String> = inputs
        .units
        .iter()
        .map(|unit| {
            format!(
                "{}|{}|{}|{}|{}",
                unit.crate_name,
                unit.metadata.as_deref().unwrap_or_default(),
                unit.crate_types.join(","),
                unit.extra_filename.as_deref().unwrap_or_default(),
                unit.normalized_digest
            )
        })
        .collect();
    values.sort();
    crate::interposition::framed_text_digest(&values)
}

fn compare(left: &StageInputs, right: &StageInputs, differences: &mut Vec<Difference>) {
    let comparison = format!("{}_vs_{}", left.stage.label(), right.stage.label());
    let mut push = |class: &'static str, crate_name: Option<String>| {
        debug_assert!(
            DIFFERENCE_CLASSES.contains(&class),
            "every difference must stay inside the closed bounded set"
        );
        differences.push(Difference {
            class,
            comparison: comparison.clone(),
            crate_name,
        });
    };

    if left.real_rustc != right.real_rustc
        || left.real_rustc_version != right.real_rustc_version
        || left.shim_sha256 != right.shim_sha256
    {
        push("tool_changed", None);
    }

    if has_duplicate_identity(&left.units) || has_duplicate_identity(&right.units) {
        push("ambiguous_crate_identity", None);
        return;
    }
    if left.units.len() != right.units.len() {
        push("crate_count", None);
    }

    let mut right_paired = vec![false; right.units.len()];
    let mut left_leftovers = Vec::new();
    for unit in &left.units {
        match right
            .units
            .iter()
            .position(|other| other.identity() == unit.identity())
        {
            Some(index) => {
                right_paired[index] = true;
                compare_unit(unit, &right.units[index], &mut push);
            }
            None => left_leftovers.push(unit),
        }
    }

    // A leftover pair sharing one crate name differs only by Cargo's artifact
    // identity hash, which is one named difference rather than two crate
    // changes.
    for unit in left_leftovers {
        let counterparts: Vec<usize> = right
            .units
            .iter()
            .enumerate()
            .filter(|(index, other)| !right_paired[*index] && other.crate_name == unit.crate_name)
            .map(|(index, _)| index)
            .collect();
        match counterparts.as_slice() {
            [index] => {
                right_paired[*index] = true;
                push("artifact_metadata_changed", Some(unit.crate_name.clone()));
                compare_unit(unit, &right.units[*index], &mut push);
            }
            _ => push("crate_missing", Some(unit.crate_name.clone())),
        }
    }
    for (index, unit) in right.units.iter().enumerate() {
        if !right_paired[index] {
            push("crate_added", Some(unit.crate_name.clone()));
        }
    }
}

fn has_duplicate_identity(units: &[UnitInputs]) -> bool {
    units.iter().enumerate().any(|(index, unit)| {
        units[index + 1..]
            .iter()
            .any(|other| other.identity() == unit.identity())
    })
}

fn compare_unit(
    left: &UnitInputs,
    right: &UnitInputs,
    push: &mut impl FnMut(&'static str, Option<String>),
) {
    let named = || Some(left.crate_name.clone());
    if left.crate_types != right.crate_types {
        push("crate_kind_changed", named());
    }
    if left.extra_filename != right.extra_filename {
        push("artifact_extra_filename_changed", named());
    }
    if left.argument_digests == right.argument_digests {
        return;
    }
    if left.argument_digests.len() != right.argument_digests.len() {
        push("argument_count", named());
    }
    let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
    for digest in &left.argument_digests {
        *counts.entry(digest.as_str()).or_default() += 1;
    }
    for digest in &right.argument_digests {
        *counts.entry(digest.as_str()).or_default() -= 1;
    }
    let removed = counts.values().any(|count| *count > 0);
    let added = counts.values().any(|count| *count < 0);
    if removed {
        push("argument_removed", named());
    }
    if added {
        push("argument_added", named());
    }
    if !removed && !added {
        push("argument_order", named());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompilerParityRecord, DIFFERENCE_CLASSES, MISMATCH_REASON, ParityScope, StageInputs,
        UnitInputs, decide_confirmation, decide_training,
    };
    use crate::interposition::ShimStage;
    use std::path::PathBuf;

    fn unit(name: &str, metadata: &str, arguments: &[&str]) -> UnitInputs {
        UnitInputs {
            crate_name: name.to_owned(),
            metadata: Some(metadata.to_owned()),
            crate_types: vec!["lib".to_owned()],
            extra_filename: Some(format!("-{metadata}")),
            argument_digests: arguments.iter().map(|value| (*value).to_owned()).collect(),
            normalized_digest: arguments.join("+"),
        }
    }

    fn stage(stage: ShimStage) -> StageInputs {
        StageInputs {
            stage,
            real_rustc: PathBuf::from("/toolchain/rustc"),
            real_rustc_version: "rustc 1.97.1".to_owned(),
            shim_sha256: "shim-hash".to_owned(),
            capture_digest: format!("capture-{}", stage.label()),
            record_count: 4,
            units: vec![
                unit("app", "aaaa", &["a1", "a2", "a3"]),
                unit("lib", "bbbb", &["b1", "b2"]),
            ],
        }
    }

    fn training(mutate: impl FnOnce(&mut StageInputs)) -> CompilerParityRecord {
        let mut optimization = stage(ShimStage::Use);
        mutate(&mut optimization);
        decide_training(
            Some(stage(ShimStage::Reference)),
            Some(stage(ShimStage::Generate)),
            Some(optimization),
        )
    }

    #[test]
    fn identical_observed_inputs_match_across_all_three_stages() {
        let parity = training(|_| {});
        assert!(parity.matched);
        assert!(parity.reason.is_none());
        assert!(parity.differences.is_empty());
        assert_eq!(parity.scope, ParityScope::PgoTraining);
        assert_eq!(parity.stages.len(), 3);
        // The normalized aggregate is content-keyed, so every stage agrees.
        let digests: Vec<&str> = parity
            .stages
            .iter()
            .map(|summary| summary.normalized_aggregate_digest.as_str())
            .collect();
        assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(parity.stages[0].target_compilations, 2);
    }

    #[test]
    fn record_order_alone_is_not_a_difference() {
        let parity = training(|optimization| optimization.units.reverse());
        assert!(parity.matched, "{:?}", parity.differences);
    }

    #[test]
    fn every_adversarial_mutation_rejects_with_its_bounded_class() {
        type Mutation = (&'static str, &'static str, fn(&mut StageInputs));
        let mutations: [Mutation; 11] = [
            ("argument added", "argument_added", |stage| {
                stage.units[0].argument_digests.push("extra".to_owned());
            }),
            ("argument removed", "argument_removed", |stage| {
                stage.units[0].argument_digests.pop();
            }),
            ("argument changed", "argument_added", |stage| {
                stage.units[0].argument_digests[1] = "changed".to_owned();
            }),
            ("argument reordered", "argument_order", |stage| {
                stage.units[0].argument_digests.reverse();
            }),
            ("argument duplicated", "argument_count", |stage| {
                let first = stage.units[0].argument_digests[0].clone();
                stage.units[0].argument_digests.push(first);
            }),
            ("crate added", "crate_added", |stage| {
                stage.units.push(unit("extra", "cccc", &["c1"]));
            }),
            ("crate removed", "crate_missing", |stage| {
                stage.units.pop();
            }),
            ("crate renamed", "crate_added", |stage| {
                stage.units[0].crate_name = "renamed".to_owned();
            }),
            ("target kind changed", "crate_kind_changed", |stage| {
                stage.units[0].crate_types = vec!["bin".to_owned()];
            }),
            (
                "artifact identity changed",
                "artifact_metadata_changed",
                |stage| {
                    stage.units[0].metadata = Some("mutated".to_owned());
                },
            ),
            ("tool path changed", "tool_changed", |stage| {
                stage.real_rustc = PathBuf::from("/other/rustc");
            }),
        ];
        for (label, expected, mutate) in mutations {
            let parity = training(mutate);
            assert!(!parity.matched, "{label} matched");
            assert_eq!(parity.reason, Some(MISMATCH_REASON), "{label}");
            assert!(
                parity
                    .difference_classes()
                    .iter()
                    .any(|class| class == expected),
                "{label} reported {:?} instead of {expected}",
                parity.difference_classes()
            );
            // Every reported class stays inside the closed bounded set.
            assert!(
                parity
                    .difference_classes()
                    .iter()
                    .all(|class| DIFFERENCE_CLASSES.contains(&class.as_str())),
                "{label} reported an unbounded class"
            );
        }
    }

    #[test]
    fn a_duplicate_crate_identity_is_not_comparable() {
        let parity = training(|stage| {
            let duplicate = stage.units[0].clone();
            stage.units.push(duplicate);
        });
        assert!(!parity.matched);
        assert!(
            parity
                .difference_classes()
                .iter()
                .any(|class| class == "ambiguous_crate_identity")
        );
    }

    #[test]
    fn an_incomplete_comparison_can_never_match() {
        for parity in [
            decide_training(
                None,
                Some(stage(ShimStage::Generate)),
                Some(stage(ShimStage::Use)),
            ),
            decide_training(
                Some(stage(ShimStage::Reference)),
                None,
                Some(stage(ShimStage::Use)),
            ),
            decide_training(
                Some(stage(ShimStage::Reference)),
                Some(stage(ShimStage::Generate)),
                None,
            ),
            decide_confirmation(Some(stage(ShimStage::Use)), None),
            decide_confirmation(None, None),
        ] {
            assert!(!parity.matched);
            assert_eq!(parity.reason, Some(MISMATCH_REASON));
            assert!(
                parity
                    .difference_classes()
                    .iter()
                    .any(|class| class == "evidence_unavailable")
            );
        }
    }

    #[test]
    fn confirmation_parity_compares_the_accepted_use_stage() {
        let parity = decide_confirmation(
            Some(stage(ShimStage::Use)),
            Some(stage(ShimStage::Confirmation)),
        );
        assert!(parity.matched);
        assert_eq!(parity.scope, ParityScope::PgoConfirmation);
        assert_eq!(parity.stages.len(), 2);
    }
}
