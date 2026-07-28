# Temper v0.0.3 EP-001 compiler-interposition evidence

Date: 2026-07-28. Epic: EP-001 of
[`tasks/prd-temper-v0.0.3.md`](../../../tasks/prd-temper-v0.0.3.md).

This directory retains correctness and cost evidence only. It makes no
optimization-gain or production-performance claim. Every workload here is
**synthetic**: two purpose-built fixtures whose only job is to expose compiler
inputs. No corpus-v1 case, no representativeness claim, and no runtime gain is
involved.

| Field | Value |
|---|---|
| Tested Temper commit | `f1619f9201c2abe75043952fb278c0d0c2295034` (v0.0.2; `src`, `tests`, `Cargo.toml` and `Cargo.lock` unmodified, the only tracked change being this epic's verification-ledger entry) |
| Cargo | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| rustc | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6 |
| Second Cargo | `cargo 1.93.1 (083ac5135 2025-12-15)` for the feature-version boundary |
| Host | Linux 7.1.5-200.fc44.x86_64, `x86_64-unknown-linux-gnu` |
| CPU | AMD Ryzen 7 7800X3D, 16 logical cores |
| Checks | 125 executed assertions, 0 failed |

Regenerate everything with
`TEMPER_EP001_WORK=<scratch> harness/run-all.sh`. Fixtures, target directories
and raw captures live in the scratch directory; only analysed evidence is
written back here.

## What the harness is

`harness/shim.rs` is a candidate compiler shim compiled directly with `rustc`
(no Cargo project, no dependency, no `Cargo.lock` change). It is installed as
the `RUSTC` value of an experiment build, records one JSON capture per
invocation, optionally appends PGO controls, and then replaces itself with the
real compiler through Unix `exec`. `harness/wrapper.rs` plays the part of
`RUSTC_WRAPPER` / `RUSTC_WORKSPACE_WRAPPER` and can emulate a
build-directory-independent compiler cache. `harness/fake-rustc.rs` is a
deterministic stand-in compiler used for process-level probes.
`harness/analyze.py` implements the candidate schema-3 parity normalization so
the allowlist can be attacked before production code exists.

Temper v0.0.2 neither inspects nor rejects an ambient `RUSTC`, so the shim
observes the unmodified implementation without changing it. No production code
or schema behaviour was modified for this epic.

## US-001 — the include loss is reproduced, not inferred

Fixture `fixtures/include-loss` declares the sentinel target rustflag
`--cfg temper_included_sentinel` only through Cargo configuration `include`
files. Results: `results/us-001/`.

| Config case | Direct `cargo build` | Temper baseline / thin-lto / fat-lto-cgu1 | Temper PGO generate + use | Schema-2 `phase_parity.matched` |
|---|---|---|---|---|
| `direct` (control, no include) | sentinel present | present | **present** | `true` |
| `required-include` | sentinel present | present | **absent** | `true` |
| `nested-include` (two levels) | sentinel present | present | **absent** | `true` |
| `optional-missing` + required include | sentinel present | present | **absent** | `true` |

The defect is exactly as the PRD predicted, and it is silent: schema 2 reports
`matched: true` with no unexpected difference while both PGO phases compiled
without a compiler input that the baseline and both static candidates received.
The `direct` control isolates the cause: v0.0.2 preserves a target rustflag it
can see in a directly discovered config file, and loses it as soon as the same
value arrives through `include`.

`fixtures/include-strict` turns the same loss into an executed build failure: a
`compile_error!` guarded by the sentinel. The baseline builds, and the PGO
instrumentation build fails with `cargo_build_failed` and the message *the
included target rustflag did not reach this compilation*
(`results/us-001/strict-run.json`).

Cargo source-graph failures were observed directly: a missing required include
and an include cycle both fail with `could not load Cargo configuration`
(`results/us-001/missing-required.log`, `results/us-001/cycle-include.log`).

### Cargo feature-version boundary

On `cargo 1.93.1`, the same fixture **builds successfully and silently ignores
the whole `include` graph**: exit status 0, no error, no warning naming the
unsupported key, and the included target rustflags never reach rustc
(`results/us-001/version-boundary.log`). Temper therefore cannot rely on Cargo
to signal the boundary; FR-024 must be enforced by Temper's own Cargo version
check.

### Cargo `include` syntax actually accepted

Read from the local Cargo checkout (`/home/arthur/dev/cargo/src/context/mod.rs`)
and confirmed by execution:

- `include` must be a **list**: a list of strings, or a list of tables. A bare
  string is rejected.
- An optional entry is `{ path = "extra.toml", optional = true }`. There is no
  `?` suffix form.
- Every include path must end in `.toml`, must not contain glob patterns and
  must not contain `{`/`}`.
- Paths resolve relative to the **including file**; includes load left to
  right, then the including file overrides them.
- Cargo discovers `.cargo/config.toml` from the **current directory**, not from
  `--manifest-path`. The US-008 scanner must use the same anchor Temper gives
  Cargo (the workspace root).

## US-002 — classification, transparency and the parity allowlist

Fixture `fixtures/units` is a workspace with a build script, a proc macro, a
target dependency and the selected binary. Results: `results/us-002/`.

### Observed invocation classes (one build, explicit `--target`)

| Class | Count | Crates | `--target <supported>` | `--emit` | `--print` |
|---|---|---|---|---|---|
| `target_compile` | 2 | `temper_units_app`, `temper_units_lib` | yes | yes | no |
| `host_compile` | 2 | `build_script_build`, `temper_units_macro` | **no** | yes | no |
| `probe` | 2 | `___` | one of the two | no | yes |
| `other` | 4 | — (`rustc -vV`) | no | no | no |

The discriminator is `--emit`, not `--target`: one of Cargo's two
`--print=file-names` probes carries the exact supported target triple and must
still be passed through. The predicate proven here is therefore *exact
supported `--target` **and** an `--emit` form **and** no `--print`*. With an
explicit `--target`, host units carry no `--target` argument at all.

`harness/probe_process.py` drives the shim directly for the shapes Cargo does
not produce: the `--target=<triple>` equals-form, a foreign triple, a
`--print` probe that also carries `--emit`, and a host compile whose
`--out-dir` merely *contains* the host triple as a path component. None of the
non-target shapes receives an injected control.

### Process transparency

32 process-level probes pass: arbitrary non-UTF-8 argument bytes survive
`exec` byte for byte (verified by length-framed comparison), stdin, stdout and
stderr are preserved, and exit status 0, exit status 42 and termination by
`SIGABRT` are identical with and without the shim. Ten protocol-failure
variants (missing, empty, relative or absent real rustc; missing or absent
capture directory; missing stage; missing target; malformed and relative
injection) each exit `97` with one bounded diagnostic, and a `rustc` canary
placed first on `PATH` is never executed.

An independent Python implementation reproduces every length-framed argv digest
the shim recorded, so the digest contract is not self-certified.

### Pass-through artifact identity

A direct build and a pass-through build in distinct fresh target directories
produced the selected executable with identical SHA-256
(`979a06351430de5a…`) and an identical set of Cargo artifact file names
(`results/us-002/artifact-identity.json`).

### Parity allowlist, and one required correction to the PRD assumption

Reference, generate and use builds ran in three fresh target directories.
Exactly three argv positions depend on the stage target root, identically in
all three phases:

| Position | Shape |
|---|---|
| `--out-dir` | `<STAGE_ROOT>/…` |
| `-L` | `dependency=<STAGE_ROOT>/…` |
| `--extern` | `<crate>=<STAGE_ROOT>/…` |

After normalizing those, the observed compiler inputs match exactly, and the
only remaining differences are the documented phase controls:

- `generate`: one `-Cprofile-generate=<absolute>`
- `use`: one `-Cprofile-use=<absolute>` plus `-Cllvm-args=-pgo-warn-missing-function`

**Correction.** The PRD assumed the allowlist would also have to permit *Cargo
artifact identity fields proven to derive from the stage roots* to differ. They
do not differ. `-Cmetadata` and `-Cextra-filename` were byte-identical across
all three phases, because Cargo computes them before the shim appends anything:

| Crate | `-Cmetadata` | `-Cextra-filename` |
|---|---|---|
| `temper_units_app` | `982a605e74331892` in all three phases | `-0aad598a7310d852` in all three phases |
| `temper_units_lib` | `a49209a198aff691` in all three phases | `-4835122452554d4f` in all three phases |

The allowlist US-009 must implement is therefore **narrower** than projected:

1. substrings equal to the stage target root, normalized to `<STAGE_ROOT>`;
2. `-Cprofile-generate=<abs>` present only in the generate phase;
3. `-Cprofile-use=<abs>` and `-Cllvm-args=-pgo-warn-missing-function` present
   only in the use phase.

Everything else, including artifact identity fields, must compare equal.
Comparison is keyed by logical crate identity (crate name), with crate kinds,
metadata, extra-filename, the resolved tool path and the ordered argument list
compared as fields. A duplicate crate identity within one phase is itself a
failure (`ambiguous_crate_identity`).

### Adversarial mutations

13 mutation classes were applied to the observed use-phase set. 12 are rejected
with a bounded difference class; `record_order` is correctly *not* a difference
because comparison is keyed by crate identity rather than capture order.

| Mutation | Reported difference classes |
|---|---|
| `argument_added` | `argument_added`, `argument_count` |
| `argument_removed` | `argument_count`, `argument_removed` |
| `argument_changed` | `argument_added`, `argument_removed` |
| `argument_order` | `argument_order` |
| `escaped_stage_root_path` | `argument_added`, `argument_removed` |
| `crate_added` | `crate_added`, `crate_count` |
| `crate_removed` | `crate_count`, `crate_missing` |
| `crate_name_changed` | `crate_added`, `crate_missing` |
| `crate_kind_changed` | `crate_kind_changed` |
| `artifact_metadata_changed` | `artifact_metadata_changed` |
| `artifact_extra_filename_changed` | `artifact_extra_filename_changed` |
| `tool_changed` | `tool_changed` |
| `record_order` | none (expected: identity-keyed comparison) |

## US-003 — the wrapper boundary is a cache-correctness boundary

Results: `results/us-003/`.

### Wrapper chain

Four wrapper fixtures were built through the shim with a PGO control injected:

| Fixture | Observed chain |
|---|---|
| no wrapper | no wrapper process at all |
| general wrapper only | `general-wrapper` → shim |
| workspace wrapper only | `workspace-wrapper` → shim |
| nested | `general-wrapper` → `workspace-wrapper` → shim |

Cargo executes `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC` exactly as
documented, and in the nested fixture both wrappers see the same unit set,
including Cargo's probes. Critically, **no wrapper in any fixture ever observes
a PGO control**: the shim appends them after every wrapper has already seen and
could already have keyed the command.

### Cache aliasing, demonstrated at the artifact level

`harness/wrapper.rs` in `cache` mode is a documented emulation of a
build-directory-independent compiler cache: it normalizes the stage target root
out of the key, exactly as a cache must do to hit across build directories. It
is not `sccache` and makes no claim about `sccache` internals.

- Reference, generate and use produce **one shared cache key per target crate**.
- The generate and use phases are served from the reference cache entry
  (8 cache hits).
- All three stage executables have the **same SHA-256**.
- The executable the instrumented phase produced emits **zero** `.profraw`
  files at runtime, while an uncached instrumented build of the same fixture
  emits profile data normally.
- Under the cache, **zero** target or host compilations ever reach the shim in
  the generate and use phases.

That is the whole argument for the v0.0.3 wrapper rejection, executed rather
than assumed: with an outer cache present, the PGO phases can silently return a
non-instrumented reference artifact, and Temper's own evidence capture never
even runs. No wrapper-specific bypass, environment tweak or cache-disable flag
was added, and none should be until a wrapper class can prove its cache key
includes the effective phase controls.

### Cargo fingerprints do not see shim-appended flags

Building into a fresh directory, then rebuilding **the same directory** with
`-Cprofile-generate` injected, produced 10 rustc invocations followed by
**zero**: Cargo considered every unit fresh and the artifact SHA-256 was
unchanged. Building a third phase through a **symlinked alias** of that
directory is equally invisible: zero invocations again. A fresh per-stage
directory recompiles every target unit.

Cargo also accepts a **precreated** stage directory without any warning and
compiles into it normally, so a pre-existing directory is not something Cargo
will ever reject on Temper's behalf. The unique, initially absent, canonically
distinct target directory per stage is therefore a hard correctness invariant
Temper must enforce itself, not a hygiene preference.

### A second v0.0.2 detection defect

| Compiler override | Declared directly | Declared only through `include` |
|---|---|---|
| `build.rustc-wrapper` | rejected, `unproven_compiler_override` | **not detected**, PGO trained with the wrapper active for all 53 compiler invocations |
| `build.rustc-workspace-wrapper` | rejected, `unproven_compiler_override` | **not detected**, PGO trained with the wrapper active for all 53 compiler invocations |
| `build.rustc` | rejected, `unproven_compiler_override` | **not detected**, PGO trained while Cargo used the configured compiler for all 53 invocations, 7 of them target compilations, and Temper identified a different one |

An ambient `RUSTC_WRAPPER` and an ambient `RUSTC_WORKSPACE_WRAPPER` are both
still rejected as `ambient_compiler_override`, and in both cases the static
candidates stay eligible. The include-graph gap is what US-008's recursive
scanner has to close for FR-007 and FR-008.

### Overhead

12 paired cold builds, alternating which arm runs first, on the `units`
fixture:

| Metric | Value |
|---|---|
| Median pass-through overhead | **2.24%** (budget: 5%) |
| Median direct build | 152.1 ms |
| Median pass-through build | 154.6 ms |
| Ratio range | 0.996 to 1.050 |

Raw per-pair monotonic timings are in `results/us-003/overhead.json`. The
fixture is small, so the per-`exec` cost is amortized over only ten compiler
invocations; this is a conservative rather than a flattering measurement, but
it is not a large-workspace result.

## Assumption verdicts

| PRD assumption | Verdict |
|---|---|
| Target compilation classifiable from exact `--target` plus `--emit` | Validated. `--emit` is the discriminator; `--print` must be excluded explicitly because a probe carries the exact target triple |
| Unix `exec` preserves argument bytes and process semantics | Validated across normal, failing, signalled and non-UTF-8 cases |
| Setting `RUSTC` preserves wrapper ordering, but an outer cache may key before the phase flags | Validated, and stronger than stated: the cache both aliases the artifact and hides the phase from evidence capture entirely |
| A strict normalized digest can compare non-PGO compiler inputs | Validated **with a correction**: Cargo artifact identity fields do not differ and must not be allowlisted |
| Pass-through artifact identity and a 5% overhead budget | Validated: identical SHA-256, 2.19% median over 12 pairs |
| The real rustc can be resolved before the `RUSTC` override | Validated for the ambient toolchain. A configured `build.rustc` remains out of scope and must be rejected; v0.0.2 fails to detect it through `include` |

No assumption failed, so
[`tasks/prd-temper-v0.0.3.md`](../../../tasks/prd-temper-v0.0.3.md) stays
`READY`. Two findings change downstream work and must be honoured by EP-002 and
EP-003:

1. US-009's normalization allowlist is narrower than the PRD projected. Artifact
   identity fields are compared, not normalized.
2. FR-024 cannot rely on a Cargo error: Cargo below 1.94 ignores `include`
   silently, so Temper must gate on the Cargo version itself.

## Evidence limits

- Both fixtures are synthetic and tiny. The overhead result and the observed
  invocation counts do not transfer to a large workspace.
- The caching wrapper is a documented emulation, not `sccache`. It shows that a
  build-directory-independent key aliases the phases; it does not measure any
  particular product.
- The Cargo feature-version boundary was executed on `cargo 1.93.1` only. No
  claim is made about other pre-1.94 releases.
- Retained captures contain fixture-local argv from synthetic projects under a
  scratch directory. They are experiment evidence, not a `run.json` field, and
  the bounded-publication rule for schema 3 is unaffected.
- Nothing here measures runtime performance of any optimized binary.
