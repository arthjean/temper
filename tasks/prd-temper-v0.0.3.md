[PRD]
# PRD: Temper v0.0.3, Cargo-effective PGO inputs

## Changelog

| Version | Date | Author | Summary |
|---------|------|--------|---------|
| 0.0.3 | 2026-07-28 | Arthur Jean | Preserve Cargo-resolved compiler inputs through a transparent rustc shim and replace schema-2 projected parity with observed schema-3 evidence |

## Problem Statement

Temper v0.0.2 proves parity between the PGO inputs that Temper currently knows.
It does not prove parity between the compiler inputs Cargo actually resolves.
That distinction is now a correctness defect:

1. Cargo 1.94 stabilized recursive configuration `include`. Temper discovers
   Cargo home and ancestor `.cargo/config` or `.cargo/config.toml` files, but
   parses only their direct `build` and `target` tables. It does not follow the
   included configuration graph.
2. Cargo selects one rustflags source by precedence:
   `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, matching target or cfg rustflags,
   then `build.rustflags`. Temper sets its own `CARGO_ENCODED_RUSTFLAGS` during
   PGO, so it replaces every lower-precedence flag that its partial inspection
   did not copy.
3. A flag defined only in an included file can therefore affect the baseline
   and static candidates, disappear from both PGO phases, and still produce
   `phase_parity.matched = true` in schema 2.
4. The same projection-based design rejects ambient rustflags and compiler
   wrappers. Ambient rustflags can be observed at the final compiler boundary,
   but a compiler cache outside that boundary may key an artifact before seeing
   shim-appended PGO flags. Wrapper support therefore needs a cache-correctness
   contract, not only argv forwarding.
5. Adding more optimization strategies or expanding performance claims before
   fixing this boundary would amplify an invalid comparison across more builds
   and more retained evidence.

**Why now:** The post-v0.0.2 source survey found a stable Cargo feature that
invalidates an accepted schema-2 assumption. The defect affects the integrity
of every PGO result, while the relevant Cargo and rustc process interfaces are
stable and locally testable. Temper must close this proof gap before changing
its workload contract, widening its strategy space, or pursuing deeper compiler
integration.

## Overview

Temper v0.0.3 keeps Cargo as the owner of workspace resolution, configuration
merging, profiles, unit construction, wrappers, build scripts, fingerprints and
rustc invocation. Temper remains an external Cargo subcommand and does not
import Cargo or rustc internals.

The release replaces PGO flag substitution with compiler interposition for a
wrapper-free PGO subpipeline:

1. Temper resolves and records the active real rustc before starting a Cargo
   build.
2. Before interposition, Temper follows the Cargo config source graph far enough
   to detect compiler overrides and wrappers. Any ambient, direct or included
   compiler wrapper rejects only PGO; baseline and static candidates keep their
   existing Cargo behavior.
3. Temper performs a new fresh PGO reference build using the selected base
   strategy with no PGO flags. The reference, generation, use and confirmation
   builds set a process-local `RUSTC` value to Temper's own executable and
   supply a private, versioned shim protocol containing the real rustc path,
   stage, target triple, isolated target directory and capture directory.
4. The executable dispatches to the private shim before Clap parsing. The shim
   forwards probes and host invocations unchanged. It appends phase-specific
   PGO flags only when the final rustc argv represents a real compile for
   `x86_64-unknown-linux-gnu`, identified by the exact target plus an `--emit`
   argument.
5. The shim uses Unix process replacement to preserve rustc stdout, stderr,
   exit status and signals. It forwards argument bytes without shell parsing or
   lossy Unicode conversion.
6. Before process replacement, each shim invocation writes one bounded,
   create-new evidence record. The record contains structural fields and
   normalized digests, not the unrestricted raw argv.
7. After Cargo exits, Temper validates the complete capture set. PGO reference,
   generation and use parity is computed only after all three builds have
   executed, against the observed compiler inputs and a minimal explicit
   allowlist of phase differences.

Temper also follows Cargo `include` recursively for provenance and safety
checks. This scanner records source paths, hashes and include edges. It does not
merge Cargo values or use the graph to reconstruct effective rustflags.

New runs use report schema 3. Schema 1 and schema 2 records retain their
historical meaning and are never rewritten. The CLI remains
`cargo temper optimize`; the shim protocol is internal and is not a public
subcommand or compatibility promise.

## Goals

| Goal | Release Target | First 6 Months |
|------|----------------|----------------|
| Preserve Cargo-effective non-PGO compiler inputs | 100% of supported direct, included, cfg, build and environment rustflags fixtures preserve ordered target inputs across reference, generate, use and confirmation builds | 100% across the first 50 retained PGO attempts, with zero confirmed lost-input defects |
| Restrict PGO to target compilation | Zero PGO flags on probes, build scripts or proc macros in the complete integration matrix | Zero reported host-injection defects across the first 50 retained PGO attempts |
| Replace projected parity with observed evidence | 100% of PGO attempts persist complete schema-3 interposition and parity evidence before screening | 100% of retained PGO attempts remain auditable from `run.json` without schema reinterpretation |
| Fail closed on unknown compiler drift | 100% of at least 10 injected missing, duplicate, corrupt or mismatched capture cases reject only PGO | Zero promotion from an incomplete or unmatched compiler evidence set |
| Make the compiler-wrapper boundary explicit | 100% of ambient, direct and included general or workspace wrapper fixtures reject only PGO before the PGO reference build | A targeted follow-up either proves cache-key separation for a wrapper class or retains the rejection boundary |
| Bound interposition cost | Median paired cold-build overhead is at most 5% on the defined integration fixture, with identical direct-versus-pass-through executable SHA-256 | Median overhead remains at most 5% on the maintained compatibility fixture |

## Target Users

### Primary: Rust performance engineer

- **Role:** A developer optimizing a CPU-bound or throughput-sensitive Cargo
  binary with project-specific rustflags and possibly a compiler cache.
- **Behaviors:** Uses Cargo release configuration, target cfg values, local
  `.cargo` files, wrappers such as `sccache`, and executable workloads.
- **Pain points:** A PGO build can silently drop effective project flags;
  wrapper compatibility has no cache-safety proof; a green parity field does
  not expose the actual compiler boundary.
- **Current workaround:** Runs Cargo verbosely, captures rustc commands, disables
  wrappers, flattens config files and manually compares phase arguments.
- **Success looks like:** Wrapper-free projects preserve their Cargo-effective
  flags, every PGO flag is target-scoped, and wrapper projects receive a precise
  nonfatal PGO rejection while static optimization remains available.

### Secondary: Temper maintainer

- **Role:** A maintainer evaluating whether a compiler integration remains
  correct across Cargo and rustc releases.
- **Behaviors:** Reviews source-level contracts, maintains fixtures, audits
  `run.json`, and qualifies changes against local Rust and Cargo checkouts.
- **Pain points:** Partial Cargo configuration parsing ages poorly; hidden
  compiler inputs make regressions difficult to reproduce; unrestricted argv
  capture would create size and privacy risk.
- **Current workaround:** Adds another configuration special case and infers
  parity from the inputs Temper generated.
- **Success looks like:** Stable external-process contracts, bounded observed
  evidence, an explicit normalization allowlist, and fixtures that fail when
  Cargo changes the relevant invocation shape.

## Research Findings

### Verified Product and Codebase Findings

- Temper v0.0.2 sets one owned `CARGO_ENCODED_RUSTFLAGS` value by concatenating
  its parsed project flags and PGO flags
  ([`src/strategy.rs`](../src/strategy.rs)). Cargo receives those environment
  overrides in [`src/cargo.rs`](../src/cargo.rs).
- `effective_target_rustflags` reads only directly discovered config files,
  rejects some direct ambiguous values and never resolves `include`
  ([`src/strategy.rs`](../src/strategy.rs)).
- Schema 2 records those inspected sources and known project rustflags, then
  compares generation and use before the optimized build has provided observed
  rustc inputs ([`docs/schema-v2.md`](../docs/schema-v2.md)).
- The complete cross-codebase finding, horizons and rejected directions are in
  [`docs/research/temper-post-v0.0.2-opportunity-survey.md`](../docs/research/temper-post-v0.0.2-opportunity-survey.md).

### Primary Cargo and rustc Contracts

- Cargo 1.94 stabilized the top-level `include` key. Included files load
  recursively from left to right, then the including file overrides them.
  Paths are relative to the including file and optional entries may be absent.
  Sources: [Cargo 1.94 changelog](https://doc.rust-lang.org/cargo/CHANGELOG.html#cargo-194-2026-03-05)
  and [Cargo configuration includes](https://doc.rust-lang.org/cargo/reference/config.html#including-extra-configuration-files).
- Cargo rustflags sources are mutually exclusive by precedence:
  `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, matching target and cfg tables, then
  `build.rustflags`. With explicit `--target`, target flags do not apply to host
  build scripts or proc macros. Source:
  [Cargo build rustflags](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags).
- `RUSTC_WRAPPER` wraps all rustc invocations;
  `RUSTC_WORKSPACE_WRAPPER` wraps workspace members only. When both exist,
  Cargo executes `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC`. Only the
  workspace wrapper has a documented artifact filename-hash effect. Sources:
  [Cargo wrapper configuration](https://doc.rust-lang.org/cargo/reference/config.html#buildrustc-wrapper)
  and [Cargo environment variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html).
- Cargo's source distinguishes target units from host build scripts and proc
  macros through `CompileKind`. Target rustc commands receive the requested
  `--target`; target-info probes may also contain `--target` but use `--print`
  rather than compilation `--emit`
  (`/home/arthur/dev/cargo/src/compiler/compile_kind.rs`,
  `/home/arthur/dev/cargo/src/compiler/mod.rs`,
  `/home/arthur/dev/cargo/src/compiler/build_context/target_info.rs`).
- Cargo hashes resolved unit rustflags, profile and compile kind for unit
  fingerprints. Flags appended by an external compiler shim are not part of
  that unit fingerprint. Cargo does fingerprint wrapper executables for its
  rustc information cache
  (`/home/arthur/dev/cargo/src/compiler/fingerprint/mod.rs`,
  `/home/arthur/dev/cargo/src/util/rustc.rs`).
- The rustc PGO guide requires instrumented compilation, representative
  execution, strict profile merge and optimized recompilation with equivalent
  non-PGO compiler flags. It recommends absolute profile paths, explicit
  `--target`, clean profile data and missing-function warnings. Source:
  [rustc profile-guided optimization](https://doc.rust-lang.org/rustc/profile-guided-optimization.html).

### Architecture Implication

There is no stable Cargo query for the fully resolved rustc argv or configured
wrapper chain. Importing Cargo would couple Temper to internal types and still
make external wrappers part of the command boundary. A same-binary `RUSTC` shim
observes the argv after Cargo resolution without reconstructing Cargo inputs.
However, when an outer compiler cache is present, the cache sees the pre-shim
argv and may not include the private PGO phase in its cache key. v0.0.3 therefore
uses the shim only after proving that no compiler wrapper is active.

The main cost is that shim-appended flags are invisible to Cargo unit
fingerprints. Temper's existing fresh target directory per build stage must
therefore become a tested hard invariant. No target directory from a direct,
static, generate, use or confirmation build may be shared or reused.

## Assumptions & Constraints

### Assumptions to Validate

- A real target compilation can be classified from exact
  `--target x86_64-unknown-linux-gnu` plus an `--emit` form, while Cargo probes,
  host build scripts and proc macros remain outside that class. US-002 validates
  this against the current Cargo source and executed fixtures before production
  injection.
- Replacing the shim with the real rustc through Unix `exec` preserves argument
  bytes, stdout, stderr, exit status and signals. US-002 validates normal,
  failing, interrupted and non-UTF-8 cases.
- Setting `RUSTC` to the Temper shim mechanically preserves Cargo wrapper
  ordering, but an outer cache may key the compile before Temper appends PGO
  controls. US-003 validates this risk and the fail-closed wrapper boundary
  instead of promising wrapper support.
- A strict normalized digest can compare effective non-PGO compiler inputs
  while permitting only target-root-derived paths, Cargo artifact identity
  fields and the documented PGO phase flags to differ. US-002 defines the
  observed candidate allowlist; US-009 implements and adversarially tests it.
- A pass-through shim produces the same executable bytes as direct rustc on the
  deterministic fixture and adds no more than 5% median paired cold-build
  overhead. US-003 measures both claims.
- The active real rustc can be resolved and identified before the process-local
  `RUSTC` override. A configured `build.rustc` remains outside this release
  because Cargo exposes no stable effective compiler query.

### Hard Constraints

- v0.0.3 retains the v0.0.2 platform boundary: Linux host
  `x86_64-unknown-linux-gnu`, Cargo binary targets, an existing `Cargo.lock`,
  release mode and `--locked`.
- `include` is supported as a stable Cargo input only with Cargo 1.94 or newer.
  Temper does not enable the earlier nightly `-Zconfig-include` feature.
- Cargo remains the owner of config merging and wrapper nesting. Temper may
  discover and hash include sources, but must not reconstruct effective
  rustflags by merging TOML.
- Every interposed PGO reference, generation, use and confirmation stage keeps a
  unique, initially absent target directory. Reuse, aliasing or a pre-existing
  directory is a hard failure before rustc interposition.
- Any nonempty ambient `RUSTC_WRAPPER` or `RUSTC_WORKSPACE_WRAPPER`, and any
  declared `build.rustc-wrapper` or `build.rustc-workspace-wrapper` in the
  discovered config graph, rejects only PGO before the PGO reference build.
  v0.0.3 does not reproduce scalar precedence to prove that a declaration is
  inactive, or classify wrappers as caching or non-caching.
- Every PGO flag uses an absolute path inside the current run. The adjacent
  `llvm-profdata` from the active rustc remains mandatory.
- Existing rustc profile generation, profile use or coverage controls in the
  observed final argv reject only PGO before conflicting flags reach rustc.
- Missing, duplicate, malformed, symlinked, out-of-root or over-budget compiler
  capture evidence rejects the affected build. Incomplete PGO evidence can
  never reach screening, confirmation or promotion.
- PGO rejection remains nonfatal to the bounded search. A valid static
  candidate may still be confirmed and promoted.
- New v0.0.3 runs use report schema 3. Schema-1 and schema-2 `run.json` and
  `latest.json` records are never migrated, rewritten or reinterpreted.
- Raw unrestricted compiler argv is not published in `run.json`. Persisted
  evidence contains ordered digests and bounded allowlisted structural fields.
- Workloads, Cargo build scripts and compiler wrappers are trusted arbitrary
  code. Temper provides no sandbox and performs no automatic network access.
- No new Rust dependency, service, daemon, database, nightly Cargo option,
  `rustc_private` component or Cargo library integration is introduced.
- Measurement-v1, the strategy set, screening, confirmation thresholds,
  promotion semantics and corpus-v1 representativeness boundary do not change.

## Quality Gates

These commands must pass for every user story:

- `cargo fmt --check` - verifies Rust formatting.
- `cargo check --all-targets --locked` - verifies compilation for every local target without changing the lockfile.
- `cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings` - rejects local Clippy and compiler warnings.
- `cargo test --all-targets --locked` - executes the complete local test suite.

Story-specific tests and retained evidence are additional gates, not
replacements for these commands.

## Epics & User Stories

### EP-001: Prove the compiler-interposition contract

Reproduce the schema-2 defect and close the empirical unknowns that could
invalidate a same-binary `RUSTC` shim before it becomes a production boundary.

**Definition of Done:** Retained evidence demonstrates the included-rustflag
loss on v0.0.2, target versus host classification, byte-transparent process
replacement, the outer-cache key risk, the wrapper rejection boundary,
target-directory isolation, executable identity and the required overhead
budget. Any failed assumption updates this PRD before EP-002 starts.

#### US-001: Reproduce the stable Cargo include loss

**As a** Temper maintainer, **I want** a minimal retained reproduction of the
schema-2 include defect **so that** the release fixes an executed failure rather
than only a source-level implication.

**Acceptance Criteria:**

- [ ] A fixture on Cargo 1.94 or newer defines a sentinel target rustflag only through a required included TOML file; direct `cargo build --release --locked --target x86_64-unknown-linux-gnu` shows the sentinel in the target rustc argv.
- [ ] The unmodified v0.0.2 implementation at commit `f1619f9201c2abe75043952fb278c0d0c2295034` preserves the sentinel for its baseline or static build and omits it from both PGO phases while schema-2 phase parity reports no unexpected difference.
- [ ] Nested required and missing optional includes are captured as separate source cases, with Cargo version, rustc version, host, source commit, commands and raw evidence retained under a dated `docs/evidence/` directory.
- [ ] A Cargo version below 1.94 is not described as supporting stable `include`; the evidence distinguishes a Cargo feature-version rejection from the Temper defect.
- [ ] No production code or schema behavior changes in this story; an inconclusive reproduction blocks US-004 and records the exact missing proof.

**Priority:** P0
**Size:** S
**Blocked By:** None

#### US-002: Validate shim classification, transparency and parity inputs

**As a** Temper maintainer, **I want** an executable shim experiment across
Cargo probe, host and target invocations **so that** the injection predicate and
normalization boundary are proven before implementation.

**Acceptance Criteria:**

- [ ] The experiment records Cargo's initial rustc probes, one build script, one proc macro, one target dependency and the selected binary; only real target compilations contain both the exact supported `--target` and a compilation `--emit`.
- [ ] A pass-through shim forwards arbitrary `OsString` argument bytes, stdin, stdout and stderr without shell parsing or lossy conversion, including one Linux non-UTF-8 argument fixture.
- [ ] Unix process replacement preserves success, nonzero exit and termination by signal; a missing real-rustc path or malformed private protocol fails before any fallback compiler is executed.
- [ ] Direct and pass-through builds in distinct fresh target directories produce selected executables with identical SHA-256 and matching Cargo artifact identity.
- [ ] The experiment enumerates every argv field that differs between the reference, generate and use builds; the proposed parity allowlist is explicit, minimal and rejects one mutation in every non-allowlisted field class.
- [ ] A target-info `--print` probe carrying `--target` receives no PGO classification; any observed probe or host ambiguity blocks US-004.

**Priority:** P0
**Size:** L
**Blocked By:** None

#### US-003: Validate the wrapper-cache boundary, fingerprints and overhead

**As a** Rust performance engineer, **I want** compiler caches and wrappers
rejected unless their PGO cache semantics are proven **so that** Temper cannot
reuse an artifact keyed before its phase flags were appended.

**Acceptance Criteria:**

- [ ] Fixtures for no wrapper, general wrapper, workspace wrapper and nested wrappers observe Cargo's documented `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC` order and show that the outer wrapper receives argv before the shim appends PGO controls.
- [ ] A caching-wrapper fixture demonstrates whether reference, generate and use can share the same outer cache key when only the private shim phase changes; the retained result justifies rejecting every wrapper in v0.0.3.
- [ ] Nonempty ambient wrappers and any direct-config or included-config general or workspace wrapper declaration are detected before the PGO reference build and reject only PGO, while baseline and static candidates retain their normal Cargo wrapper behavior.
- [ ] A configured `build.rustc` in a direct or included Cargo config is detected and rejected with a stable unsupported-compiler boundary rather than being silently overridden.
- [ ] Reusing, precreating, symlinking or aliasing a stage target directory is proven unsafe and rejected; fresh per-stage directories produce no cross-phase Cargo `fresh` reuse.
- [ ] At least 10 paired direct and pass-through cold builds of the defined fixture show median shim overhead at or below 5%, with raw timings and host identity retained.
- [ ] No wrapper-specific bypass, environment tweak or cache disable flag is added; future support requires a separate proof that the wrapper key includes the effective phase controls.

**Priority:** P0
**Size:** L
**Blocked By:** US-002

### EP-002: Interpose rustc without replacing Cargo inputs

Implement the private compiler shim, target-only PGO injection, bounded capture
protocol and hard target-directory isolation while preserving Cargo's resolved
arguments on the explicit wrapper-free PGO boundary.

**Definition of Done:** Every supported PGO reference, generation, use and
confirmation build executes through a byte-transparent shim, project rustflags
remain in their Cargo-selected order, PGO flags appear exactly once on target
compilations only, captures are complete and bounded, and any protocol or
evidence failure prevents candidate screening.

#### US-004: Add private same-binary rustc dispatch

**As a** Temper maintainer, **I want** the installed `cargo-temper` executable
to act as an internal compiler shim **so that** no second binary, dependency or
public CLI surface is required.

**Acceptance Criteria:**

- [ ] `main` checks a versioned private shim protocol before Clap parsing and dispatches ordinary `cargo temper` invocations exactly as before.
- [ ] The private protocol resolves a canonical real rustc, stage, target triple, fresh target root and capture root before starting Cargo; all paths remain inside the current run where required.
- [ ] Shim mode treats argv after the executable as opaque `OsString` values and replaces itself with the canonical real rustc on Unix while preserving stdin, stdout, stderr, exit status and signals.
- [ ] Shim-only environment values are removed from the real rustc child where they are no longer required, and no workload or shell evaluation is introduced.
- [ ] Missing, duplicate, unsupported-version or out-of-root private values terminate with one bounded diagnostic and a stable protocol failure; they never fall through to the public CLI.
- [ ] `cargo temper --help` and `cargo temper optimize` expose no internal shim subcommand or private environment contract.

**Priority:** P0
**Size:** M
**Blocked By:** US-001, US-002, US-003

#### US-005: Append PGO flags to resolved target compilations

**As a** Rust performance engineer, **I want** Temper to append PGO controls
after Cargo resolves project flags **so that** included, cfg, build and
environment rustflags are preserved without an outer cache hiding the phase.

**Acceptance Criteria:**

- [ ] A fresh PGO reference stage uses pass-through observation mode with the selected base strategy; PGO generation, use and confirmation append their phase flags only to invocations classified as real supported-target compilations.
- [ ] Instrumentation appends exactly one absolute `-Cprofile-generate=...`; use and confirmation append exactly one absolute `-Cprofile-use=...` plus the missing-function warning, preserving every pre-existing argv item and its order.
- [ ] `CARGO_ENCODED_RUSTFLAGS` is no longer owned or synthesized by Temper for PGO; Cargo-selected string, array, cfg, build, `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` inputs reach the shim unchanged.
- [ ] Pre-existing profile-generate, profile-use, instrument-coverage or equivalent split-form compiler controls in the final argv reject only PGO with `pgo_compiler_input_conflict` before rustc executes the conflicting command.
- [ ] Probes, build scripts and proc macros receive zero Temper PGO flags even when their argv contains a host triple or a similarly named path.
- [ ] A classification ambiguity or unsupported rustc argument shape rejects the affected PGO build; it never broadens the predicate or injects speculatively.

**Priority:** P0
**Size:** L
**Blocked By:** US-004

#### US-006: Capture compiler invocations safely under concurrency

**As a** Temper maintainer, **I want** one bounded evidence record per shim
invocation **so that** parallel Cargo builds can be audited without publishing
unrestricted compiler commands.

**Acceptance Criteria:**

- [ ] Each shim process writes one create-new regular file inside its stage capture directory before `exec`; filename collisions use a bounded retry policy and never overwrite an existing record.
- [ ] Each record contains protocol version, stage, structural crate fields, target/host/probe classification, ordered pre-injection digest, ordered post-injection digest, injected flag identifiers and normalization version.
- [ ] Digest construction uses raw Unix argument bytes with length framing; persisted structural values are allowlisted and bounded, and unrestricted raw argv is absent from `run.json`.
- [ ] The parent accepts at most 10,000 records and 32 MiB per stage, rejects symlinks, non-regular files, malformed records, duplicate record identities and paths outside the canonical capture root.
- [ ] Missing, corrupt, over-budget or unclassifiable capture evidence produces a stable build failure before a built artifact can be screened.
- [ ] Parallel fixture builds with at least 100 shim processes produce a complete deterministic aggregate across repeated runs; an injected collision or truncated record fails closed.

**Priority:** P0
**Size:** L
**Blocked By:** US-004

#### US-007: Integrate interposition with each isolated PGO stage

**As a** Temper maintainer, **I want** Cargo build orchestration to own one
consistent shim lifecycle **so that** baseline, candidates, PGO and confirmation
cannot bypass evidence collection or reuse hidden state.

**Acceptance Criteria:**

- [ ] `BuildPlan` assigns the PGO reference, generation, use and confirmation stages unique initially absent target and capture directories; canonical path aliasing across plans is rejected before Cargo starts.
- [ ] `cargo::build` applies process-local shim values, collects and validates capture evidence after Cargo exits, and attaches the aggregate to both successful and failed build records.
- [ ] A Cargo artifact is accepted only after its capture aggregate is complete; Cargo failure, shim failure, missing artifact or capture failure retains the existing bounded diagnostics plus the new stable reason.
- [ ] Baseline and static build orchestration remains unchanged; a wrapper rejection skips the PGO reference stage and preserves eligible static candidates.
- [ ] PGO confirmation rebuilds in a fresh target directory with the same observed non-PGO input contract and profile-use controls as the accepted optimized PGO build.
- [ ] Interruption during Cargo, capture validation or confirmation persists the durable run failure and never publishes `latest.json` or a promoted binary.
- [ ] Static-strategy behavior remains unchanged when PGO is rejected for a shim or capture reason.

**Priority:** P0
**Size:** M
**Blocked By:** US-005, US-006

### EP-003: Publish schema-3 observed compiler parity

Replace schema-2 projected inputs with a recursive configuration source graph,
observed compiler aggregates and strict post-build parity that cannot match when
evidence is absent or changed.

**Definition of Done:** Every new run is schema 3; every wrapper-free PGO attempt
records fresh reference, generation and use compiler evidence; parity is decided
after all three builds; only documented phase differences are allowed; all
other drift is a persisted nonfatal PGO rejection.

#### US-008: Version the config graph and compiler evidence as schema 3

**As a** Temper maintainer, **I want** an immutable schema-3 evidence model
**so that** future Cargo changes cannot silently alter the meaning of historical
PGO parity.

**Acceptance Criteria:**

- [ ] Schema 3 adds a versioned compiler-interposition record to every interposed PGO build, including real rustc identity, shim executable path and SHA-256, protocol version, classification counts, aggregate digests and injected-flag summary.
- [ ] Cargo config discovery follows required and optional `include` entries recursively in documented order, resolves relative paths against the including file, records canonical source hashes and include edges, and does not merge config values.
- [ ] A missing required include, cycle, malformed include entry, unreadable file or source mutation during hashing fails preflight or the affected build with a stable reason; a missing optional include is recorded without failure.
- [ ] Stable `include` on Cargo below 1.94 is rejected with the Cargo version boundary and no nightly opt-in; configs without `include` retain the prior supported Cargo boundary.
- [ ] Relevant compiler-input environment variable names and value digests are recorded without publishing unrestricted values; absence and empty values remain distinguishable.
- [ ] `docs/schema-v3.md` defines every field, bound, normalization rule and failure reason; schema-1 and schema-2 files and records remain byte-for-byte unchanged.

**Priority:** P0
**Size:** L
**Blocked By:** US-006

#### US-009: Compute strict compiler parity after PGO execution

**As a** Rust performance engineer, **I want** parity based on observed final
compiler inputs **so that** a PGO candidate cannot pass by comparing only
Temper's planned configuration.

**Acceptance Criteria:**

- [ ] A fresh pass-through reference build using the selected PGO base strategy, the instrumentation build and the optimized build each expose a deterministic multiset of target compiler input records keyed by stable logical crate identity.
- [ ] Parity is computed only after the optimized Cargo build and before any PGO screening sample; confirmation parity is checked before promotion.
- [ ] Normalization permits only documented stage target-root paths, Cargo artifact identity and output paths derived from those roots, profile-generate versus profile-use, and the use-only missing-function warning.
- [ ] Any added, removed, duplicated, reordered or changed non-allowlisted compiler input, crate identity, tool identity, config source hash, environment digest or observed argv rejects PGO with `compiler_input_mismatch`.
- [ ] At least 10 adversarial mutations across flags, cfg values, crate counts, target kind, tool path, included config, environment and record order each produce `matched: false` with the exact bounded difference class.
- [ ] No incomplete comparison can serialize `matched: true`; an unavailable reference or capture aggregate records `matched: false` and prevents screening.

**Priority:** P0
**Size:** L
**Blocked By:** US-007, US-008

#### US-010: Report actionable fail-closed compiler decisions

**As a** Rust performance engineer, **I want** concise compiler-input rejection
reasons in human and JSON reports **so that** I can restore compatibility
without reading raw Cargo commands.

**Acceptance Criteria:**

- [ ] Schema-3 JSON and human reports distinguish at least protocol failure, unsupported compiler override, input conflict, capture missing, capture corrupt, capture limit, config include failure and compiler parity mismatch.
- [ ] Every reason includes stage, affected logical crate or source when safely available, a bounded difference class and one actionable remediation without exposing unrestricted argv or environment values.
- [ ] The complete persisted compiler diagnostic and difference budget is bounded at 64 KiB per build; truncation is explicit and can never remove the stable reason.
- [ ] PGO-specific reasons reject only PGO and leave valid static candidates eligible; baseline or static interposition failures remain fatal because their build artifact is unproven.
- [ ] Emergency manifest, `run.json`, promotion manifest and `latest.json` durability ordering remains unchanged, with schema 3 written before any success pointer publication.
- [ ] `--json` emits exactly one valid schema-3 object on stdout for success and post-run failure; shim and Cargo diagnostics remain on stderr or in bounded fields.

**Priority:** P0
**Size:** M
**Blocked By:** US-009

### EP-004: Qualify Cargo compatibility and close v0.0.3

Exercise the new boundary across Cargo configuration sources, wrapper rejection,
host and target units, failure modes and retained dogfooding before declaring
the PRD implemented.

**Definition of Done:** Both compatibility matrices pass on the supported host,
the overhead budget is met, one clean include-based PGO run and one corpus-v1
case retain auditable schema-3 evidence, documentation matches the implemented
boundary, and no performance claim exceeds the evidence.

#### US-011: Execute the Cargo rustflags and include matrix

**As a** Temper maintainer, **I want** a stable integration matrix for Cargo
configuration provenance **so that** future Cargo releases cannot reintroduce
silent flag loss.

**Acceptance Criteria:**

- [ ] Fixtures cover no rustflags, direct string and array target rustflags, required include, nested include, missing optional include, target cfg, `build.rustflags`, `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS`.
- [ ] Precedence fixtures with multiple defined sources observe Cargo's winning source exactly and preserve its argument order through reference, PGO generation, use and confirmation.
- [ ] Included config changes between PGO phases, include cycles, malformed values and required missing files fail with the expected source or parity reason before screening.
- [ ] Direct, split-form and included profile or coverage conflicts all reject only PGO; no conflicting compiler invocation reaches real rustc.
- [ ] Build scripts and proc macros in every applicable fixture contain zero Temper PGO flags while target dependencies and the selected binary contain exactly one phase control.
- [ ] The matrix passes on the current supported toolchain and records the tested Cargo, rustc, host and Temper commits; failures on a newer Cargo version block compatibility claims until classified.

**Priority:** P0
**Size:** L
**Blocked By:** US-005, US-009

#### US-012: Execute the wrapper-rejection, process and evidence matrix

**As a** Temper maintainer, **I want** adversarial wrapper-rejection and process
coverage **so that** unsafe cache composition cannot enter the supported PGO
boundary.

**Acceptance Criteria:**

- [ ] The no-wrapper fixture completes reference, generation, use and confirmation with complete target captures; general, workspace, nested and `sccache`-like wrapper fixtures reject PGO before the reference build.
- [ ] Wrapper rejection is nonfatal to baseline and static candidates, identifies the active environment or config source without exposing its unrestricted value, and works through nested `include`.
- [ ] Probe, host, target, non-UTF-8, nonzero-exit, signal and interruption cases preserve their expected process behavior and never misclassify a host invocation.
- [ ] Duplicate execution, argv mutation in the test harness, corrupt capture, symlink capture, record collision and record-budget exhaustion each fail with the expected stable reason.
- [ ] Fresh target-directory enforcement rejects pre-existing, reused, symlinked and canonically aliased directories before Cargo execution.
- [ ] The paired overhead benchmark from US-003 is repeated on the production implementation and remains at or below 5% median with identical pass-through artifact SHA-256.
- [ ] Tests assert exact schema-3 bounds and reason codes without relying on unrestricted substring matching or target-directory artifact inference.

**Priority:** P0
**Size:** L
**Blocked By:** US-007, US-010

#### US-013: Close the release with retained evidence and documentation

**As a** Temper maintainer, **I want** clean-state dogfooding and an updated
product contract **so that** v0.0.3 claims match the evidence users can audit.

**Acceptance Criteria:**

- [ ] Package version and experimental CLI messaging are 0.0.3 and schema 3; `docs/v0.0.3.md`, `docs/schema-v3.md`, README current implementation and repository guidance agree on the supported boundary.
- [ ] A clean committed implementation runs the include-based fixture through PGO and retains source, config graph, toolchain, host, workload, raw `run.json` and artifact hashes under a dated evidence directory.
- [ ] One existing real corpus-v1 case completes with schema-3 evidence on the reference host; its workload remains labelled a bounded local proxy and no new production-representativeness claim is made.
- [ ] The verification ledger records the tested commit, dirty state, toolchain, host, workload definition, raw result location and whether each workload is synthetic or representative.
- [ ] No forced-promotion scenario is cited as an optimization gain, no aggregate corpus score is introduced, and no result lacking retained `run.json` is cited.
- [ ] The status tracker changes to `DONE` only after all prior stories are reviewed and every Quality Gate plus both compatibility matrices pass.

**Priority:** P1
**Size:** L
**Blocked By:** US-011, US-012

## Functional Requirements

### Compiler Interposition

- **FR-001:** Temper must resolve and identify the real rustc before applying a
  process-local `RUSTC` shim override.
- **FR-002:** The same installed `cargo-temper` binary must dispatch the private
  shim protocol before public CLI parsing.
- **FR-003:** Public CLI parsing and help must expose no shim subcommand.
- **FR-004:** The shim must forward raw Unix argv bytes and process streams
  without shell evaluation.
- **FR-005:** The shim must replace itself with the real rustc so exit and signal
  semantics are preserved.
- **FR-006:** A fresh PGO reference build plus PGO generation, use and
  confirmation must use observation mode through the shim; baseline and static
  orchestration remain unchanged.
- **FR-007:** Any nonempty ambient general or workspace compiler wrapper, and
  any wrapper declaration in a direct or included config, must reject only PGO
  before the reference build.
- **FR-008:** An effective custom `build.rustc` must be rejected until Temper can
  prove the real compiler identity without overriding project intent.

### Target-Scoped PGO

- **FR-009:** Temper must classify a PGO-eligible invocation only from exact
  supported target and compilation markers proven by EP-001.
- **FR-010:** Probe, build-script and proc-macro invocations must remain
  pass-through.
- **FR-011:** PGO generation, use and missing-function flags must be appended
  after the observed Cargo arguments on the wrapper-free boundary.
- **FR-012:** Temper must not synthesize `CARGO_ENCODED_RUSTFLAGS` for PGO.
- **FR-013:** Existing compiler profiling or coverage controls must reject only
  PGO before real rustc execution.
- **FR-014:** PGO profile paths must be canonical absolute paths inside the
  current run.

### Evidence and Parity

- **FR-015:** Every shim invocation must create one bounded evidence record
  before real rustc execution.
- **FR-016:** Evidence digests must frame and hash raw argument bytes in order.
- **FR-017:** Capture aggregation must reject missing, duplicate, malformed,
  symlinked, out-of-root and over-budget records.
- **FR-018:** Every interposed PGO stage must use a unique, initially absent
  canonical target directory and capture directory.
- **FR-019:** Schema 3 must record shim, real rustc, config source graph,
  environment-input digest, classification counts and aggregate compiler input
  evidence.
- **FR-020:** Parity must compare a fresh pass-through reference using the
  selected base strategy, PGO generation and PGO use only after their observed
  evidence exists.
- **FR-021:** Normalization must use a closed allowlist; every unknown
  difference must reject PGO.
- **FR-022:** Confirmation must revalidate the accepted PGO input contract
  before promotion.

### Cargo Configuration and Reporting

- **FR-023:** Config provenance must follow Cargo 1.94 include recursion,
  relative paths, ordering and optional entries without merging effective
  values.
- **FR-024:** Stable include on Cargo below 1.94 must produce an explicit version
  boundary and must not enable nightly behavior.
- **FR-025:** Human and JSON output must use stable bounded reason codes for
  compiler interposition and parity failures.
- **FR-026:** A PGO-only failure must preserve eligible static candidates.
- **FR-027:** New successful and failed runs must serialize schema 3; historical
  schema 1 and 2 data must remain untouched.
- **FR-028:** Success pointer publication and promotion must occur only after
  schema-3 evidence is durably persisted.

## Non-Functional Requirements

### Performance

- **NFR-001:** On the defined cold-build fixture, 10 or more paired builds must
  show median pass-through shim overhead at or below 5%.
- **NFR-002:** Direct and pass-through builds of the deterministic fixture must
  produce selected executables with identical SHA-256.
- **NFR-003:** Capture aggregation must process 10,000 valid records within
  2 seconds on the reference host after Cargo exits.

### Reliability and Correctness

- **NFR-004:** The integration matrix must observe PGO controls on 100% of
  expected target compilations and 0% of host or probe invocations.
- **NFR-005:** Every tested missing, duplicate, corrupt or mismatched evidence
  case must prevent screening and promotion.
- **NFR-006:** No schema-3 parity object may report `matched: true` without
  complete reference, generation and use evidence.
- **NFR-007:** Repeating an identical fixture build with fresh roots must produce
  the same normalized compiler-input aggregate digest.

### Resource Bounds

- **NFR-008:** Each build stage accepts at most 10,000 capture records and
  32 MiB of capture data.
- **NFR-009:** Persisted compiler diagnostics and parity differences are bounded
  at 64 KiB per build with explicit truncation.
- **NFR-010:** The shim creates no daemon, socket, background worker or file
  outside the current run and Cargo-owned target roots.

### Security and Privacy

- **NFR-011:** No workload, compiler argument or config value is executed through
  a shell.
- **NFR-012:** `run.json`, `--json` and human reports contain no unrestricted raw
  compiler argv or environment value.
- **NFR-013:** Capture validation rejects symlinks and any path escaping the
  canonical stage capture root.
- **NFR-014:** Existing trusted-code boundaries for Cargo build scripts,
  workloads and wrappers remain explicit in user documentation.

### Compatibility and Maintainability

- **NFR-015:** v0.0.3 must use only stable Cargo and rustc process interfaces on
  the supported host.
- **NFR-016:** The implementation introduces no new dependency and does not
  change `Cargo.lock`.
- **NFR-017:** Schema-3 normalization and reason codes must be documented and
  directly exercised by integration tests.
- **NFR-018:** Configuration source discovery and compiler evidence capture must
  remain separate modules from optimization selection and measurement logic.

## Edge Cases & Error States

### Validation and Configuration

| Case | Expected Behavior |
|------|-------------------|
| Cargo config has a required include that is missing | Reject preflight with source path and stable include failure |
| Optional include is missing | Continue and record the absent optional edge |
| Include graph contains a cycle | Reject preflight; do not partially accept the graph |
| Included file changes between discovery and build | Reject parity or provenance validation before screening |
| Cargo is below 1.94 and config uses include | Reject with the stable feature version boundary |
| Direct or included config selects `build.rustc` | Reject the unsupported compiler override without silently using another compiler |
| Existing argv contains PGO or coverage controls | Reject only PGO before executing the conflicting target compile |

### Process and Wrapper Behavior

| Case | Expected Behavior |
|------|-------------------|
| Private shim protocol is missing or malformed | Emit one bounded error, execute no fallback, fail the build |
| Cargo target-info probe carries `--target` | Pass through because it is not a compilation `--emit` |
| Build script or proc macro compiles for host | Pass through with no Temper PGO flag |
| General or workspace compiler wrapper is active | Reject only PGO before the reference build and preserve eligible static candidates |
| Wrapper is declared only in an included config | Detect its source through the include graph and reject only PGO |
| Real rustc exits nonzero | Preserve exit semantics and Cargo diagnostics |
| Real rustc is terminated by signal | Preserve signal semantics and persist interrupted or failed run state |
| Argument contains non-UTF-8 bytes | Forward bytes exactly and compute a length-framed digest |

### Concurrency, Integrity and Resource Limits

| Case | Expected Behavior |
|------|-------------------|
| Two shim processes choose the same capture name | Bounded create-new retry, then fail closed without overwrite |
| Capture is truncated or invalid JSON | Reject the build with `compiler_capture_corrupt` |
| Capture path is a symlink or escapes its root | Reject the build before reading it |
| More than 10,000 records or 32 MiB appear | Reject the build with `compiler_capture_limit` |
| Target directory already exists or aliases another stage | Reject before Cargo starts |
| Capture exists but Cargo emits no selected artifact | Preserve capture evidence and reject the missing artifact |
| Cargo succeeds but expected target capture is absent | Reject before screening |

### Reporting and Durability

| Case | Expected Behavior |
|------|-------------------|
| Parity has an unknown difference | Persist `matched: false`, reject PGO and retain bounded difference class |
| PGO fails but static candidate remains valid | Continue bounded selection with the static candidate |
| Baseline interposition fails | Fail the run because no comparable baseline exists |
| Interruption occurs during capture aggregation | Persist durable failure, publish no success pointer |
| Report budget truncates details | Preserve stable reason, stage, counts and explicit truncation marker |
| Historical schema-1 or schema-2 record is present | Leave it untouched and create a new schema-3 run |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Shim-injected flags are absent from Cargo unit fingerprints | Certain | Critical if target dirs are reused | Require unique initially absent target dirs for every stage; reject reuse and test Cargo `fresh` behavior |
| Argv normalization hides a meaningful compiler difference | Medium | Critical false parity | Derive a minimal allowlist from executed fixtures; adversarially mutate each field class; reject every unknown difference |
| Argv normalization is too strict for valid Cargo path differences | Medium | High false PGO rejection | Key by logical crate identity, normalize only target-root-derived artifact fields, retain bounded difference classes and expand only with proof |
| An outer compiler cache keys argv before shim-appended PGO flags | High | Critical cross-phase artifact aliasing | Reject every compiler wrapper for PGO in v0.0.3; retain static behavior; require a separate cache-key proof before support |
| Setting `RUSTC` changes build-script-observable environment | Medium | Medium transparency drift | Use the same mode across PGO reference and candidate stages, remove private values from real rustc, compare pass-through artifacts and document trusted build scripts |
| Concurrent capture files collide or are corrupted | Low | High missing proof | Create-new writes, bounded retries, canonical roots, per-record validation and deterministic aggregation |
| Raw compiler inputs expose paths or secrets | Medium | High privacy issue | Hash raw argument bytes, persist only allowlisted structural fields, bound diagnostics and never publish unrestricted environment values |
| Stable Cargo invocation shape changes | Medium | Medium compatibility break | Version the normalization contract, maintain integration matrices, record Cargo identity and fail closed on unknown shapes |
| Shim overhead distorts build cost or user experience | Low | Medium | Paired cold-build budget at 5%; no daemon or network; block release if budget fails |
| Include scanner becomes a second Cargo config engine | Medium | High architectural drift | Limit it to source graph discovery, hashes, version checks and unsupported compiler detection; never merge rustflags or profiles |
| Corpus dogfooding is mistaken for representative performance proof | Medium | Medium product overclaim | Retain existing proxy labels, cite no aggregate score and make no new runtime gain claim in this PRD |

## Non-Goals

- Adding ThinLTO variants, codegen-unit search, CPU targeting, AutoFDO, BOLT or
  any other optimization strategy.
- Changing measurement-v1, workload invocation, scenario weights, screening,
  confirmation or promotion thresholds.
- Separating PGO training and evaluation workloads. That remains the next
  product exploration after compiler input fidelity.
- Reimplementing Cargo configuration merging, profile resolution, unit graphs,
  fingerprints, scheduling or artifact layout.
- Importing Cargo as a library or using `cargo::Executor`, `--unit-graph`,
  `-Zbuild-analysis`, `rustc_driver`, `rustc_public` or any nightly compiler API.
- Providing a public rustc wrapper, plugin API, compiler-shim CLI or stable
  internal protocol.
- Supporting a custom `build.rustc`, alternate rustc-compatible compiler or
  non-Cargo build system.
- Supporting any general or workspace compiler wrapper during PGO. Baseline and
  static Cargo behavior remains available; wrapper-safe PGO needs a separate
  cache-key exploration.
- Widening beyond Linux `x86_64-unknown-linux-gnu`, host builds, Cargo binary
  targets and existing lockfiles.
- Adding sandboxing, remote execution, distributed cache, daemonization or a
  build-system replacement.
- Modifying corpus-v1 sources, inputs, licenses, workload definitions or
  representativeness claims.
- Publishing Temper, choosing its public license or promising 0.x compatibility.

## Files NOT to Modify

- `docs/measurement-v1.md` - the statistical contract is unchanged.
- `tasks/prd-temper-v0.0.1.md` and
  `tasks/prd-temper-v0.0.1-status.json` - immutable completed history.
- `tasks/prd-temper-v0.0.2.md` and
  `tasks/prd-temper-v0.0.2-status.json` - immutable completed history.
- `docs/schema-v2.md` - schema 2 keeps its historical meaning; add
  `docs/schema-v3.md` instead.
- Existing dated directories under `docs/evidence/` - new evidence uses a new
  dated v0.0.3 directory.
- `benchmarks/corpus/v1/cases/` and corpus manifests - the corpus contract is
  unchanged.
- Existing corpus-v1 raw run records - v0.0.3 dogfooding stores new evidence
  without rewriting historical runs.
- `Cargo.lock` - no dependency change is authorized.
- `/home/arthur/dev/rust` and `/home/arthur/dev/cargo` - they are read-only
  reference codebases for this PRD.

## Technical Considerations

### How should the shim preserve compiler selection?

Recommendation: resolve and identify the real rustc before setting the
process-local Cargo `RUSTC` override, then pass its canonical path through a
versioned private protocol. Preserve a valid ambient `RUSTC` only if it passes
the existing identity and adjacent-toolchain checks. Reject config-defined
`build.rustc` until a stable effective-compiler query exists.

### Should Temper use `RUSTC`, `RUSTC_WRAPPER` or `RUSTC_WORKSPACE_WRAPPER`?

Recommendation: use `RUSTC` on the wrapper-free PGO boundary. A global wrapper
would collide with the project's general wrapper; a workspace wrapper would
miss non-workspace target dependencies. The compiler slot reaches every target
dependency without requiring Temper to reconstruct Cargo rustflags.

### Should every build stage use the shim?

Recommendation: no. Keep existing baseline and static orchestration unchanged.
After selecting the PGO base strategy, perform a fresh pass-through PGO
reference build, followed by generation, use and confirmation through the same
shim. This gives an exact comparison boundary without expanding interposition
to runs where PGO is already rejected.

### How should target compilation be classified?

Recommendation: require the exact supported `--target` plus the compile form
proven by US-002, currently expected to include `--emit`. Explicitly exclude
`--print` probes. Treat any new ambiguous shape as unsupported and fail closed
rather than guessing from crate names or paths.

### How should compiler arguments be captured without leaking data?

Recommendation: normalize and hash raw Unix argument bytes in the shim using
length framing, then persist only digests and allowlisted fields such as logical
crate identity, target, crate types, stage and injection decision. Do not
serialize unrestricted argv or environment values to the public report.

### How should parity handle Cargo-generated output differences?

Recommendation: define a closed, versioned normalization allowlist from the
executed EP-001 matrix. Normalize only stage-root-derived output and dependency
paths, Cargo artifact identity fields proven to derive from those paths, and the
documented PGO phase controls. Any new difference class rejects PGO until
separately justified.

### How should compiler wrappers be supported?

Recommendation: reject every nonempty ambient wrapper and every wrapper
declaration discovered in direct or included config for PGO in v0.0.3. A
wrapper outside the shim can compute a cache key before seeing the appended PGO
flags, and no stable generic protocol proves otherwise. Preserve normal baseline
and static Cargo behavior. Explore wrapper support later with a cache-key proof
or an upstream surface, not wrapper-name heuristics.

### How should shim flags interact with Cargo fingerprints?

Recommendation: never rely on Cargo to fingerprint externally appended flags.
Use a unique initially absent target directory for each build and confirmation
stage, record the shim and real rustc identities, and reject any directory reuse
or aliasing before Cargo starts.

### Should Temper request a stable effective-input API from Cargo?

Recommendation: not as a dependency for v0.0.3. After schema-3 fixtures reveal
the minimum information Temper repeatedly reconstructs, prepare an upstream
proposal for a stable machine-readable effective compiler invocation or config
provenance surface. Do not block the correctness fix on that longer process.

## Success Metrics

| Metric | Baseline | Target | Timeframe | Measurement |
|--------|----------|--------|-----------|-------------|
| Included target rustflag preserved in PGO | Source analysis predicts loss; no executed fixture exists | 100% across direct, required, nested and optional include fixtures | Before US-011 review | Compare observed target compiler input digests and sentinel fields across reference, generate, use and confirmation |
| Host/probe PGO injection | Unproven beyond current Cargo rustflags behavior | 0 injected host, proc-macro, build-script or probe invocations | Before EP-002 DONE and first 50 retained PGO attempts | Aggregate schema-3 classification and injected-flag counts |
| Complete observed parity | Schema 2 compares projected known inputs | 100% of PGO attempts have complete schema-3 reference, generate and use aggregates; no incomplete `matched: true` | Release and first 50 retained attempts | Audit `run.json` parity objects and adversarial integration cases |
| Unknown-drift rejection | No actual-argv mismatch contract | 100% of at least 10 adversarial mutations reject before screening | Before US-009 review | Integration matrix by difference class and stable reason |
| Wrapper boundary | All ambient or configured wrappers are currently rejected by PGO through partial direct inspection | 100% of ambient, direct, included, general, workspace and nested wrapper fixtures reject only PGO before reference execution | Before US-012 review | Stable rejection reason, source provenance and absence of PGO capture files |
| Pass-through artifact identity | No shim exists | 100% SHA-256 equality on deterministic fixture | Before US-003 and US-012 review | Paired direct and pass-through fresh builds |
| Build overhead | No shim exists | Median at most 5% across at least 10 paired cold builds | US-003 prototype and US-012 production rerun | Retained monotonic wall-clock measurements on the reference host |
| False promotion from evidence failure | Schema 2 cannot observe omitted included flags | 0 across all missing, corrupt, duplicate, mismatched and over-budget cases | Release and first 6 months | Integration failures plus retained run audit |
| Historical schema mutation | Schema 1 and 2 records exist | 0 historical records rewritten | Release and ongoing | Git diff, fixture hashes and schema compatibility tests |

## Open Questions

No question blocks implementation after EP-001. If EP-001 invalidates a hard
assumption, this PRD must return to `DRAFT` and record the changed decision
before EP-002 begins.

Non-blocking follow-ups:

1. Which stable cache-key or outer-interposition proof would be sufficient to
   support `sccache` or another compiler wrapper without phase artifact aliasing?
2. Which minimal effective-config or compiler-invocation surface would be worth
   proposing upstream to Cargo once the schema-3 evidence model is exercised?
3. After v0.0.3 closes compiler fidelity, should the next PRD prioritize
   training/evaluation workload separation or a clean representative corpus
   expansion? The post-v0.0.2 survey currently ranks workload separation next.

[/PRD]
