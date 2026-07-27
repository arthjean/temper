[PRD]
# PRD: Temper v0.0.1

## Changelog

| Version | Date | Author | Summary |
|---------|------|--------|---------|
| 0.0.1 | 2026-07-27 | Arthur Jean | Initial PRD for the first experimental optimization loop |

## Problem Statement

1. Rust developers can combine release profiles, LTO, codegen-unit changes and LLVM PGO, but determining which combination improves a specific application requires manual builds, toolchain setup and repeated measurements.
2. Existing tools automate individual mechanisms or recommend static settings. They do not conservatively accept a produced binary only after measuring it against the same project baseline and an explicit workload.
3. Performance experiments can be invalidated through target-directory contamination, workload failures, host drift, measurement noise or an incorrect PGO tool. The resulting binary may show a lower duration without a reproducible improvement.
4. Temper currently has a product contract and no implementation. The first development milestone must prove the complete Cargo-to-measurement loop before adding more strategies, platforms or public stability guarantees.

**Why now:** Cargo exposes versioned metadata and JSON build events, LLVM PGO is available through the Rust toolchain, and Temper's repository already defines measurement, Cargo compatibility, explainability and reproducibility as its core principles. A bounded v0.0.1 can validate those principles without committing to a stable v1 interface.

## Overview

Temper v0.0.1 is an experimental Linux CLI installed as `cargo-temper` and invoked through:

```shell
cargo temper optimize --package my-package --bin my-binary -- ./scripts/temper-workload --dataset representative
```

Temper resolves one Cargo binary target, builds the project's existing release configuration as the baseline, evaluates two fixed static strategies, trains one LLVM PGO candidate from the best pre-PGO configuration, and confirms the strongest candidate against a fresh baseline. The workload is an executable plus literal arguments. Temper passes the candidate path through `TEMPER_BINARY`, measures the wall-clock duration of the whole workload process, and treats exit status zero as the workload's correctness contract.

A candidate is promoted only when a separate paired confirmation batch reports a default improvement of at least 2%, the 95% confidence interval clears that threshold, measured dispersion remains within the documented limit, and every workload invocation succeeds. Temper never edits the target project's sources, manifests or lockfile. Builds, measurements, decisions and produced binaries live under `.temper/` in versioned per-run records.

Version `0.0.1` is explicitly unstable. Its CLI, JSON schema and strategy set may change in later `0.x` releases. A v1 stability commitment is outside this PRD and requires sustained use by Arthur and external users.

## Goals

| Goal | Month-1 Target | Month-6 Target |
|------|---------------|----------------|
| Complete optimization runs on supported real Cargo projects | 5 projects, at least 90% completed without Temper orchestration failure | 30 projects, at least 95% completed without Temper orchestration failure |
| Prevent false binary promotion in A/A control experiments | 0 promotions across 20 controls | At most 1 promotion across 100 controls |
| Demonstrate a confirmed runtime improvement | At least 1 project with an upper 95% confidence ratio bound at or below 0.98 | At least 20% of completed projects with an upper 95% confidence ratio bound at or below 0.98 |
| Exercise the product outside its author | 1 primary dogfood user | 10 external users with at least 1 completed run each |
| Preserve decision provenance | 100% of completed runs contain a valid schema-v1 manifest | 100% of completed runs contain a valid schema-v1 manifest |

## Target Users

### Primary: Rust performance engineer

- **Role:** A developer optimizing a CPU-bound Rust CLI, service or systems binary.
- **Behaviors:** Uses Cargo release profiles, benchmark scripts, profilers and manual compiler flags. Can provide a command that executes and validates a representative workload.
- **Pain points:** Repeats builds and measurements manually, must align LLVM tooling, and lacks one record explaining why a candidate was retained or rejected.
- **Current workaround:** Combines `cargo build --release`, custom `RUSTFLAGS`, `cargo-pgo`, Criterion or shell benchmark loops, then copies a selected artifact manually.
- **Success looks like:** One command produces a baseline, evaluates the bounded strategy set, rejects unsupported or noisy cases, and returns a verified artifact path plus complete measurements.

### Secondary: Rust project maintainer

- **Role:** A maintainer evaluating whether advanced release settings justify their build and operational cost.
- **Behaviors:** Owns a Cargo workspace and CI-ready workload but may not know rustc codegen details.
- **Pain points:** Static optimization advice is not specific to the application, while PGO setup and result interpretation require specialist knowledge.
- **Current workaround:** Copies profile settings from other projects or avoids PGO and LTO because the expected benefit is unknown.
- **Success looks like:** Receives a report showing baseline and candidate timings, uncertainty, build cost, rejected strategies and the exact accepted configuration without source changes.

## Research Findings

Key findings that informed this PRD:

### Competitive Context

- [`cargo-pgo`](https://github.com/Kobzol/cargo-pgo) automates LLVM PGO and experimental BOLT workflows. Temper differs by comparing several configurations and applying an independent promotion gate.
- [`cargo-wizard`](https://github.com/Kobzol/cargo-wizard) recommends intent-oriented Cargo profile settings such as runtime performance. Temper tests settings against the user's workload instead of treating a preset as evidence.
- [Cargo release profiles](https://doc.rust-lang.org/cargo/reference/profiles.html) expose stable LTO and codegen-unit controls with explicit runtime and build-time trade-offs.
- **Market gap:** the adjacent tools provide configuration or mechanism automation, but not a bounded baseline-search-confirm-promote loop with a durable rejection record.

### Best Practices Applied

- Cargo external tools should use [`cargo metadata --format-version 1` and JSON compiler messages](https://doc.rust-lang.org/cargo/reference/external-tools.html) rather than infer paths under `target/`.
- LLVM PGO is a multi-stage process of instrumentation, representative workload execution, profile merge and optimized rebuild, as documented by the [LLVM PGO guide](https://llvm.org/docs/HowToBuildWithPGO.html).
- Criterion's [measurement methodology](https://criterion-rs.github.io/book/user_guide/command_line_output.html) motivates warmup, sampling, confidence intervals, noise thresholds and explicit environmental caveats.
- Rust [`std::process::Command`](https://doc.rust-lang.org/std/process/struct.Command.html) passes arguments without a shell. Temper adopts direct execution and excludes Windows from v0.0.1 because batch files have different command-decoding risks.
- Cargo's [rustflags precedence](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags) means PGO flag injection can silently replace project flags. Temper fails closed whenever it cannot prove composition is safe.

## Assumptions & Constraints

### Assumptions (to validate)

- The elapsed wall-clock time of the complete workload command tracks application runtime when process and script overhead remain below 1% of the median measured duration.
- Twenty paired confirmation observations and a bootstrap confidence interval can hold the A/A false-promotion rate to at most 1% under the supported test environment.
- A project-specific workload represents the deployment behavior closely enough for PGO training and candidate selection.
- Cargo metadata plus compiler-artifact JSON messages can resolve every supported binary without inspecting target-directory layouts.
- The active Rust toolchain contains a compatible `llvm-profdata` after installation of `llvm-tools-preview`.
- The three-candidate strategy set can produce an accepted or explicitly rejected comparison without combinatorial search.

### Hard Constraints

- v0.0.1 supports Linux `x86_64-unknown-linux-gnu`, the host target, Cargo binary targets and an existing `Cargo.lock`.
- Cross-compilation, virtual targets without executable artifacts, library-only packages, examples, tests and benchmark targets are rejected.
- The target project is built with `--locked`; Temper must not edit its sources, `Cargo.toml` or `Cargo.lock`.
- Workloads and Cargo build scripts are trusted arbitrary code. Temper provides containment by direct execution, timeout, output bounds and process-group termination, not sandboxing.
- The baseline preserves the project's effective release profile. Candidate changes are isolated to the documented strategy overrides.
- PGO is skipped unless Temper can prove that injected flags will not be displaced by `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS` or `build.rustflags`.
- Only the `llvm-profdata` adjacent to the active rustc target-lib directory is accepted. No implicit `PATH` fallback is allowed.
- At most three non-baseline candidates are evaluated: ThinLTO, Fat LTO with one codegen unit, and PGO based on the best pre-PGO configuration.
- Public publication is blocked until the repository has an explicit open-source license. Publishing to crates.io is outside this PRD.

## Quality Gates

These commands must pass for every user story:

- `cargo fmt --check` - verifies Rust formatting.
- `cargo check --all-targets --locked` - verifies compilation for every local target without changing the lockfile.
- `cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings` - rejects local Clippy and compiler warnings.
- `cargo test --all-targets --locked` - executes the complete local Rust test suite.

## Epics & User Stories

### EP-001: Experimental CLI and Cargo boundary

Create one executable Temper surface and make Cargo the authoritative source for project, target and artifact discovery.

**Definition of Done:** `cargo temper optimize` can validate a supported project, resolve exactly one binary target, build an isolated release baseline and persist its artifact identity without editing the target project.

#### US-001: Scaffold the v0.0.1 binary and CLI contract

**Description:** As a Rust developer, I want an explicitly experimental Cargo subcommand so that I can discover the supported optimization workflow without assuming v1 stability.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** None

**Acceptance Criteria:**

- [ ] The repository contains one Rust package at version `0.0.1` with a `cargo-temper` binary and the required workspace Clippy policy.
- [ ] `cargo temper optimize --help` documents package selection, binary selection, minimum improvement, timeout, dirty-source handling and the trailing workload command.
- [ ] Startup output identifies the CLI and report schema as experimental version 1 with no backward-compatibility promise during `0.x`.
- [ ] Given no workload after `--`, `optimize` exits with code 2, prints one actionable usage error and creates no `.temper/` directory.
- [ ] Given an unknown option, the process exits with code 2 and does not start Cargo.

#### US-002: Run deterministic preflight diagnostics

**Description:** As a Rust developer, I want unsupported environments rejected before compilation so that I do not spend build time on a run Temper cannot validate.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-001

**Acceptance Criteria:**

- [ ] Preflight records the host triple, kernel, CPU model, logical core count, `cargo -Vv`, `rustc -Vv`, manifest path and `Cargo.lock` SHA-256.
- [ ] Linux `x86_64-unknown-linux-gnu` with an existing lockfile passes the platform and project checks.
- [ ] A dirty Git worktree is rejected by default; `--allow-dirty` permits the run and records `source_reproducibility: dirty`.
- [ ] Given a missing Cargo executable, missing lockfile or unsupported host triple, preflight exits before any build and lists the failed prerequisite.
- [ ] Preflight never installs a Rust component, edits project files or prints inherited environment values.

#### US-003: Resolve one workspace binary through Cargo metadata

**Description:** As a workspace maintainer, I want Temper to select the intended package and binary using Cargo identities so that optimization never targets an inferred filesystem path.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-002

**Acceptance Criteria:**

- [ ] Temper invokes `cargo metadata --format-version 1 --locked` with the selected manifest path and parses package and target identities.
- [ ] A workspace with exactly one supported binary target is selected without additional target flags.
- [ ] `--package` and `--bin` resolve an exact package ID and binary target in a multi-package workspace.
- [ ] Given zero or multiple matching binaries, Temper exits before building and lists the valid package and binary choices.
- [ ] A library, example, test, benchmark or cross-target selection is rejected with the v0.0.1 support boundary.

#### US-004: Build and identify an isolated release baseline

**Description:** As a Rust developer, I want the baseline to match my existing release configuration so that every candidate is compared against the binary I would otherwise ship.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-003

**Acceptance Criteria:**

- [ ] Temper builds the selected target with `cargo build --release --locked --target x86_64-unknown-linux-gnu --message-format=json` in a run-specific target directory.
- [ ] No candidate-specific LTO, codegen-unit or PGO override is applied to the baseline.
- [ ] Non-artifact Cargo messages are retained as bounded diagnostics and are never interpreted as artifact paths.
- [ ] Given a failed Cargo process, no executable event or multiple matching executable events, the run is marked failed and no strategy or workload starts.
- [ ] The baseline executable path, SHA-256, size and build duration are written atomically to the run manifest.

---

### EP-002: Trusted workload and measurement protocol

Define a measurable subprocess contract, validate the statistical gate and handle every interruption without promoting partial results.

**Definition of Done:** Temper can execute a workload against any built artifact, collect bounded samples, distinguish unstable or failed results, and satisfy the documented A/A false-promotion target.

#### US-005: Validate assumption: measurement-v1 controls false promotion

**Description:** As a Temper maintainer, I want the proposed measurement protocol tested before search orchestration so that later stories depend on quantified error bounds rather than intuition.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-004

**Acceptance Criteria:**

- [ ] `docs/measurement-v1.md` defines two warmups, seven screening samples, twenty paired confirmation observations with alternating AB/BA order, 10,000 fixed-seed bootstrap resamples, a 95% confidence interval and a default 2% practical-improvement threshold.
- [ ] The protocol rejects a batch when relative median absolute deviation exceeds 10% for either confirmed artifact.
- [ ] Across 100 deterministic A/A control datasets, no more than one dataset is accepted as an improvement.
- [ ] Across 20 deterministic datasets with an injected 5% regression, zero regressions are accepted.
- [ ] Across 20 deterministic datasets with an injected 5% improvement and at most 2% relative dispersion, at least 18 improvements are accepted.
- [ ] If any bound fails, the story remains incomplete until thresholds are adjusted, documented as measurement-v1 and all three validation sets pass.

#### US-006: Execute a workload without a shell

**Description:** As a Rust developer, I want my workload executed with the candidate path and literal arguments so that quoting and shell expansion cannot change the measured command.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-004

**Acceptance Criteria:**

- [ ] Arguments after `--` are split into one executable and zero or more literal arguments passed through `std::process::Command`.
- [ ] The workload runs from the Cargo workspace root with `TEMPER_BINARY` set to the canonical candidate executable path.
- [ ] Paths and arguments containing spaces, quotes, wildcard characters and dollar signs reach the child unchanged.
- [ ] Exit status zero marks one invocation valid; any nonzero status rejects the current artifact with the captured bounded diagnostic.
- [ ] The default per-invocation timeout is 300 seconds, configurable from 1 to 86,400 seconds.
- [ ] Stdout and stderr are each limited to 1 MiB; exceeding either limit terminates the invocation and rejects the artifact.

#### US-007: Sample workloads and compute measurement-v1

**Description:** As a Rust developer, I want every artifact measured through one versioned algorithm so that screening and confirmation results can be compared and reproduced.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-005, US-006

**Acceptance Criteria:**

- [ ] Workload duration is measured with a monotonic clock and stored as integer nanoseconds for every valid invocation.
- [ ] Screening runs exactly two unrecorded warmups and seven recorded samples per artifact.
- [ ] Confirmation consumes paired baseline and candidate observations, computes the median ratio, relative median absolute deviation and 95% bootstrap interval defined by measurement-v1.
- [ ] The implementation produces deterministic estimates for a fixed input sample set and bootstrap seed.
- [ ] Empty samples, zero durations, arithmetic overflow and non-finite derived values return typed measurement errors without panic.
- [ ] An artifact above the dispersion limit is rejected as `unstable_measurement`, even when its median is lower than the baseline.

#### US-008: Terminate interrupted or bounded workload trees

**Description:** As a Rust developer, I want timeouts and interrupts to stop every workload descendant so that failed experiments do not leave processes running or partial winners.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-006

**Acceptance Criteria:**

- [ ] Every workload starts in a dedicated Linux process group.
- [ ] On timeout or SIGINT, Temper sends termination to the process group, escalates when required, reaps the direct child and returns within 2 seconds after escalation.
- [ ] An interrupted run is atomically marked `interrupted`; a timed-out artifact is marked `timeout`.
- [ ] No interrupted, timed-out or output-limited artifact can enter confirmation or promotion.
- [ ] A fixture that forks a sleeping descendant leaves zero matching descendant processes after Temper exits.
- [ ] Captured diagnostics never include the complete inherited environment.

---

### EP-003: Bounded strategy search and LLVM PGO

Evaluate a fixed static strategy set and one fail-closed PGO candidate without exposing a plugin system or mutating Cargo configuration.

**Definition of Done:** The orchestrator can build, screen and record ThinLTO, Fat LTO and one toolchain-matched PGO candidate while isolating failures to the affected strategy.

#### US-009: Build the fixed static strategy set

**Description:** As a Rust developer, I want Temper to evaluate a bounded set of stable Cargo profile changes so that each run performs no more than three candidate builds.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-004

**Acceptance Criteria:**

- [ ] The strategy model is a closed enum containing baseline, `thin-lto`, `fat-lto-cgu1` and `pgo`; no dynamic plugin interface is introduced.
- [ ] `thin-lto` overrides only `profile.release.lto = "thin"` through Cargo's documented configuration interface.
- [ ] `fat-lto-cgu1` overrides `profile.release.lto = "fat"` and `profile.release.codegen-units = 1`.
- [ ] Each strategy uses a separate target directory derived from its canonical strategy identity.
- [ ] The exact Cargo arguments, overrides, build duration, artifact checksum and outcome are persisted for each strategy.
- [ ] A static candidate build failure rejects only that candidate; a baseline failure remains fatal to the run.

#### US-010: Prove PGO prerequisites and build instrumentation

**Description:** As a Rust developer, I want PGO attempted only with composable flags and the matching LLVM tool so that instrumentation cannot silently ignore project configuration.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-002, US-009

**Acceptance Criteria:**

- [ ] Temper locates `llvm-profdata` relative to `rustc --print target-libdir` and verifies it is executable; it never selects a `PATH` fallback.
- [ ] Missing `llvm-profdata` rejects PGO with an instruction to install `llvm-tools-preview` and does not reject completed static strategies.
- [ ] PGO preflight rejects the strategy when `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS` or an effective `build.rustflags` source prevents proven composition.
- [ ] Compatible target-specific rustflags are preserved while `-Cprofile-generate=<run-profile-dir>` is injected through a target-scoped Cargo config override.
- [ ] Cargo is forced to the host target so build scripts and proc macros are not instrumented as target artifacts.
- [ ] An instrumentation build failure records `pgo_instrumentation_failed` and does not start PGO training.

#### US-011: Train, merge and rebuild the PGO candidate

**Description:** As a Rust developer, I want the same workload to train and measure PGO so that generated profile data reflects the declared application behavior.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-006, US-010

**Acceptance Criteria:**

- [ ] PGO starts from the lowest-median valid pre-PGO configuration, including the baseline when neither static strategy improves screening.
- [ ] The workload executes once against the instrumented binary with the same executable and arguments used for measurement.
- [ ] Temper rejects PGO when training fails or produces zero `.profraw` files.
- [ ] The toolchain-matched `llvm-profdata merge` creates one run-scoped `.profdata` file whose SHA-256 is recorded.
- [ ] A separate target directory rebuilds the selected configuration with `-Cprofile-use=<merged-profile>` and records the optimized executable.
- [ ] Merge warnings, stale-profile build failures or missing optimized artifacts reject PGO without deleting valid baseline or static results.

#### US-012: Orchestrate the complete bounded search

**Description:** As a Rust developer, I want one deterministic state machine to coordinate the experiment so that failures and decisions are visible at phase boundaries.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-007, US-009, US-011

**Acceptance Criteria:**

- [ ] The run transitions through `preflight`, `baseline_build`, `static_builds`, `screening`, `pgo_training`, `pgo_build`, `candidate_selection` and `confirmation`.
- [ ] At most three non-baseline candidates are built and measured in the fixed order ThinLTO, Fat LTO, PGO.
- [ ] Every completed phase is written atomically before the next phase starts.
- [ ] A failed non-baseline strategy records its reason and the remaining valid strategies continue.
- [ ] A baseline build failure or baseline workload failure stops the run and prevents confirmation.
- [ ] Repeated execution with the same canonical inputs produces the same strategy identities and directory layout, excluding the run ID.

---

### EP-004: Confirmation, artifact promotion and evidence

Independently verify the selected candidate, publish no false winner and make every outcome inspectable by humans and tools.

**Definition of Done:** A completed run ends in either a confirmed artifact or an explicit no-improvement result, with atomic files, checksums, complete measurements and end-to-end Linux fixtures.

#### US-013: Confirm the selected candidate against a fresh baseline

**Description:** As a Rust developer, I want the screened winner retested against a fresh baseline so that selection noise cannot promote the binary.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-012

**Acceptance Criteria:**

- [ ] Temper rebuilds the baseline and selected candidate in dedicated confirmation target directories using the frozen run configuration.
- [ ] Confirmation performs twenty paired observations with alternating AB/BA order according to measurement-v1.
- [ ] A candidate is eligible only when the upper 95% confidence bound is at or below `1 - min_improvement`, where the default minimum improvement is 2%.
- [ ] Both artifacts must remain at or below the 10% dispersion limit and every confirmation workload invocation must exit zero.
- [ ] If no candidate satisfies all gates, the run completes with `no_improvement`, exits with code 0 and promotes nothing.
- [ ] A candidate that was best during screening but fails confirmation is recorded as `confirmation_rejected` with its interval and rejection reason.

#### US-014: Promote one artifact atomically

**Description:** As a Rust developer, I want only the confirmed executable copied to a stable run location so that partial or rejected binaries cannot be mistaken for the result.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-013

**Acceptance Criteria:**

- [ ] The confirmed executable is copied to a temporary file under `.temper/runs/<run-id>/best/`, synced, assigned its original executable permissions and atomically renamed.
- [ ] The promoted artifact SHA-256 and source candidate identity match the confirmation manifest.
- [ ] The baseline and all candidate records remain available after promotion.
- [ ] `.temper/latest.json` is replaced atomically only after the artifact and final run manifest are durable.
- [ ] A copy failure, checksum mismatch, interrupt or insufficient disk space leaves the previous `latest.json` unchanged and marks the current run failed.
- [ ] Temper never overwrites a file outside `.temper/`.

#### US-015: Emit human and machine-readable reports

**Description:** As a Rust developer, I want a terminal result bounded to 25 lines when no failure occurs and a complete JSON record so that I can understand the decision and automate later analysis.

**Priority:** P0  
**Size:** M (3 pts)  
**Dependencies:** Blocked by US-004, US-007

**Acceptance Criteria:**

- [ ] The terminal report lists baseline and candidate status, build duration, screening median, confirmation median ratio, confidence interval, dispersion and rejection reason.
- [ ] `run.json` declares `schema_version: 1` and contains source state, Cargo/rustc/host fingerprint, selected target, workload executable and arguments, strategy configurations, raw sample durations, checksums and final decision.
- [ ] `--json` writes exactly one final JSON object to stdout with no ANSI sequences while progress and bounded diagnostics use stderr.
- [ ] `NO_COLOR` disables ANSI formatting in human output.
- [ ] Environment values, unrestricted child output and secret-like variables are absent from the report.
- [ ] Failed, interrupted and no-improvement runs still produce a schema-valid final manifest when the run directory was created.

#### US-016: Prove the v0.0.1 flow with end-to-end fixtures

**Description:** As a Temper maintainer, I want real Cargo and workload fixtures covering success and failure so that the first implementation proves its declared end-to-end contract.

**Priority:** P0  
**Size:** L (5 pts)  
**Dependencies:** Blocked by US-008, US-013, US-014, US-015

**Acceptance Criteria:**

- [ ] A checked-in Cargo workspace fixture proves single-target and explicit multi-target resolution through real Cargo JSON messages.
- [ ] An A/A fixture completes as `no_improvement` and promotes no artifact.
- [ ] A deterministic sample fixture proves the acceptance algorithm for a confirmed improvement without relying on scheduler timing.
- [ ] A PGO integration fixture completes instrumentation, training, merge and optimized rebuild when `llvm-tools-preview` is present, and records a nonfatal PGO rejection when it is absent.
- [ ] Timeout, nonzero workload, ambiguous target, dirty source, missing lockfile and interrupted promotion each have an end-to-end regression test.
- [ ] `docs/v0.0.1.md` documents installation from source, the exact workload contract, trust warning, supported platform, strategy set, report paths and every explicit non-goal.

## Functional Requirements

- FR-01: The system must expose `cargo temper optimize` through a `cargo-temper` binary at version `0.0.1`.
- FR-02: The system must accept one workload executable and literal argument vector after `--`; it must not evaluate a shell command string.
- FR-03: The system must support only Linux `x86_64-unknown-linux-gnu` host binary targets in v0.0.1.
- FR-04: The system must resolve packages and binaries through Cargo metadata format 1.
- FR-05: The system must discover built executables only through matching Cargo compiler-artifact JSON events.
- FR-06: The system must build with `--locked` and isolated target directories without editing target-project files.
- FR-07: The system must use the project's effective release profile as the baseline.
- FR-08: The system must evaluate ThinLTO, Fat LTO with one codegen unit, and at most one PGO candidate.
- FR-09: The system must pass each tested executable through `TEMPER_BINARY`.
- FR-10: The system must enforce per-invocation timeout, 1 MiB output bounds and Linux process-group cleanup.
- FR-11: The system must implement and version the measurement-v1 warmup, screening, paired confirmation, confidence and dispersion rules.
- FR-12: The system must use the same workload for PGO training and measurement.
- FR-13: The system must use only toolchain-local `llvm-profdata` and reject PGO when rustflag composition is unproven.
- FR-14: The system must continue after a non-baseline strategy failure and stop after a baseline build or workload failure.
- FR-15: The system must promote no artifact unless every confirmation and correctness gate passes.
- FR-16: The system must treat a valid no-improvement outcome as a successful command with exit code 0.
- FR-17: The system must write phase progress and final schema-v1 state atomically.
- FR-18: The system must provide bounded human output and a machine-readable JSON mode.

## Non-Functional Requirements

- **Measurement integrity:** Across 100 deterministic A/A control datasets, at most 1 may pass the promotion rule; across 20 datasets with a 5% regression, 0 may pass.
- **Promotion threshold:** The default minimum improvement is 2%; a confirmed candidate's upper 95% confidence ratio bound must be at most 0.98.
- **Dispersion:** Confirmation is invalid when either artifact's relative median absolute deviation exceeds 10%.
- **Process termination:** After timeout or SIGINT escalation, the direct child and its process group must be reaped within 2 seconds on the supported platform.
- **Output containment:** Captured stdout and stderr are each capped at 1,048,576 bytes per workload invocation.
- **CLI overhead:** Excluding Cargo and workload child processes, metadata resolution and plan construction must complete within 1 second for a workspace containing 100 packages on the reference development machine.
- **Memory:** The Temper parent process must remain below 150 MiB resident memory for a run with 4 artifacts and 100 total samples, excluding child processes.
- **Durability:** 100% of completed phase transitions and final promotions use temporary-file plus same-filesystem atomic rename semantics.
- **Source safety:** Automated tests must observe zero byte changes to the target project's tracked sources, manifests and lockfile across successful and failed runs.
- **Report completeness:** 100% of completed runs contain schema version, toolchain fingerprint, target identity, workload argv, raw samples, strategy outcomes and final decision.
- **Machine output:** `--json` emits exactly one JSON document on stdout and zero ANSI escape sequences.

## Edge Cases & Error States

| # | Scenario | Trigger | Expected Behavior | User Message |
|---|----------|---------|-------------------|--------------|
| 1 | Missing workload | No executable follows `--` | Exit before preflight and create no run directory | "Provide a workload executable after `--`." |
| 2 | Ambiguous target | Multiple binary targets match | List valid package and binary pairs; do not build | "Select one target with `--package` and `--bin`." |
| 3 | Missing lockfile | `Cargo.lock` does not exist | Reject before Cargo build | "Temper v0.0.1 requires an existing Cargo.lock." |
| 4 | Dirty source | Git reports changes and `--allow-dirty` is absent | Reject before build | "Commit or stash changes, or pass `--allow-dirty`." |
| 5 | Cargo build failure | Baseline or candidate Cargo exits nonzero | Baseline stops the run; candidate is rejected | "Cargo build failed for `<strategy>`; see bounded diagnostics." |
| 6 | Artifact mismatch | Zero or multiple matching executable events | Mark build invalid; never infer a path | "Cargo did not emit exactly one executable for `<target>`." |
| 7 | Workload failure | Workload exits nonzero | Reject artifact and retain logs within the output cap | "Workload rejected `<strategy>` with exit code `<code>`." |
| 8 | Workload timeout | Invocation reaches configured deadline | Terminate process group and reject artifact | "Workload exceeded `<seconds>` seconds." |
| 9 | Output overflow | Child emits more than 1 MiB on either stream | Terminate process group and reject artifact | "Workload output exceeded the 1 MiB limit." |
| 10 | Measurement noise | Relative median absolute deviation exceeds 10% | Reject confirmation and promote nothing | "Measurements are unstable; reduce host noise or lengthen the workload." |
| 11 | PGO tools absent | Toolchain-local `llvm-profdata` is missing | Skip PGO and continue static candidates | "Install `llvm-tools-preview` to evaluate PGO." |
| 12 | Rustflags conflict | PGO flag composition cannot be proven | Skip PGO and name the conflicting source | "PGO skipped because existing rustflags would make instrumentation ambiguous." |
| 13 | No improvement | Confirmation interval does not clear threshold | Complete successfully without promotion | "No candidate cleared the 2% confirmation threshold." |
| 14 | Interrupted run | User sends SIGINT | Stop child group, mark interrupted, preserve completed phases | "Run interrupted; no artifact was promoted." |
| 15 | Promotion failure | Disk full, checksum mismatch or rename failure | Preserve previous latest pointer and mark run failed | "Confirmed artifact could not be promoted atomically." |

## Risks & Mitigations

| # | Risk | Probability | Impact | Mitigation |
|---|------|-------------|--------|------------|
| 1 | Measurement noise produces a false winner | High | High | Block implementation on US-005, require independent paired confirmation and track A/A controls as a release metric |
| 2 | User workload does not represent production | Medium | High | Persist the exact argv, make representativeness an explicit user responsibility and avoid claims beyond that workload |
| 3 | PGO flags replace existing project rustflags | Medium | High | Follow Cargo precedence, inject target-scoped flags and fail closed on every unproven source |
| 4 | `llvm-profdata` does not match rustc LLVM | Medium | High | Accept only the binary adjacent to the active toolchain target-lib directory |
| 5 | Trusted workload damages local data or exposes secrets | Medium | High | Display trust warning, execute directly without shell, bound time/output and document that v0.0.1 is not a sandbox |
| 6 | Static strategy overrides interact with unusual Cargo profiles | Medium | Medium | Preserve baseline, isolate target directories, record exact overrides and reject only the affected candidate |
| 7 | First corpus shows no confirmed improvement | Medium | Medium | Treat no improvement as valid evidence; evaluate product correctness separately from optimization hit rate |
| 8 | Multiplatform process handling delays the first end-to-end loop | High | Medium | Limit v0.0.1 to Linux x86_64 and defer portability until dogfood validates the core |
| 9 | Generated artifacts consume excessive disk space | Medium | Medium | Record per-run paths, fail before promotion on write errors and document manual run-directory cleanup |
| 10 | Experimental interfaces are mistaken for v1 commitments | Medium | Medium | Mark CLI and schema experimental, version the schema and state that `0.x` may break compatibility |

## Non-Goals

Explicit boundaries for v0.0.1:

- No v1 compatibility or stability promise. v1 requires several months of use by Arthur and external users.
- No BOLT, AutoFDO, post-link optimizer or linker replacement.
- No Windows, macOS, musl, non-x86_64 host, cross-compilation or remote execution.
- No library, cdylib, example, test or benchmark target optimization.
- No optimization of compilation time, incremental development builds or Cargo dependency resolution.
- No integrated build system, Cargo fork or use of Cargo's internal Rust APIs.
- No dynamic strategy plugins, scripting API, daemon, service or database.
- No automatic source edits, profile edits, lockfile updates, component installation or deployment.
- No shell command workloads, sandbox, container isolation or untrusted-code guarantee.
- No custom metric protocol, energy metric, memory metric or hardware-counter objective.
- No resume of interrupted runs, distributed search or cross-machine comparison.
- No crates.io publication or release automation before an explicit license decision.

## Files NOT to Modify

- `.codex/` - app-managed state outside product implementation.
- `README_FR.md` - untracked user-owned translation.
- `README.md` - current long-term product contract; v0.0.1 implementation details belong in `docs/v0.0.1.md` unless Arthur explicitly requests a README update.
- Target-project source files, `Cargo.toml` and `Cargo.lock` used by fixtures or dogfood runs - Temper must prove non-mutation.

## Technical Considerations

- **Architecture:** Should the first implementation remain one Rust package with a thin CLI and shallow `cargo`, `workload`, `measurement`, `strategy`, `run` and `report` modules? Recommended: yes, because no current requirement forces a workspace or plugin abstraction. Engineering should confirm after US-004.
- **Cargo protocol:** Should implementation use the `cargo_metadata` crate for metadata and compiler messages or local Serde schemas for the consumed fields? Recommended: `cargo_metadata` if its current release preserves unknown fields and format-version 1 behavior; otherwise use narrow local schemas. Verify before adding the dependency.
- **Measurement:** Should Temper embed Criterion or own the bounded measurement-v1 calculations? Recommended: own the versioned calculation module while applying Criterion's documented methodology, because Temper needs direct access to subprocess samples and its own report schema.
- **PGO flags:** Can every relevant Cargo config source be enumerated well enough to prove rustflag composition? Recommended: start fail-closed, using target-scoped `--config` injection and explicit conflict detection. If proof is incomplete, skip PGO rather than weaken the claim.
- **Process groups:** Should Linux termination use a focused Unix process dependency or direct libc calls? Recommended: use one maintained dependency limited to process-group creation, signals and reaping without introducing an async runtime.
- **Data model:** Should run state use append-only events plus a projection or one atomically rewritten manifest? Recommended: one schema-v1 manifest rewritten atomically after each phase; current scale does not require event sourcing.
- **Artifact hashing:** Which SHA-256 implementation should be used? Recommended: a RustCrypto implementation with streaming file reads and no platform service dependency.
- **Migration:** No migration is required before v0.0.1. Future schema versions must never reinterpret an existing `schema_version: 1` record silently.

## Success Metrics

| Metric | Baseline (current) | Target | Timeframe | How Measured |
|--------|-------------------|--------|-----------|-------------|
| Supported real projects completing without Temper orchestration failure | 0, no implementation | At least 5 projects with at least 90% completion | Month 1 | Schema-v1 run manifests grouped by project |
| A/A false promotions | N/A, no measurement engine | 0 across 20 controls | Before v0.0.1 tag | Automated control-run report |
| A/A false-promotion rate | N/A | At most 1% across 100 controls | Month 6 | Versioned measurement corpus |
| Projects with a confirmed improvement of at least 2% | 0 | At least 1 | Month 1 | Confirmation confidence intervals in run manifests |
| External users completing a run | 0 | At least 10 | Month 6 | Opt-in issue, discussion or shared manifest, counted once per user |
| Completed manifests satisfying schema-v1 required fields | 0 | 100% | Before v0.0.1 tag and Month 6 | JSON schema validation in tests |
| Target-project mutation incidents | N/A | 0 across all automated fixtures | Before v0.0.1 tag | Before/after file checksums in end-to-end tests |
| Orphan workload process groups after timeout or SIGINT | N/A | 0 across 100 lifecycle tests | Before v0.0.1 tag | Linux process-table assertion in integration tests |

## Open Questions

- Does measurement-v1 meet the required A/A and injected-change bounds on Arthur's reference machine? Owner: US-005, required before US-007.
- Can effective Cargo `build.rustflags` provenance be proven across repository, ancestor and Cargo-home configuration without unstable Cargo APIs? Owner: US-010, required before enabling PGO.
- Which maintained Unix process dependency meets the 2-second process-group cleanup requirement with no unrelated runtime subsystem? Owner: US-008, decide during implementation.
- Does the selected current `cargo_metadata` release preserve all compiler-artifact fields and tolerate unrelated messages required by US-003 and US-004? Owner: US-003, decide before dependency lock.
- Which open-source license will govern public distribution? Owner: Arthur, required before any public v0.0.1 publication but not before local implementation.
[/PRD]
