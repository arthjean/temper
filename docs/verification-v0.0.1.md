# Temper v0.0.1 verification ledger

This document records completed validation and its evidentiary limits. It is
the durable source for test, dogfooding, and benchmark status. Product behavior
remains specified by [`../tasks/prd-temper-v0.0.1.md`](../tasks/prd-temper-v0.0.1.md)
and [`measurement-v1.md`](measurement-v1.md).

## Status snapshot

| Field | Value |
|---|---|
| Verification date | 2026-07-28 |
| Tested commit | `024b3899fd64a25084eaeff33301acf76b5f4d94` |
| PRD status | `DONE`, 4 epics and 16 stories complete |
| Host | Linux 7.1.5, `x86_64-unknown-linux-gnu` |
| CPU | AMD Ryzen 7 7800X3D, 16 logical cores |
| Rust | rustc 1.97.1, LLVM 22.1.6 |
| Cargo | cargo 1.97.1 |
| Verdict | All required gates and the documented dogfooding matrix passed |

The implementation was tested from a clean `main` worktree. Documentation added
after this snapshot did not change Rust source or test behavior.

## PRD quality gates

All four commands required by the PRD passed:

```sh
cargo fmt --check
cargo check --all-targets --locked
cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings
cargo test --all-targets --locked
```

The Rust suite executed 40 tests with 40 passes, 0 failures, and 0 ignored:

| Suite | Tests |
|---|---:|
| Unit tests across `src/` | 19 |
| EP-001 Cargo integration | 5 |
| EP-001 CLI integration | 2 |
| EP-002 workload integration | 3 |
| EP-003 strategy and PGO integration | 6 |
| EP-004 confirmation and promotion integration | 5 |

`cargo test --doc --locked` is not applicable because Temper is currently a
binary-only package with no library target. It is not a PRD quality gate.

## Dogfooding matrix

Five dependency-free, temporary Rust repositories were created outside the
Temper worktree:

1. A single CPU-bound binary.
2. A two-package workspace with `alpha/alpha` and `beta/beta-tool`.
3. A binary with a custom release profile and literal workload arguments.
4. A valid binary with nonzero, timeout, and output-limit workloads.
5. A deliberately non-compiling binary.

The repositories were committed locally before execution so clean and dirty
source handling could be tested. After verification they were moved to the
system trash. Their raw manifests and build trees are therefore not repository
artifacts.

Twelve scenarios were executed:

| Scenario | Expected boundary | Result |
|---|---|---|
| CPU-bound complete search | Baseline, ThinLTO, Fat LTO, PGO, confirmation | `no_improvement` |
| Explicit workspace selection | Select `beta/beta-tool` through Cargo metadata | `no_improvement` |
| Custom release profile and literal argv | Preserve profile and spaces, `$`, `*`, quote | `no_improvement` |
| Ambiguous workspace | Reject before run creation or build | Passed |
| Dirty source by default | Reject before run creation or build | Passed |
| Dirty source with `--allow-dirty` | Record dirty reproducibility and continue | `no_improvement` |
| Nonzero workload | Persist failure, skip confirmation and promotion | Passed |
| One-second timeout | Kill and reap the descendant process group | Passed |
| Missing `Cargo.lock` | Reject before run creation or build | Passed |
| Forced promotion | Confirm, copy, checksum, and update `latest.json` | `confirmed` |
| Stdout above 1 MiB | Terminate and persist bounded diagnostics | Passed |
| Invalid baseline source | Stop before strategies and workload | Passed |

### Measurement results

Durations are wall-clock nanoseconds for the whole workload process.

| Workload | Baseline screening median | Selected candidate | Confirmation median ratio | 95% CI | Decision |
|---|---:|---|---:|---|---|
| CPU-bound | 20,381,218 | PGO | 1.000215 | [0.999432, 1.001071] | `no_improvement` |
| Workspace `beta-tool` | 10,255,389 | ThinLTO | 1.000954 | [1.000017, 1.001488] | `no_improvement` |
| Custom release profile | 10,262,370 | ThinLTO | 1.000101 | [0.998909, 1.000966] | `no_improvement` |
| Dirty source with `/bin/true` | 5,189,685 | ThinLTO | 1.000411 | [0.997571, 1.001389] | `no_improvement` |
| Forced promotion | 55,793,646 | Fat LTO, CGU 1 | 0.184302 | [0.184050, 0.184550] | `confirmed` |

The acceptance threshold was 0.98. The first four results demonstrate
conservative rejection under synthetic workloads, not production performance.
The dirty `/bin/true` workload mostly measures process overhead and did not
produce PGO data.

The forced-promotion workload slept for 50 ms when the executable path denoted a
baseline and 5 ms otherwise. It exists only to verify the promotion machinery.
Its apparent 81.6% improvement is intentionally manufactured and must never be
reported as a compiler or strategy speedup.

### PGO evidence

Four full real-toolchain PGO paths completed:

- instrumented Cargo build;
- workload execution producing at least one `.profraw`;
- merge through the `llvm-profdata` adjacent to active rustc;
- optimized rebuild with `-Cprofile-use`;
- PGO candidate screening.

The dirty `/bin/true` scenario correctly rejected only PGO because the workload
did not execute `TEMPER_BINARY` and produced no `.profraw`.

## Cross-run audit

The nine scenarios that created run directories produced:

- 9 parseable schema-v1 manifests with unique run IDs;
- statuses: 1 `confirmed`, 4 `no_improvement`, and 4 `failed`;
- 43 recorded build artifacts whose on-disk SHA-256 values matched;
- 4 merged `.profdata` files whose SHA-256 values matched;
- exactly one `latest.json`, belonging to the confirmed run;
- no confirmation or promotion for nonzero, timeout, output-limit, or baseline
  build failures;
- no surviving timeout descendant process;
- no source, manifest, or lockfile mutation;
- no detected secret-like inherited environment assignment in persisted output.

For the custom profile project, a separate standard
`cargo build --release --locked` produced the same binary SHA-256 as Temper's
baseline. This confirms that the baseline preserved the effective release
profile in that scenario.

## What has not been benchmarked

Temper has no checked-in benchmark corpus, `benches/` directory, Cargo bench
target, or production application result. No claim has been established for:

- speedup on a representative real application;
- the PRD month-1 or month-6 project and user targets;
- cross-machine reproducibility;
- false-promotion behavior under live scheduler noise beyond the deterministic
  measurement cohorts in the test suite;
- macOS, Windows, musl, non-x86_64, or cross-compilation.

The next meaningful performance milestone is a versioned, checked-in benchmark
corpus with correctness-checking workloads and retained raw run records.

## 2026-07-28 v0.0.2 PGO-hardening evidence

This append-only section records correctness evidence for EP-002. It makes no
optimization-gain, representative-benchmark or release-readiness claim.

| Field | Value |
|---|---|
| Baseline commit | `024b3899fd64a25084eaeff33301acf76b5f4d94` |
| Tested worktree | Dirty EP-002 implementation, tree SHA-256 `726f10f73179d5753d8dd724984aa9eb7c88b78c816b19202199e570ad942859` |
| Host | Linux 7.1.5, `x86_64-unknown-linux-gnu` |
| Rust | rustc 1.97.1, LLVM 22.1.6 |
| Cargo | cargo 1.97.1 |
| Raw evidence | [`evidence/pgo-hardening/2026-07-28`](evidence/pgo-hardening/2026-07-28) |
| Classification | Synthetic correctness fixtures and one deliberately incompatible-profile control |

The four PRD quality gates passed:

```sh
cargo fmt --check
cargo check --all-targets --locked
cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings
cargo test --all-targets --locked
```

The full suite executed 62 tests with 62 passes, 0 failures and 1 ignored
collector test. The collector was then run explicitly and passed.

Four real-toolchain paths completed instrumentation, training, strict merge,
optimized build and PGO screening: a single binary, string target rustflags, a
multi-package workspace with a build script and proc macro, and array rustflags
under a path containing spaces. A fifth path changed fixture source only after
instrumented training. The resulting real LLVM warning was classified as
`pgo_missing_profile_data`; that PGO candidate received zero screening samples
and static candidates remained eligible.

Five raw `run.json` files, exact fixture inputs, workloads and the full
toolchain fingerprint are retained. Before temporary build directories were
released, 34 artifact checksum references, 10 raw-profile checksum references
and 5 merged-profile checksum references were independently recomputed and
matched. Counts include repeated references to the same raw profile in training
and merge records.

These results identify an uncommitted implementation by its baseline commit and
tree checksum. A clean committed-state rerun remains required before a release
claim. The synthetic timing decisions in the retained runs are controls and
support no performance claim.

## Repeating or extending verification

For code validation, run the four PRD gates above. For future dogfooding:

1. Record the current commit, kernel, CPU, rustc, LLVM, and Cargo versions.
2. Use a clean Git repository with an existing `Cargo.lock`.
3. Make the workload execute and validate `TEMPER_BINARY`.
4. Classify the workload as synthetic or production-representative before
   interpreting any score.
5. Retain `run.json`, workload source, project revision, and promoted artifact
   checksum in a stable evidence directory.
6. Append a dated result section here. Never replace historical results
   silently.

## 2026-07-28 EP-002 review remediation evidence

This append-only section records the post-review correction and rerun. It
supersedes the earlier dirty-tree evidence for EP-002 completion, but does not
alter or remove that historical record.

| Field | Value |
|---|---|
| Baseline commit | `024b3899fd64a25084eaeff33301acf76b5f4d94` |
| Production inputs | SHA-256 `980625bea61ce6cdfbfa6499946678b0ac1d3e7255153c84106932ed51479cbd` |
| Evidence harness | SHA-256 `17e216125fafa94f52d8ce24c715a92f75aa69f62ff101c1135081604743f45e` |
| Tested binary | SHA-256 `353478f50a81e09a60f0ebd0b093a080bd629a15dea67cdc54e08cf0d31a02be` |
| Host | Linux 7.1.5, `x86_64-unknown-linux-gnu` |
| Rust | rustc 1.97.1, LLVM 22.1.6 |
| Cargo | cargo 1.97.1 |
| Raw evidence | [`evidence/pgo-hardening/2026-07-28-review`](evidence/pgo-hardening/2026-07-28-review) |
| Classification | Synthetic correctness fixtures and one deliberately incompatible-profile control |

The review corrected missing-profile classification during PGO confirmation and
moved the JSON-looking, zero-artifact and multiple-artifact compatibility cases
into independently named PGO-phase tests. The four PRD quality gates then
passed. The full suite executed 66 tests with 66 passes, 0 failures and 1
ignored collector test. The collector was run explicitly and passed.

The retained rerun contains four complete real-toolchain PGO paths and one
incompatible-profile control rejected as `pgo_missing_profile_data` with zero
PGO screening samples. Its 34 artifact, 10 raw-profile and 5 merged-profile
checksum references were independently recomputed and matched before temporary
build directories were released. These are correctness controls only and
support no optimization-gain, representative-benchmark or release-readiness
claim.

## 2026-07-28 corpus-v1 reference baseline

This append-only section records the first `temper-corpus-v1` reference-host
collection. It reports each real application independently. No arithmetic mean,
geometric mean, ranking, composite score or universal speedup was computed.

| Field | Value |
|---|---|
| Baseline commit | `024b3899fd64a25084eaeff33301acf76b5f4d94` |
| Tested worktree | Dirty v0.0.2 implementation, production-input SHA-256 `980625bea61ce6cdfbfa6499946678b0ac1d3e7255153c84106932ed51479cbd` |
| Corpus | `temper-corpus-v1`, changelog `1.0.0` |
| Host | Linux 7.1.5, AMD Ryzen 7 7800X3D, 16 logical cores |
| Rust | rustc 1.97.1, LLVM 22.1.6 |
| Cargo | cargo 1.97.1 |
| Raw evidence | [`../benchmarks/corpus/v1/results/reference/2026-07-28`](../benchmarks/corpus/v1/results/reference/2026-07-28) |
| Evidence count | 12 parseable schema-v2 `run.json` records, 12 metadata records, 12 checksum audits |

The real workload cases are bounded local proxies for expected application use,
not production traffic: BLAKE3 hashes its upstream 31 KiB test-vector corpus at
32-byte and 64-byte output lengths with weights 60/40; `xsv` computes full
statistics and a two-column projection over deterministic CSV data with weights
55/45; `hexyl` streams a complete upstream ELF fixture and a bounded slice with
weights 60/40. One workload invocation executes every scenario exactly its
weight and verifies each process exit plus its semantic output SHA-256. Curated
baseline workload medians were 280 ms, 420 ms and 230 ms respectively.

| Case | Run | Baseline median | Selected candidate | Confirmation ratio | 95% CI | Decision |
|---|---:|---:|---|---:|---|---|
| BLAKE3 `b3sum` | 1 | 288.470 ms | ThinLTO | 1.000674 | [0.995120, 1.015795] | `no_improvement` |
| BLAKE3 `b3sum` | 2 | 288.445 ms | FatLTO, CGU 1 | 0.988767 | [0.978179, 0.999949] | `no_improvement` |
| BLAKE3 `b3sum` | 3 | 283.426 ms | PGO | 0.999922 | [0.982727, 1.003828] | `no_improvement` |
| `xsv` | 1 | 431.089 ms | PGO | 0.952620 | [0.941101, 0.956382] | `confirmed` |
| `xsv` | 2 | 432.867 ms | PGO | 0.963293 | [0.943980, 0.964902] | `confirmed` |
| `xsv` | 3 | 430.042 ms | PGO | 0.952486 | [0.941844, 0.954220] | `confirmed` |
| `hexyl` | 1 | 237.908 ms | PGO | 0.979217 | [0.978600, 0.999785] | `no_improvement` |
| `hexyl` | 2 | 237.833 ms | FatLTO, CGU 1 | 0.999363 | [0.989188, 0.999975] | `no_improvement` |
| `hexyl` | 3 | 237.843 ms | PGO | 0.981490 | [0.979237, 1.000026] | `no_improvement` |

All nine real runs and all three synthetic-control runs passed their correctness
oracles. The three `xsv` observations confirmed PGO for this exact pinned
source, input, workload, host and toolchain only. They do not establish a
cross-application, cross-machine or universal gain. The synthetic control is
non-representative harness evidence and has no performance interpretation.

Before each temporary worktree was released, the collector rechecked every
available referenced checksum: 84 artifact references, 2,400 raw-profile
references and 12 merged-profile references matched. Source, Cargo manifest,
lockfile and input checksums were unchanged. The collector forced Cargo offline,
retained no failure record and wrote an explicit null aggregate score.

The evidence identifies an uncommitted implementation through both its baseline
commit and production-input hash. A clean committed-state rerun remains required
before any release-readiness claim.

## 2026-07-28 strict v0.0.1 contract review

This append-only section audits the v0.0.1 requirements inherited by the
current v0.0.2/schema-2 implementation. It does not rewrite the historical
v0.0.1 commit or claim that the reviewed uncommitted worktree was releasable.

| Field | Value |
|---|---|
| Baseline commit | `350357ba8ebdb9f657af5819ea00bc1feb1d4b54` |
| Tested worktree | Dirty remediation worktree |
| Current package/report identity | Temper `0.0.2`, schema 2 |
| Historical contract artifact | [`schema-v1.json`](schema-v1.json), CLI `0.0.1` fixture |
| Production inputs | SHA-256 `00cb6b89e56ee47e9ceb54dda429c27745821b3f2fbf1499ee449edd0c13bfba` |
| Host | Linux 7.1.5, `x86_64-unknown-linux-gnu` |
| Rust | rustc 1.97.1, LLVM 22.1.6 |
| Cargo | cargo 1.97.1 |
| Classification | Correctness, durability and reference-host NFR evidence |

The four exact repository gates passed:

```sh
cargo fmt --check
cargo check --all-targets --locked
cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings
cargo test --all-targets --locked
```

The broad suite executed 90 passing tests, 0 failures and 4 intentionally
ignored tests. Two ignored tests are one-shot evidence collectors. The other
two are reference-host NFR measurements and were run explicitly:

```sh
cargo test --locked --test nfr_v001 -- --ignored --nocapture
```

Both passed. The 100-package planning path consumed less than one observable
Linux scheduler tick of parent CPU (`0.000 s` at the available resolution).
The complete four-artifact run recorded 68 samples and reached 7,244 KiB
maximum direct-parent RSS, below the 150 MiB limit.

The remediation added or strengthened the following discriminating evidence:

- deterministic measurement cohorts now assert 0 accepted A/A datasets across
  100 controls, 0 accepted regressions across 20 controls and at least 18
  accepted improvements across 20 controls;
- one unit test performs 100 timeout lifecycles and checks every forked
  descendant in `/proc`; separate integration tests exercise real SIGINT
  cleanup;
- successful and failed end-to-end runs compare target `Cargo.toml`,
  `Cargo.lock` and source bytes before and after execution;
- the checked-in schema-v1 contract validates structurally complete confirmed,
  no-improvement, failed and interrupted terminal variants, including exact
  sample counts and nested build, confirmation, promotion and failure fields;
- SIGINT during a baseline Cargo build persists an interrupted terminal
  manifest;
- SIGINT during artifact copy is made deterministic by stopping the Temper
  process while the temporary artifact exists, queuing SIGINT, then resuming
  it;
- promotion copy, checksum, rename and both post-rename directory-sync failures
  leave no stable artifact;
- `latest.json` publication preserves a durable hard-link recovery point and
  restores the previous pointer when its post-rename directory sync fails;
- promotion reserves enough allocated filesystem space for the current
  manifest plus a bounded failure record, and releases it only after
  `latest.json` completes; before every active `run.json` persistence, a full
  emergency terminal manifest containing all previously durable experimental
  evidence is written and synchronized. A reserve-creation failure E2E
  activates that fallback, and an emergency-refresh failure activates the
  preceding durable fallback. `/dev/full` separately proves write-side ENOSPC
  classification;

The review also corrected the package identity to `0.0.2`, while retaining the
historical schema-1 fixture and preserving existing schema-1 files byte for
byte. The v0.0.1 platform and workload boundaries remain unchanged.

Evidence limits remain:

- the reviewed state was an uncommitted v0.0.2 successor worktree; this section
  does not identify a post-commit clean-state rerun and does not retroactively
  change commit `024b3899fd64a25084eaeff33301acf76b5f4d94`;
- the temporary 2026-07-28 schema-v1 dogfood manifests are unavailable, so
  their historical aggregate cannot be revalidated against the new checked-in
  schema;
- ENOSPC is covered by allocated-reserve behavior, reserve-creation failure
  E2E, emergency fallback activation, rollback, injected sync failures and
  `/dev/full`, not by a full end-to-end run on a deliberately saturated
  isolated filesystem;
- the CPU and RSS results apply only to the stated reference host and
  toolchain;
- the PRD month-1 and month-6 adoption, completion-rate and external-user
  targets are time-based product outcomes, not local implementation gates;
- no new production-representative benchmark or clean committed-state
  verification was produced.

On the available evidence, no known code-level v0.0.1 contract blocker remains
in the current successor implementation. This is a strict implementation
conformance conclusion, not a claim that historical v0.0.1 evidence is
reproducible or that either version is release-ready.

## 2026-07-28 v0.0.3 EP-001 compiler-interposition experiments

This append-only section records the executed EP-001 evidence for
[`../tasks/prd-temper-v0.0.3.md`](../tasks/prd-temper-v0.0.3.md). It is
correctness and cost evidence for a compiler-interposition design. It contains
no optimization-gain claim and no corpus result.

| Field | Value |
|---|---|
| Tested commit | `f1619f9201c2abe75043952fb278c0d0c2295034` |
| Tested worktree | `src`, `tests`, `Cargo.toml` and `Cargo.lock` unmodified; the only tracked change is this ledger entry, alongside untracked v0.0.3 planning and evidence files |
| Toolchain | rustc 1.97.1 (LLVM 22.1.6), cargo 1.97.1; cargo 1.93.1 for the feature-version boundary |
| Host | Linux 7.1.5-200.fc44.x86_64, AMD Ryzen 7 7800X3D, 16 logical cores |
| Raw evidence | [`evidence/interposition/2026-07-28`](evidence/interposition/2026-07-28) |
| Workload class | **Synthetic**. Two purpose-built fixtures exist only to expose compiler inputs |
| Executed assertions | 125, all passing (US-001 39, US-002 57, US-003 29) |

Workload definition: `fixtures/include-loss` and `fixtures/include-strict` are
single-binary crates whose `main` runs a fixed 40,000,000-iteration integer
mixing loop, and `fixtures/units` is a three-member workspace adding a build
script, a proc macro and a target dependency around the same loop. Each Temper
run used the workload `exec "$TEMPER_BINARY"`. These fixtures are not
production-representative and carry no runtime interpretation.

The experiments reproduced two v0.0.2 correctness defects on executed builds
rather than by source analysis:

- a target rustflag reaching Cargo only through a stable `include` file is
  preserved by the baseline and both static candidates and lost by both PGO
  phases, while schema 2 still reports `phase_parity.matched = true`. A
  `compile_error!` variant converts the same loss into a `cargo_build_failed`
  PGO instrumentation failure;
- `build.rustc-wrapper`, `build.rustc-workspace-wrapper` and `build.rustc`
  declared only through an `include` file are not detected. PGO trained
  normally with a wrapper active for all 53 compiler invocations of those runs,
  and with a configured compiler Temper never identified performing 7 target
  compilations. The same three declarations are correctly rejected as
  `unproven_compiler_override` when they appear in a directly discovered file.

The measured cost and identity results were: identical direct and pass-through
executable SHA-256 on the deterministic fixture, and 2.24% median pass-through
overhead across 12 paired cold builds (152.1 ms direct, 154.6 ms pass-through,
per-pair ratios 0.996 to 1.050). Raw monotonic per-pair timings are retained in
`results/us-003/overhead.json`.

Evidence limits: both fixtures are synthetic and tiny, so neither the overhead
figure nor the observed invocation counts transfer to a large workspace; the
caching wrapper is a documented emulation of a build-directory-independent
cache and not `sccache`; the Cargo feature-version boundary was executed on
`cargo 1.93.1` only; and nothing in this section measures the runtime of any
optimized binary.

## 2026-07-29 v0.0.3 EP-004 compatibility matrices and release evidence

This append-only section records the executed EP-004 evidence for
[`../tasks/prd-temper-v0.0.3.md`](../tasks/prd-temper-v0.0.3.md). It is
correctness and cost evidence. It contains no optimization-gain claim, no
aggregate corpus score, and no production-representativeness claim.

| Field | Value |
|---|---|
| Tested commit | `f3d3fc53a6d9dc41391790c24c178267aacd606b` |
| Tested worktree | `src`, `tests`, `Cargo.toml`, `Cargo.lock`, `scripts` and `docs` committed and unmodified; the collectors reported `worktree_dirty` only because they were writing the untracked evidence directory named below |
| Toolchain | rustc 1.97.1 (8bab26f4f 2026-07-14), LLVM 22.1.6; cargo 1.97.1 (c980f4866 2026-06-30) |
| Host | Linux 7.1.5-200.fc44.x86_64, `x86_64-unknown-linux-gnu`, AMD Ryzen 7 7800X3D, 16 logical cores |
| Raw evidence | [`evidence/v0.0.3/2026-07-29`](evidence/v0.0.3/2026-07-29) |
| Regenerate with | `scripts/collect-v0.0.3-evidence.sh <YYYY-MM-DD>` |

### Quality gates

`cargo fmt --check`, `cargo check --all-targets --locked`,
`cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings`
and `cargo test --all-targets --locked` all pass on the tested commit. The test
suite reports 158 passing and 7 ignored cases; the ignored cases are the
evidence collectors and the paired overhead benchmark, which the script above
runs explicitly.

The suite was executed repeatedly while EP-004 roughly doubled its concurrent
build work, and three separate load-induced failures were observed and fixed
rather than accepted:

- `ep002_matrix` base-strategy selection and `ep004_confirmation` promotion used
  workload contrasts whose faster arm had no duration floor, so per-invocation
  process noise could flip the decision. Both now keep a nonzero floor with
  wider absolute gaps and the same ratios;
- `ep001_v002_contract` failed with
  `Baseline screening was unstable (relative MAD 0.143449)`. The shared fixture
  helper used `/bin/true`, so screening measured a zero-duration process and its
  relative median absolute deviation was pure process noise. The default
  workload is now `/bin/sleep 0.02`, together with every explicit call site that
  asserts a measured run. Call sites that fail before measurement keep
  `/bin/true`.

Measurement-v1 is unchanged, and every artifact in a run still executes the same
workload, so no decision boundary moved. The gate remains genuinely sensitive to
host load by design: these runs shared the host with an unrelated Rust build,
and that is what exposed each margin.

### Compatibility matrices

Both matrices in
[`pgo-compatibility-matrix-v1.md`](pgo-compatibility-matrix-v1.md) pass on the
toolchain above: 13 Cargo rustflags and `include` provenance cases and 6
wrapper, isolation and evidence cases, plus the retained v0.0.2 PGO cases.

### Include-based PGO run

Workload class: **synthetic**. The fixture is a single-binary crate whose only
source of a required target rustflag is a Cargo `include` file and whose source
fails to compile without it. Its workload sleeps longer for every non-PGO
artifact so the bounded search reaches confirmation; that path dependence
manufactures the decision boundary and is coverage evidence only. **The
confirmation ratio of that run is not an optimization gain and must never be
cited as one.**

The run (`1785314226-174831361-2721393`) is schema 3, `confirmed`, and records
four interposed stages, each with exactly one observed target compilation, zero
host compilations, two passed-through probes, and injected phase controls only
on `pgo_generate`, `pgo_use` and `pgo_confirmation`. Observed training parity
and confirmation parity both match, and the config graph records `included.toml`
before `config.toml` with a missing optional edge. Raw `run.json`, both report
streams and the hashed fixture sources are retained.

### Paired pass-through overhead

12 paired cold builds of `tests/fixtures/pgo-workspace` with a release
`cargo-temper` as the interposed compiler, alternating arm order after one
discarded warm-up pair: **median ratio 1.0348**, range 0.9992 to 1.0992, against
the 5% budget. Direct and pass-through builds produced the selected executable
with identical SHA-256 (`0e171fd9…`). Raw per-pair ratios are in
`overhead.json`. The fixture is small, so per-`exec` cost is amortized over few
compiler invocations and the figure does not transfer to a large workspace.

### Real corpus-v1 case, and the predicate limitation it exposed

Corpus-v1 case `hexyl` was executed unchanged against the release
implementation, outside the immutable corpus reference tree. The corpus
manifest, sources, inputs, workloads and existing 2026-07-28 reference records
are byte-for-byte untouched. Its workload remains a **bounded local proxy**; no
representativeness claim and no aggregate cross-application score is introduced.

The run (`1785314254-53154417-2729874`) is schema 3 and completed with
`no_improvement`, selecting `fat-lto-cgu1` and promoting nothing
(median ratio 1.000355, 95% CI 1.000069 to 1.022402).

PGO was rejected for that run with one bounded `pgo_compiler_input_ambiguous`
decision, scoped `pgo_only`. The cause was isolated to a single invocation: the
`rustix` build script probes the compiler for the requested target with

```text
rustc --crate-type=rlib --emit=metadata --target x86_64-unknown-linux-gnu \
      --out-dir <target>/release/build/rustix-<hash>/out -
```

reading its source from stdin and naming no crate. It carries the exact
supported target and a compilation `--emit`, so the v0.0.3 predicate cannot
separate it from a real unit and fails closed rather than guessing, exactly as
the PRD requires. The consequence is a real product limitation rather than an
implementation defect: **any dependency tree containing such a build-script
probe gets static optimization only**, and `rustix` is common in the CLI
ecosystem. The shape is pinned by
`rejects_ambiguous_target_and_identity_shapes` in `src/interposition.rs` so a
future widening of the predicate must be deliberate and separately evidenced.

The same trace showed that the `anyhow`, `thiserror` and `proc-macro2` build
scripts run target probes that *do* carry `--crate-name`, so they are classified
as target compilations and would receive phase controls. Their output is
metadata written into a build-script output directory and discarded, so no
artifact is affected, but the observation belongs with any future work on the
predicate.

### Evidence limits

- the include fixture is synthetic and tiny; nothing in this section measures
  the runtime of any optimized binary;
- the overhead figure applies to the stated host, toolchain and small fixture
  only;
- one corpus case was executed once; that is a schema-3 completeness check, not
  a benchmark, and no aggregate score exists;
- the predicate limitation above was found by executing one real case. Other
  real dependency trees may contain further unclassifiable shapes that no
  fixture in this repository covers.
