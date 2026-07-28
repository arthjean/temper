# Temper v0.0.2 corpus readiness

Date: 2026-07-28

Verdict: EP-001 and EP-002 have executable proof, no unresolved failure and
independent `/review-epic` completion. Their epic statuses are `DONE`, so the
mechanical corpus gate is satisfied and US-010 may start. No
`benchmarks/corpus/v1` content has been created.

## EP-001 criterion ledger

| Criterion | Proof |
|---|---|
| US-001-1 | Checked-in `rustflags-workspace` captures absent, string and array target rustflags. |
| US-001-2 | `cargo_preserves_owned_target_flag_order_and_excludes_host_units` proves project flags precede the PGO flag. |
| US-001-3 | The same capture proves build scripts and proc macros omit target PGO flags. |
| US-001-4 | Array embedded spaces are preserved; ambiguous quoted string boundaries reject stably. |
| US-001-5 | Ambient, build, cfg-selected and multiple target sources remain fail-closed. |
| US-001-6 | Fixture README records the supported matrix and Cargo 1.97.1. |
| US-002-1 | `BuildPlan.environment_overrides` is process-local. |
| US-002-2 | PGO records one ordered `CARGO_ENCODED_RUSTFLAGS` override. |
| US-002-3 | Thin/Fat LTO remain Cargo profile overrides. |
| US-002-4 | PGO emits no list-valued target rustflags `--config`. |
| US-002-5 | Schema 2 persists environment arguments as an ordered array. |
| US-002-6 | Real absent, string and array PGO matrix paths complete. |
| US-002-7 | Ambient higher-precedence flags reject only PGO before Cargo. |
| US-003-1 | Plain `TextLine` noise remains accepted and bounded. |
| US-003-2 | JSON-looking `TextLine` rejects as `unrecognized_cargo_json`. |
| US-003-3 | Unknown and malformed message fixtures exercise cargo_metadata 0.23.1. |
| US-003-4 | Artifact selection matches package ID, binary name, exact bin kind and executable. |
| US-003-5 | Zero and multiple executables have distinct stable reasons and zero samples. |
| US-003-6 | Dependency, build-script and proc-macro artifacts are ignored. |
| US-003-7 | A fresh matching artifact inside the isolated target is accepted and hashed. |
| US-004-1 | New records use schema 2; a historical schema-1 byte fixture is unchanged. |
| US-004-2 | Both phases persist normalized package, target, arguments, profile, flags, config hashes and tool identities. |
| US-004-3 | The parity allowlist contains exactly the three specified differences. |
| US-004-4 | Matching paths persist `matched: true` and no unexpected difference before screening. |
| US-004-5 | Injected config and unit-level field changes persist named mismatches and reject only PGO. |
| US-004-6 | Existing anchored atomic persistence remains the only run-manifest write path. |
| US-004-7 | `docs/schema-v2.md`, CLI output and JSON fixtures document schema 2 without rewriting schema 1. |

## EP-002 criterion ledger

| Criterion | Proof |
|---|---|
| US-005-1 | `pgo-missing-function` creates real incompatible profile data with the LLVM warning flag. |
| US-005-2 | The fixture test captures the raw Cargo JSON line and parsed compiler-message fields. |
| US-005-3 | Cargo exits successfully; fixture documentation pins rustc, LLVM and Cargo versions. |
| US-005-4 | The fingerprint requires warning level, no code and the narrow message prefix after `: `. |
| US-005-5 | A structured `dead_code` warning proves unrelated warnings do not match. |
| US-005-6 | The supported toolchain emitted one classifiable warning, so US-006 was unblocked. |
| US-006-1 | Only optimized PGO adds `-Cllvm-args=-pgo-warn-missing-function`. |
| US-006-2 | Bounded structured diagnostics retain package, target, level, code, message and rendered text. |
| US-006-3 | Matching diagnostics reject as `pgo_missing_profile_data`. |
| US-006-4 | The strategy failure retains the optimized invocation and matched structured diagnostic. |
| US-006-5 | Both synthetic injection and real control record zero PGO samples and never select PGO. |
| US-006-6 | Unrelated warnings remain diagnostic evidence without PGO missing-profile rejection. |
| US-006-7 | Static candidates remain eligible and confirmation continues. |
| US-007-1 | Raw profiles are canonical, ordered and record byte length plus SHA-256. |
| US-007-2 | Zero, excessive, non-regular and symlinked raw profile cases reject before merge. |
| US-007-3 | Merge arguments persist explicit `--failure-mode=any`. |
| US-007-4 | Corruption, nonzero status and any bounded merge diagnostics reject. |
| US-007-5 | Success requires one nonempty regular in-run `.profdata` with size and SHA-256. |
| US-007-6 | Missing, empty, existing/symlinked and out-of-run output checks precede optimized Cargo. |
| US-007-7 | Merge rejection leaves static selection live and starts no optimized PGO build. |
| US-008-1 | `docs/pgo-compatibility-matrix-v1.md` names 17 independently reported cases. |
| US-008-2 | Three successful cases cover all required rustflag, path, base strategy, workspace and host-tool dimensions. |
| US-008-3 | Named rejection cases cover every required incompatibility. |
| US-008-4 | Supported cases persist all PGO phases, matched parity and seven screening samples. |
| US-008-5 | Rejections persist stable reasons, zero PGO samples and static eligibility. |
| US-008-6 | Matrix helpers compare source, manifest and lockfile inputs before and after execution. |
| US-008-7 | Existing process-group tests prove descendant cleanup; symlink tests prove no path escape write. |
| US-009-1 | Four retained real-toolchain paths cover single, workspace, string flags and host tools. |
| US-009-2 | The real incompatible-profile control records `pgo_missing_profile_data` and zero PGO samples. |
| US-009-3 | The post-review dated evidence retains fixture source, workloads, toolchain fingerprint, production/harness/binary hashes and five raw manifests. |
| US-009-4 | Every referenced artifact and profile checksum was independently recomputed before cleanup. |
| US-009-5 | The verification ledger contains an append-only v0.0.2 correctness section. |
| US-009-6 | All retained workloads are labeled synthetic controls with no performance claim. |
| US-009-7 | This record enumerates all criteria and preserves the independent `/review-epic` corpus gate. |

## Gate state

EP-001 and EP-002 are `DONE` after their independent reviews and required
quality gates. EP-003 and US-010 remain `TODO`, but their hard dependency gate
is now satisfied. No `benchmarks/corpus/v1` content has been created.
