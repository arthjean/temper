[PRD]
# PRD: Temper v0.0.2, PGO integrity and benchmark corpus v1

## Changelog

| Version | Date | Author | Summary |
|---------|------|--------|---------|
| 0.0.2 | 2026-07-28 | Arthur Jean | Harden the LLVM PGO evidence chain, then create benchmark corpus v1 after the hardening epics are DONE |

## Problem Statement

1. Temper v0.0.1 completes the LLVM PGO workflow, but a successful optimized
   build does not prove that LLVM applied profile data to every expected
   function. The optimized phase omits
   `-Cllvm-args=-pgo-warn-missing-function`, so an incompatible or incomplete
   profile can remain silent.
2. Temper accepts Cargo target rustflags expressed as either a string or an
   array, but injects PGO through a list-valued CLI configuration override. Cargo
   rejects a merge between an existing string and an incoming list. The string
   form can therefore pass preflight and fail only when instrumentation starts.
3. Generation and use phases share a base strategy, but Temper persists no
   normalized proof that package, target, profile, project flags, toolchain and
   configuration provenance match outside an explicit PGO allowlist.
4. `cargo_metadata` tolerates some unknown or schema-incompatible JSON records by
   returning `TextLine`. Temper currently records those lines as diagnostics,
   which can hide a Cargo build-stream compatibility break while a matching
   artifact is still accepted.
5. Temper has no checked-in benchmark corpus, no production-representative
   result and no reusable raw evidence. Creating a corpus before closing the PGO
   integrity gaps would turn unproven measurements into durable benchmark
   claims.

**Why now:** The v0.0.1 implementation and dogfooding prove that the complete
workflow executes, while the 2026-07-28 Rust and Cargo source survey identifies
specific, testable gaps in flag composition, diagnostic handling and phase
parity. These gaps must be closed before Temper can create the versioned corpus
needed to evaluate real applications.

## Overview

Temper v0.0.2 keeps the existing external-process architecture. Cargo remains
the owner of dependency resolution, profiles, build scripts, fingerprints and
rustc invocations. Temper continues to use `cargo metadata --format-version 1`,
Cargo JSON messages, documented configuration, an explicit host target and
isolated target directories. It does not import Cargo or rustc internals.

The first release phase makes PGO evidence fail-closed. Temper will validate
Cargo rustflag behavior, compose project and PGO flags through one process-local
`CARGO_ENCODED_RUSTFLAGS` value, reject JSON-looking records that the current
parser cannot classify, add LLVM missing-function diagnostics, validate raw and
merged profiles, and persist a schema-v2 parity record. PGO rejection remains
nonfatal to the bounded search: valid static candidates may still be confirmed,
but a rejected PGO candidate is never screened or promoted.

The second release phase creates `benchmarks/corpus/v1` only after EP-001 and
EP-002 are both `DONE`. Corpus v1 contains at least three redistributed,
pinned, real Rust applications across defined workload classes and one
synthetic control that is excluded from representativeness claims. Every case
has a correctness oracle, explicit scenario weights, provenance, license
evidence, input and source checksums, and three retained `run.json` records from
the reference host. Temper will publish no aggregate cross-application score.

## Goals

| Goal | Month-1 Target | Month-6 Target |
|------|---------------|----------------|
| Preserve compatible Cargo target rustflags during both PGO phases | String, array and no-rustflags fixtures all complete instrumentation and use with 100% expected flag order | 100% pass rate across at least 20 supported flag-composition fixtures |
| Detect incompatible PGO profiles before screening | 100% of at least 5 injected missing-profile cases reject only PGO | 100% of at least 25 injected cases reject only PGO |
| Prove generate/use configuration parity | 100% of PGO attempts persist a schema-v2 parity record | 100% across at least 50 retained PGO attempts |
| Preserve Cargo artifact compatibility | 100% of JSON-looking unparsed lines and zero/multiple matching artifacts fail closed in the integration matrix | 100% across the supported Cargo compatibility matrix |
| Establish a representative corpus | 3 real applications across 3 workload classes plus 1 excluded synthetic control | At least 6 real applications across at least 4 workload classes |
| Preserve benchmark evidence | 3 raw `run.json` records per corpus case with source, input, lockfile and artifact hashes | 100% of cited results retain all required records |

## Target Users

### Primary: Rust performance engineer

- **Role:** A developer optimizing a CPU-bound or throughput-sensitive Rust
  binary.
- **Behaviors:** Uses Cargo release profiles, profiling tools and executable
  workloads, and reviews compiler diagnostics when an optimization is rejected.
- **Pain points:** A green PGO build can conceal missing profile data; Cargo flag
  precedence makes manual composition error-prone; current Temper evidence does
  not prove phase parity.
- **Current workaround:** Runs verbose Cargo commands, compares rustc command
  lines manually, inspects LLVM warnings and repeats workloads in ad hoc
  directories.
- **Success looks like:** Every PGO attempt either produces a parity-proven
  candidate or a persisted rejection reason before screening.

### Secondary: Temper maintainer

- **Role:** A maintainer deciding whether Temper changes improve real Rust
  applications without increasing false promotions.
- **Behaviors:** Runs the integration suite, performs toolchain dogfooding,
  curates workloads and audits retained run manifests.
- **Pain points:** Temporary fixtures and synthetic timings cannot support
  performance claims; third-party projects introduce provenance, licensing and
  correctness-oracle requirements.
- **Current workaround:** Creates temporary Cargo projects, records summary
  tables manually and loses the executable fixtures after cleanup.
- **Success looks like:** A gated corpus command reproduces each pinned case,
  validates outputs and retains per-case evidence without an aggregate score.

## Research Findings

Key findings that informed this PRD:

### Competitive Context

- [`cargo-pgo`](https://github.com/Kobzol/cargo-pgo) automates instrumentation
  and optimization, while warning that unrepresentative training can degrade
  other workloads. Temper differs by rejecting candidates through independent
  measurement and by persisting why a PGO attempt was excluded.
- [`cargo-wizard`](https://github.com/Kobzol/cargo-wizard) recommends static
  Cargo profile settings. Temper measures project-specific candidates and does
  not treat a preset as performance evidence.
- [`rustc-perf`](https://github.com/rust-lang/rustc-perf) uses versioned suites,
  named scenarios and persistent results to evaluate compiler performance.
  Temper applies the same provenance discipline to application runtime while
  retaining wall-clock uncertainty.
- Cargo's own benchmark capture preserves workspace topology but strips source
  targets and profiles. It is useful for Cargo graph benchmarks, not for
  executable runtime workloads.
- **Market gap:** Adjacent tools automate a mechanism, recommend configuration
  or benchmark the compiler. None combines conservative application
  optimization with a gated, correctness-checking, versioned application corpus.

### Best Practices Applied

- The official [rustc PGO guide](https://doc.rust-lang.org/rustc/profile-guided-optimization.html)
  requires instrumentation, representative execution, profile merge and an
  optimized rebuild. It recommends absolute paths, an explicit Cargo target,
  clean profile inputs and `-Cllvm-args=-pgo-warn-missing-function`.
- [Cargo external tools](https://doc.rust-lang.org/cargo/reference/external-tools.html)
  should resolve artifacts from `compiler-artifact.executable` and tolerate
  plain-text contamination without inferring target-directory paths.
- [Cargo configuration](https://doc.rust-lang.org/cargo/reference/config.html)
  gives environment rustflags precedence over target and build configuration.
  With an explicit `--target`, target flags do not instrument host build scripts
  or proc macros.
- [`llvm-profdata`](https://llvm.org/docs/CommandGuide/llvm-profdata.html)
  supports `--failure-mode=any`, which rejects the merge when any input profile
  is invalid. Scenario weighting must be explicit because longer executions
  otherwise contribute proportionally more profile counts.
- Versioned corpus entries need pinned source, lockfile and input identity,
  executable oracles, license evidence, raw records and a stated
  representativeness boundary.

Full local architecture findings and source references are recorded in
`docs/research/rust-cargo-codebase-survey.md`.

## Assumptions & Constraints

### Assumptions (to validate)

- The active supported rustc emits missing-profile information through a
  diagnostic shape that Temper can classify without matching an unrestricted
  substring. US-005 validates this before production behavior changes.
- A process-local `CARGO_ENCODED_RUSTFLAGS` composed from already proven target
  flags preserves Cargo's argument boundaries for both string and array config
  forms. US-001 validates the observed commands before US-002 implements it.
- Comparing normalized inputs can isolate exactly three permitted PGO
  differences: target directory, generate versus use flag, and the
  missing-function LLVM argument.
- Three real applications spanning compute, parsing/transformation and
  streaming/compression provide a minimum corpus-v1 diversity boundary. This is
  a bounded product definition, not proof of ecosystem-wide representativeness.
- A redistributed source snapshot of at most 25 MiB per real case can contain
  its lockfile, deterministic inputs and license evidence without Git LFS.
- Three independent Temper runs per corpus case are sufficient to publish
  per-case v1 observations, while making no cross-machine or aggregate claim.

### Hard Constraints

- EP-003 must not start until every story in EP-001 and EP-002 is `DONE` and
  both epic roll-ups are `DONE` in
  `tasks/prd-temper-v0.0.2-status.json`.
- Before that gate, no `benchmarks/corpus/v1` file, corpus execution or corpus
  score may be created.
- v0.0.2 retains the v0.0.1 platform boundary: Linux host
  `x86_64-unknown-linux-gnu`, Cargo binary targets, an existing `Cargo.lock` and
  release builds with `--locked`.
- PGO rejection must never invalidate a valid static strategy or trigger PGO
  screening, confirmation or promotion.
- Cargo metadata and JSON messages remain the artifact boundary. Target
  directory inference, `cargo::Executor`, `rustc_driver`, `rustc_public`,
  `--unit-graph` and other unstable interfaces remain excluded.
- Ambient `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, build rustflags, cfg-selected
  rustflags, compiler wrappers and multiple ambiguous flag sources remain
  fail-closed unless a story explicitly proves composition.
- Only the `llvm-profdata` adjacent to the active rustc is allowed.
- New v0.0.2 runs use report schema 2. Existing schema-1 records are never
  rewritten, migrated or reinterpreted.
- Corpus workloads and their Cargo build scripts are trusted arbitrary code.
  The corpus runner performs no automatic network access and provides no
  sandbox.
- Each real corpus source must permit redistribution, include its required
  license notices and pass manual secrets and personal-data review.
- No new Rust dependency, service, database, daemon or nightly toolchain option
  is introduced by this PRD.

## Quality Gates

These commands must pass for every user story:

- `cargo fmt --check` - verifies Rust formatting.
- `cargo check --all-targets --locked` - verifies compilation for every local target without changing the lockfile.
- `cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings` - rejects local Clippy and compiler warnings.
- `cargo test --all-targets --locked` - executes the complete local Rust test suite.

## Epics & User Stories

### EP-001: Cargo configuration and evidence contract

Prove the effective Cargo inputs, own one unambiguous PGO flag channel, harden
artifact parsing and persist a versioned generate/use parity decision.

**Definition of Done:** String, array and empty target-rustflags fixtures produce
the expected rustc argument order; suspicious Cargo JSON and ambiguous artifacts
fail closed; every PGO attempt persists a schema-v2 parity record; all four
stories are reviewed and `DONE`.

#### US-001: Validate Cargo rustflags composition

**Description:** As a Temper maintainer, I want executable evidence for Cargo's
effective rustflags so that the production injection design matches observed
Cargo behavior.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** None

**Acceptance Criteria:**

- [ ] A committed integration fixture captures target rustc arguments for no
  rustflags, string rustflags and array rustflags with `--target` present.
- [ ] The fixture proves the order produced when a process-local
  `CARGO_ENCODED_RUSTFLAGS` contains preserved project flags followed by one PGO
  flag.
- [ ] The fixture proves that host build scripts and proc macros do not receive
  the target PGO codegen flag.
- [ ] String and array forms containing an argument with an embedded space
  retain the Cargo-defined argument boundaries or are rejected before a build
  with one stable reason code.
- [ ] Given ambient encoded/plain rustflags, cfg-selected rustflags, build
  rustflags or multiple matching target sources, validation confirms the
  existing fail-closed boundary rather than composing an unproven value.
- [ ] The story records the supported composition matrix and its exact Cargo
  version in a test-adjacent comment or fixture document.

#### US-002: Compose PGO flags through one owned environment channel

**Description:** As a Rust performance engineer, I want Temper to preserve
supported project rustflags so that instrumentation and optimization reflect the
project's intended release build.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-001

**Acceptance Criteria:**

- [ ] `BuildPlan` can carry process-local environment overrides without
  mutating the parent Temper process environment.
- [ ] PGO builds set one owned `CARGO_ENCODED_RUSTFLAGS` value containing
  preserved target flags in observed Cargo order followed by the phase-specific
  PGO flags.
- [ ] ThinLTO and FatLTO remain Cargo profile overrides and are not translated
  into direct rustc LTO flags.
- [ ] PGO no longer injects `target.<triple>.rustflags` as a list-valued
  `--config` override.
- [ ] Build evidence records the exact non-secret environment override as an
  ordered argument array, not as a lossy space-joined string.
- [ ] Given string, array or absent target rustflags, real instrumentation and
  optimized builds complete with the expected preserved order.
- [ ] Given any ambient rustflags source that would outrank Temper's owned
  channel, PGO is rejected before Cargo starts and static candidates continue.

#### US-003: Fail closed on incompatible Cargo build messages

**Description:** As a Temper maintainer, I want suspicious build-stream records
rejected so that an artifact is never accepted after an unnoticed schema break.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** None

**Acceptance Criteria:**

- [ ] Plain non-JSON `TextLine` output remains bounded diagnostic noise and does
  not by itself fail a successful build.
- [ ] Any `TextLine` whose first non-whitespace byte is `{` fails the build with
  reason `unrecognized_cargo_json`.
- [ ] Unknown `reason`, malformed known messages and missing required fields are
  covered by fixtures that exercise `cargo_metadata` 0.23.1 behavior.
- [ ] Exactly one matching package ID, binary target name, target kind and
  non-null executable is required after Cargo exits successfully.
- [ ] Zero or multiple matching executables fail with distinct stable reason
  codes and no candidate measurement starts.
- [ ] Unrelated dependency, build-script and proc-macro artifact events remain
  ignored for executable selection.
- [ ] Given a `fresh: true` matching artifact inside the isolated target
  directory, Temper accepts it under the same identity and checksum checks.

#### US-004: Persist and enforce schema-v2 PGO phase parity

**Description:** As a Rust performance engineer, I want a machine-readable
generate/use comparison so that a PGO candidate is screened only when its inputs
are proven equivalent.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-002, US-003

**Acceptance Criteria:**

- [ ] New runs declare `schema_version: 2`; schema-1 files already on disk remain
  byte-for-byte unchanged.
- [ ] Schema 2 records normalized inputs for instrumentation and optimization:
  package ID, binary target, target triple, Cargo arguments, base profile
  overrides, ordered project rustflags, config-source paths and SHA-256 values,
  rustc/Cargo identity and `llvm-profdata` identity.
- [ ] The parity record lists exactly the permitted differences: isolated target
  directory, `-Cprofile-generate` versus `-Cprofile-use`, and
  `-Cllvm-args=-pgo-warn-missing-function` in the use phase.
- [ ] Matching inputs persist `matched: true` and an empty unexpected-differences
  array before PGO screening starts.
- [ ] An injected difference in package, target, base profile, project rustflag,
  config hash or toolchain persists `matched: false`, rejects only PGO and
  identifies every unexpected field.
- [ ] Schema-v2 persistence retains the existing atomic temp-file, sync and
  rename behavior after each state transition.
- [ ] Human and JSON documentation explains schema 2 without changing the
  meaning of any historical schema-1 field.

---

### EP-002: Fail-closed PGO integrity and validation

Make missing profiles visible, reject invalid raw or merged profile evidence,
exercise the supported compatibility matrix and close the hardening phase with
durable real-toolchain evidence.

**Definition of Done:** Missing-function diagnostics, invalid profiles, parity
mismatches and build-stream incompatibilities all reject only PGO before
screening; the integration matrix and retained dogfooding pass; EP-001 and
EP-002 are reviewed and marked `DONE`.

#### US-005: Validate the missing-function diagnostic contract

**Description:** As a Temper maintainer, I want a pinned reproduction of LLVM's
missing-profile diagnostic so that production classification is based on
observed structured evidence.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-004

**Acceptance Criteria:**

- [ ] A real-toolchain fixture deliberately applies incompatible profile data
  while passing `-Cllvm-args=-pgo-warn-missing-function`.
- [ ] The fixture captures Cargo's raw JSON line and the parsed
  `compiler-message` fields needed to classify the warning.
- [ ] The fixture records rustc, LLVM and Cargo versions and passes when Cargo
  exits successfully while emitting the expected warning.
- [ ] A diagnostic fingerprint is defined from structured fields plus the
  narrowest required message fragment; unrestricted substring matching is
  prohibited.
- [ ] At least one unrelated compiler warning proves that the fingerprint does
  not classify general warnings as missing-profile evidence.
- [ ] If the supported toolchain emits no classifiable diagnostic, the story is
  `BLOCKED` and US-006 cannot start.

#### US-006: Reject PGO on missing-profile diagnostics

**Description:** As a Rust performance engineer, I want missing profile data to
invalidate the PGO candidate so that a silently partial optimization cannot be
promoted.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-005

**Acceptance Criteria:**

- [ ] The optimized phase includes
  `-Cllvm-args=-pgo-warn-missing-function`; the instrumentation phase does not.
- [ ] Parsed compiler diagnostics retain package ID, target identity, level,
  diagnostic code when present and bounded rendered text.
- [ ] Any diagnostic matching the US-005 fingerprint rejects PGO with reason
  `pgo_missing_profile_data`.
- [ ] The rejection record contains the matched structured diagnostic and the
  optimized build evidence.
- [ ] A rejected PGO candidate receives zero screening samples, cannot enter
  confirmation and cannot update `.temper/latest.json`.
- [ ] Unrelated warnings retain their existing Cargo behavior and do not produce
  `pgo_missing_profile_data`.
- [ ] After this PGO rejection, valid static candidates remain eligible for
  selection and confirmation.

#### US-007: Validate raw profiles and enforce strict merge

**Description:** As a Temper maintainer, I want every profile input and merge
outcome recorded so that corrupt or ambiguous training data cannot reach the use
phase.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-004

**Acceptance Criteria:**

- [ ] Raw profiles are discovered in deterministic path order and each record
  contains canonical path, byte length and SHA-256.
- [ ] Zero files, more than 10,000 files, a non-regular file or a symlinked
  profile reject PGO before `llvm-profdata` starts.
- [ ] `llvm-profdata merge` receives an explicit `--failure-mode=any` and the
  exact ordered arguments are persisted.
- [ ] A corrupt input, nonzero merge exit or any merge stdout/stderr rejects PGO
  with bounded diagnostics.
- [ ] A successful merge requires one regular `.profdata` inside the run's PGO
  directory with nonzero size and a recorded SHA-256.
- [ ] Missing, empty, symlinked or out-of-run merged output rejects PGO before
  the optimized Cargo build.
- [ ] Given merge rejection, static strategy selection continues and no
  optimized PGO build starts.

#### US-008: Execute the PGO compatibility integration matrix

**Description:** As a Temper maintainer, I want one deterministic integration
matrix so that every supported and rejected PGO boundary remains executable.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-006, US-007

**Acceptance Criteria:**

- [ ] The matrix contains at least 12 named cases and reports each case
  independently.
- [ ] Supported cases include absent, string and array target rustflags; paths
  containing spaces; baseline, ThinLTO and FatLTO base strategies; a workspace
  package; and host build-script or proc-macro compilation.
- [ ] Rejection cases include ambient rustflags, a parity mismatch, JSON-looking
  unparsed output, zero/multiple artifacts, zero/corrupt profiles, merge
  diagnostics and a missing-function diagnostic.
- [ ] Every supported PGO case completes instrumentation, training, merge,
  optimized build and screening with a matched parity record.
- [ ] Every rejected PGO case records one stable reason, produces zero PGO
  screening samples and leaves valid static candidates eligible.
- [ ] Before/after checksums prove that target source, manifests and lockfiles
  are unchanged in every case.
- [ ] The matrix leaves no workload descendants and no symlink escape outside
  its temporary roots after timeout or failure.

#### US-009: Close PGO hardening with retained dogfooding evidence

**Description:** As a Temper maintainer, I want retained real-toolchain evidence
so that corpus work starts only after the production path is reviewed as DONE.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-008

**Acceptance Criteria:**

- [ ] Dogfooding runs at least four full real-toolchain PGO paths covering a
  single binary, a multi-package workspace, string rustflags and a host build
  script or proc macro.
- [ ] At least one deliberately incompatible-profile path proves
  `pgo_missing_profile_data` with zero PGO screening samples.
- [ ] Fixture source, workload, toolchain fingerprint and every raw `run.json`
  are retained under a dated PGO-hardening evidence directory.
- [ ] Every retained artifact and profile checksum referenced by a cited result
  is independently rechecked.
- [ ] `docs/verification-v0.0.1.md` receives an append-only dated v0.0.2 section
  that separates correctness evidence from performance claims.
- [ ] Synthetic or deliberately manipulated timings are labeled as controls and
  produce no optimization-gain claim.
- [ ] A corpus-readiness record enumerates every EP-001 and EP-002 criterion,
  contains no unresolved failure and states that US-010 remains blocked until
  independent `/review-epic` runs mark both epic statuses `DONE`.

---

### EP-003: Gated benchmark corpus v1

Create a versioned corpus only after PGO hardening is DONE, then execute it on
the reference host with correctness oracles and retained per-case evidence.

**Definition of Done:** The mechanical gate was satisfied before any corpus
file was created; corpus v1 contains three qualifying real applications and one
excluded synthetic control; every case has provenance, license, checksums,
weighted scenarios and an oracle; three valid raw runs per case are retained;
the verification ledger contains no aggregate cross-application score.

#### US-010: Enforce the corpus gate and define manifest schema 1

**Description:** As a Temper maintainer, I want corpus creation mechanically
blocked by PGO hardening so that benchmark evidence cannot precede pipeline
correctness.

**Priority:** P1
**Size:** M (3 pts)
**Dependencies:** Blocked by US-001, US-002, US-003, US-004, US-005, US-006, US-007, US-008, US-009

**Acceptance Criteria:**

- [ ] Before any file is created, the story verifies that EP-001 and EP-002 are
  both `DONE` and that US-001 through US-009 are `DONE` in the status tracker.
- [ ] Given any unmet status, corpus setup exits without creating
  `benchmarks/corpus/v1`, running a workload or emitting a score.
- [ ] `benchmarks/corpus/v1/manifest.json` declares
  `corpus_schema_version: 1`, a corpus identifier, a changelog version and an
  ordered case list.
- [ ] Each case schema requires ID, real or synthetic classification, workload
  class, source provenance, package/bin selection, source and lockfile
  checksums, license metadata, scenarios, oracle, resource bounds and expected
  evidence paths.
- [ ] A repository test rejects duplicate IDs, unknown schema versions, missing
  fields, invalid SHA-256 strings, nonpositive weights and paths escaping the
  corpus root.
- [ ] Corpus v1 documents that an entry change requires a new corpus version;
  historical manifests and result records are immutable.

#### US-011: Curate representative and licensed corpus cases

**Description:** As a Temper maintainer, I want bounded application diversity
with redistribution evidence so that corpus-v1 observations have an auditable
scope.

**Priority:** P1
**Size:** L (5 pts)
**Dependencies:** Blocked by US-010

**Acceptance Criteria:**

- [ ] Corpus v1 contains at least three source snapshots of real public Rust
  binaries and one synthetic control excluded from representative-case counts.
- [ ] The real cases cover exactly one or more of each minimum class:
  compute-heavy, parsing/transformation and streaming/compression.
- [ ] Each real case records upstream URL, immutable revision, snapshot
  SHA-256, `Cargo.lock` SHA-256, SPDX expression, copied license notices and a
  written redistribution determination.
- [ ] Each case contains no secret, credential, personal dataset or automatic
  network operation, with a dated manual review recorded in its provenance.
- [ ] Each source snapshot is at most 25 MiB and the complete corpus-v1 source
  plus input payload is at most 100 MiB.
- [ ] Each real baseline workload has a reference-host median between 100 ms and
  10 s, measured during curation but not cited as an optimization result.
- [ ] Any case lacking license permission, lockfile, deterministic input,
  correctness oracle or one required checksum is excluded rather than marked
  representative.

#### US-012: Execute weighted workloads through correctness oracles

**Description:** As a Rust performance engineer, I want each corpus workload to
validate the candidate while exercising declared scenarios so that training and
measurement use the same bounded behavior.

**Priority:** P1
**Size:** L (5 pts)
**Dependencies:** Blocked by US-011

**Acceptance Criteria:**

- [ ] Every real case defines at least two named scenarios with integer weights
  from 1 through 100 and documents why each scenario represents expected use.
- [ ] The workload executes `TEMPER_BINARY` according to the declared weights
  and validates every invocation through exit status plus semantic output or
  expected-output SHA-256.
- [ ] Inputs are local, immutable during a run and verified against manifest
  SHA-256 values before the first build.
- [ ] A corpus runner stages each case into a clean temporary Git repository,
  invokes the checked Temper binary with explicit package/bin selection and
  copies raw `run.json` evidence to a versioned result directory.
- [ ] The runner performs no automatic network access, shell `eval`, source
  manifest edit or lockfile edit.
- [ ] An oracle failure, timeout, output-limit breach, source checksum mismatch
  or nonzero Temper exit marks the case invalid, preserves diagnostics and makes
  the overall corpus command exit nonzero.
- [ ] The runner refuses execution when EP-001 or EP-002 is not `DONE`, even if
  corpus files already exist in a copied worktree.

#### US-013: Establish the corpus-v1 reference baseline

**Description:** As a Temper maintainer, I want retained reference-host results
so that future changes can be compared with per-case evidence and explicit
limitations.

**Priority:** P1
**Size:** L (5 pts)
**Dependencies:** Blocked by US-012

**Acceptance Criteria:**

- [ ] The reference host executes three independent complete Temper runs for
  each of the three real cases and the synthetic control.
- [ ] All twelve runs pass their correctness oracles and retain parseable
  schema-v2 `run.json` records.
- [ ] Each result directory records Temper commit, corpus version, case ID,
  source and input hashes, kernel, CPU, logical cores, Cargo, rustc, LLVM,
  workload argv and raw run identifier.
- [ ] A dated verification-ledger section reports baseline, selected candidate,
  confirmation ratio, confidence interval and decision separately for each real
  case.
- [ ] The synthetic control is labeled non-representative and is reported only
  as harness evidence.
- [ ] No arithmetic mean, geometric mean, ranking or universal speedup is
  computed across applications.
- [ ] If any real case fails its oracle, evidence requirements or three-run
  requirement, corpus v1 is not declared established and no result from that
  case is cited.
- [ ] Future source, input, workload, weight or oracle changes require corpus v2
  and do not overwrite corpus-v1 records.

## Functional Requirements

- FR-01: Temper must compose supported project and PGO rustflags through one
  process-local encoded environment channel.
- FR-02: Temper must preserve the observed order and argument boundaries of
  supported target rustflags.
- FR-03: Temper must retain an explicit `--target` for all PGO Cargo builds.
- FR-04: Ambient or ambiguous rustflag and compiler-wrapper sources must reject
  only PGO before Cargo starts.
- FR-05: JSON-looking unparsed Cargo output must fail the current build.
- FR-06: A successful build must expose exactly one matching
  `compiler-artifact.executable`.
- FR-07: New runs must persist report schema 2 and must not modify schema-1
  records.
- FR-08: Schema 2 must persist normalized instrumentation and optimization
  inputs plus their parity decision.
- FR-09: An unexpected phase-input difference must reject PGO before screening.
- FR-10: The PGO use phase must enable LLVM missing-function warnings.
- FR-11: A classified missing-function diagnostic must reject PGO before
  screening, confirmation and promotion.
- FR-12: Every raw profile must have canonical path, size and SHA-256 evidence.
- FR-13: Profile discovery must reject zero, excessive, non-regular or symlinked
  inputs.
- FR-14: Profile merge must use `--failure-mode=any` and reject any diagnostic
  output or invalid result.
- FR-15: PGO rejection must preserve valid static-strategy eligibility.
- FR-16: Corpus creation and execution must be blocked until EP-001 and EP-002
  are `DONE`.
- FR-17: Corpus v1 must contain at least three qualifying real applications and
  one excluded synthetic control.
- FR-18: Every corpus case must have pinned identity, provenance, license,
  checksums, scenarios, weights, bounds and a correctness oracle.
- FR-19: Corpus workloads must use local inputs and perform no automatic network
  access.
- FR-20: Corpus results must retain raw per-case records and must not compute an
  aggregate cross-application score.

## Non-Functional Requirements

- **Compatibility:** 100% of v0.0.2 production paths remain on stable Cargo and
  rustc CLI surfaces; nightly flags and compiler-library APIs used by production
  code remain at 0.
- **Build overhead:** PGO hardening adds 0 Cargo build invocations and 0 workload
  invocations to one optimization run relative to v0.0.1.
- **Diagnostic bounds:** Each Cargo build and each `llvm-profdata` invocation
  persists at most 64 KiB of combined diagnostics.
- **Profile bounds:** Profile discovery accepts at most 10,000 regular
  `.profraw` files per run and records 100% of their sizes and hashes.
- **Reliability:** Across the EP-002 rejection matrix, 0 rejected PGO candidates
  receive a screening sample or promotion.
- **Parity evidence:** 100% of PGO attempts reaching the optimized plan persist
  a matched or rejected schema-v2 parity record before screening.
- **Corpus size:** Each real source snapshot is at most 25 MiB and corpus-v1
  source plus inputs is at most 100 MiB.
- **Corpus duration:** Each real baseline workload has a reference-host median
  from 100 ms through 10 s; the top-level corpus runner uses the existing
  per-invocation timeout and completes 12 reference runs or exits nonzero.
- **Corpus reproducibility:** 100% of corpus cases verify source, lockfile and
  input SHA-256 before execution.
- **Evidence retention:** 100% of cited corpus results retain three schema-v2
  raw records and the complete host/toolchain fingerprint.
- **Security:** Corpus v1 contains 0 known secrets, credentials, personal
  datasets, automatic network calls or paths that resolve outside the case root.

## Edge Cases & Error States

| # | Scenario | Trigger | Expected Behavior | User Message |
|---|----------|---------|-------------------|--------------|
| 1 | String rustflags | Cargo config uses one string | Preserve arguments through the encoded channel or reject before build if boundaries are ambiguous | `PGO rejected: target rustflags cannot be composed without changing argument boundaries.` |
| 2 | Ambient rustflags | `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` is already set | Reject only PGO before Cargo starts | `PGO rejected: ambient rustflags outrank Temper's owned flag channel.` |
| 3 | JSON-looking parser fallback | Cargo stdout line begins with `{` but parses as `TextLine` | Fail the build and retain bounded line evidence | `Cargo emitted an unrecognized JSON build message.` |
| 4 | Artifact cardinality | Zero or multiple matching executables | Reject the build before measurement | `Cargo did not emit exactly one matching executable.` |
| 5 | Phase mismatch | Generate/use inputs differ outside the allowlist | Persist differences and reject only PGO | `PGO rejected: instrumentation and optimization inputs differ.` |
| 6 | Missing profile data | LLVM emits the validated missing-function diagnostic | Reject PGO with structured evidence | `PGO rejected: LLVM reported missing profile data.` |
| 7 | Empty profile set | Workload creates no `.profraw` | Reject PGO before merge | `PGO training produced no raw profiles.` |
| 8 | Corrupt profile | Any input makes strict merge fail | Reject PGO and preserve merge diagnostics | `PGO profile merge rejected at least one input.` |
| 9 | Symlinked profile | A profile or merged output is a symlink | Reject before use and do not follow it | `PGO profiles must be regular files inside the run directory.` |
| 10 | Interrupted training | Timeout or SIGINT stops the workload tree | Persist interruption and run no merge/use phase | `PGO training was interrupted before profile validation.` |
| 11 | Corpus gate closed | EP-001 or EP-002 is not `DONE` | Create no corpus file and run no benchmark | `Benchmark corpus work is blocked until PGO hardening is DONE.` |
| 12 | Unlicensed source | Redistribution permission or notice is missing | Exclude the case from corpus v1 | `Corpus case rejected: redistribution evidence is incomplete.` |
| 13 | Oracle failure | Candidate output or semantics differ | Mark the case invalid and exit corpus runner nonzero | `Corpus oracle failed; no performance result is valid.` |
| 14 | Checksum drift | Source, lockfile or input hash differs | Stop before Cargo build | `Corpus case identity does not match manifest checksums.` |
| 15 | Noisy result | Temper returns `no_improvement` or dispersion rejection | Retain the raw decision without claiming a gain | `No confirmed improvement for this corpus case.` |

## Risks & Mitigations

| # | Risk | Probability | Impact | Mitigation |
|---|------|------------|--------|------------|
| 1 | LLVM diagnostic wording changes across toolchains | Medium | High | US-005 pins structured evidence and blocks enforcement when no bounded fingerprint can be proved |
| 2 | Owned encoded rustflags diverge from Cargo config semantics | Medium | High | US-001 captures real rustc arguments for every supported form before US-002 changes production |
| 3 | Strict missing-profile rejection excludes valid partial profiles | Medium | Medium | Persist exact diagnostics, reject only PGO and retain static candidate eligibility |
| 4 | Schema 2 breaks consumers expecting schema 1 | Low | High | Increment the schema, update documentation/tests and never rewrite existing schema-1 files |
| 5 | Corpus applications do not represent intended users | Medium | High | Require three workload classes, explicit rationales, bounded claims and no aggregate score |
| 6 | Third-party source cannot be redistributed | Medium | High | Require permissive license evidence and exclude any case before snapshot inclusion |
| 7 | Workloads validate speed but not correctness | Medium | High | Require an oracle on every invocation and invalidate the whole case on one failure |
| 8 | Corpus execution is mistaken for sandboxed code | Medium | Medium | Document trusted-code execution, disable automatic network access and make sandboxing a non-goal |
| 9 | Corpus results depend on one host | High | Medium | Capture the full host fingerprint and prohibit cross-machine or universal claims |
| 10 | Corpus scope expands beyond one release | Medium | Medium | Cap v1 at three real cases plus one control, 100 MiB and 13 total PRD stories |

## Non-Goals

- Cargo-library, rustc-driver, rustc-public, query-system or codegen-backend
  integration.
- Nightly `-Zself-profile`, `-Zsection-timings`, `--unit-graph`, build-plan or
  compiler phase attribution.
- New optimization strategies, BOLT, AutoFDO, sample PGO, linker replacement or
  dynamic plugins.
- macOS, Windows, musl, non-x86_64, cross-compilation, libraries, examples,
  tests or benchmark targets.
- Changing measurement-v1, its sample counts, bootstrap method, dispersion
  limit or promotion threshold.
- Automatic download, Git clone, archive extraction, package installation,
  network service, database, daemon, CI scheduler or remote execution.
- Sandboxing third-party code, build scripts or workloads.
- An ecosystem-wide benchmark ranking, composite score, universal expected
  speedup or cross-machine comparison.
- Public crates.io publication or repository-wide license selection.

## Files NOT to Modify

- `tasks/prd-temper-v0.0.1.md` and
  `tasks/prd-temper-v0.0.1-status.json` - historical product and completion
  records must remain unchanged.
- `docs/measurement-v1.md` - statistical behavior is outside this PRD.
- Existing schema-1 `.temper/runs/*/run.json` records - no migration or rewrite.
- Existing historical sections of `docs/verification-v0.0.1.md` - new evidence
  is append-only.
- `/home/arthur/dev/rust` and `/home/arthur/dev/cargo` - reference codebases are
  read-only inputs, not implementation targets.
- `.codex/`, `.claude/`, plugin caches, sessions, databases and other
  app-managed state - outside the repository feature surface.

## Technical Considerations

- **Architecture:** Should PGO flags be scoped with `Command::env` using one
  owned `CARGO_ENCODED_RUSTFLAGS` value? Recommended: yes, after US-001 proves
  ordering, because it avoids Cargo's string/list config merge while
  `--target` preserves host isolation.
- **Build-message parsing:** Should Temper replace `cargo_metadata` parsing?
  Recommended: no. Retain 0.23.1, classify JSON-looking `TextLine` as
  incompatible and require exactly one artifact.
- **Data model:** Should parity and structured diagnostics extend schema 1 or
  create schema 2? Recommended: schema 2, because the new fields change what a
  successful PGO record proves.
- **Diagnostic matching:** Which structured fields remain invariant on the
  supported toolchain? US-005 must answer with a committed fixture before
  US-006 chooses the production fingerprint.
- **Profile merge:** Should failure behavior rely on the LLVM default?
  Recommended: no. Pass `--failure-mode=any` explicitly and persist it.
- **Corpus storage:** Should real projects be downloaded at runtime or stored as
  source snapshots? Recommended: snapshots capped at 25 MiB each, because this
  PRD prohibits automatic network access and requires reproducible identity.
- **Corpus runner:** Should a new Rust dependency or service coordinate cases?
  Recommended: no. Use repository scripts and existing Temper interfaces, with
  manifest validation covered by Rust tests using current dependencies.
- **Migration:** Should old run records be upgraded? Recommended: no. New runs
  use schema 2; old files remain historical evidence.

## Success Metrics

| Metric | Baseline (current) | Target | Timeframe | How Measured |
|--------|-------------------|--------|-----------|-------------|
| Supported target-rustflags forms completing full PGO | Array form covered; string form unproven | No-rustflags, string and array forms all pass with expected order | Before EP-001 DONE | Real Cargo integration fixtures and captured rustc argv |
| Missing-profile cases rejected before screening | 0 automated detections | 5 of 5 injected cases | Before EP-002 DONE | Integration records with `pgo_missing_profile_data` and zero samples |
| PGO attempts with persisted parity decision | 0% | 100% | Before EP-002 DONE | Schema-v2 run manifest audit |
| Suspicious Cargo JSON compatibility cases failing closed | 0 dedicated cases | 100% of at least 3 parser-fallback cases | Before EP-001 DONE | Build-stream integration fixtures |
| PGO boundary matrix | 6 EP-003 integration tests, not a complete matrix | At least 12 named cases with 100% expected outcomes | Before EP-002 DONE | Test matrix report |
| Retained v0.0.2 real-toolchain PGO paths | 0 | At least 4 plus 1 incompatible-profile control | Before EP-002 DONE | Dated hardening evidence directory |
| Real representative corpus cases | 0 | 3 across 3 required workload classes | Before EP-003 DONE | Corpus manifest validation |
| Synthetic controls correctly excluded | 0 versioned controls | 1 control with 0 representative claims | Before EP-003 DONE | Manifest and verification-ledger audit |
| Valid raw corpus records | 0 | 12 of 12 schema-v2 records | Before EP-003 DONE | Result-directory parser and checksum audit |
| Cited results with complete provenance | 0 | 100% | Month 1 and Month 6 | Verification ledger cross-check against manifest/results |
| Aggregate cross-application scores | 0 | 0 | Corpus v1 lifetime | Verification ledger review |

## Open Questions

- What exact structured diagnostic fields identify missing-profile warnings on
  the supported rustc/LLVM pair? Owner: US-005, required before US-006.
- Do Cargo string rustflags with embedded spaces preserve a usable boundary for
  the supported composition path? Owner: US-001, reject that form if the answer
  is no.
- Which three public applications satisfy the corpus size, license, workload and
  redistribution constraints? Owner: US-011, required before US-012.
- Can every selected application provide a deterministic oracle and at least two
  local weighted scenarios without source edits? Owner: US-011, exclude and
  replace any application that cannot.
- Does a selected corpus snapshot require notices beyond its SPDX expression
  and copied license files? Owner: US-011, resolve before the snapshot enters
  version control.
[/PRD]
