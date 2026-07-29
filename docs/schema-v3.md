# Temper report schema 3

Schema 3 applies only to runs created by the v0.0.3 implementation. Temper does
not discover, rewrite, migrate, or reinterpret existing schema-1 or schema-2
`run.json` and `latest.json` files. Every historical field keeps its documented
meaning, and [`docs/schema-v2.md`](schema-v2.md) stays the contract for schema-2
records.

The CLI identifies new human reports as `schema 3`. `--json`, `run.json`, and
new `latest.json` records declare `"schema_version": 3`.

Schema 3 exists because schema 2 compared the PGO inputs Temper *planned*.
Schema 3 also records the inputs the compiler *received*, observed at the final
rustc boundary through a private same-binary shim.

## What replaced `environment_overrides`

Temper no longer synthesizes `CARGO_ENCODED_RUSTFLAGS` for PGO, so a schema-3
build invocation has no `environment_overrides` field. Cargo resolves every
compiler input and the shim appends the phase controls after it.

## Compiler interposition

Every interposed PGO build carries an `interposition` object:

| Field | Meaning |
|---|---|
| `protocol` | Private shim protocol version. It is internal and is not a compatibility promise |
| `normalization` | Version of the record shape and digest construction |
| `stage` | `pgo_reference`, `pgo_generate`, `pgo_use` or `pgo_confirmation` |
| `shim_executable` | Canonical path of the installed `cargo-temper` acting as `RUSTC` |
| `shim_sha256` | SHA-256 of that executable |
| `real_rustc` | Canonical path of the compiler the shim replaces itself with |
| `real_rustc_version` | Full `rustc -Vv` identity proven before the `RUSTC` override |
| `capture_directory` | Stage capture root inside the current run |
| `profile_path` | Absolute in-run PGO profile path, or `null` for the reference stage |
| `injected_flags` | Bounded flag identifiers, never their values |

Each interposed build also carries `compiler_evidence`, the validated aggregate
of that stage's capture set: `record_count`, `capture_bytes`,
`target_compilations`, `host_compilations`, `probes`, `other_invocations`,
`injected_invocations`, `injected_flags` and `capture_digest`.

Individual capture records are **not** published. They stay inside the run
capture directory. `run.json` contains counts and digests only, so no
unrestricted compiler argv or environment value is ever serialized.

### Capture records and bounds

One shim invocation writes one create-new record before `exec`. A record holds
protocol version, stage, invocation class, structural crate fields
(`crate_name`, `crate_types`, `metadata`, `extra_filename`), the classification
booleans, the argument count, the ordered pre-injection and post-injection
digests, the normalized digest, per-argument normalized digests (target
compilations only), the injected flag identifiers and an optional rejection.

| Bound | Value |
|---|---|
| Records per stage | 10,000 |
| Capture bytes per stage | 32 MiB |
| Bytes per record | 64 KiB |
| Arguments per comparable target compilation | 2,048 |
| Crate types per record | 16 |
| Bounded string field | 512 bytes |

Digests are SHA-256 over length-framed raw Unix argument bytes. Framing makes
concatenation unambiguous, so `["ab", "c"]` and `["abc"]` never collide. A
per-argument digest is the first 16 hex characters of that construction.

## Cargo configuration source graph

`pgo_training.prerequisites.config_graph` records provenance, never merged
values. Cargo keeps ownership of effective rustflags.

| Field | Meaning |
|---|---|
| `cargo_minor` | Minor release parsed from `cargo -Vv` |
| `include_supported` | Whether that release is at least 1.94 |
| `declares_include` | Whether any discovered source declares `include` |
| `sources` | Every source in Cargo's documented load order |
| `environment_inputs` | Compiler-input variables as bounded digests |

Each source records its canonical `path`, its `sha256`, how it was discovered
(`cargo_home`, `ancestor` or `include`) and its `includes` edges. An edge
records the `declared` string, the `resolved` path, whether it is `optional`
and whether it is `present`.

Discovery follows Cargo's documented rules: `include` must be a list of
`.toml` paths or `{ path = "...", optional = true }` tables, paths resolve
against the including file, included files load left to right and precede the
file that includes them, and a missing optional entry is recorded rather than
fatal. A path containing `*`, `?`, `[`, `]`, `{` or `}` is unsupported. The
graph is bounded at 64 sources, depth 16, 256 declared includes per file and
1 MiB per source.

Cargo below 1.94 ignores `include` silently rather than failing, so Temper
gates on the Cargo version itself: a configuration that declares `include`
below 1.94 is rejected with `cargo_config_include_unsupported_version` and no
nightly opt-in. A configuration without `include` keeps the prior supported
Cargo boundary.

Every source is read twice and rejected with `cargo_config_source_mutated` when
the two reads disagree, so a file changed during discovery is never hashed as
stable evidence.

### Environment inputs

`environment_inputs` records `CARGO_ENCODED_RUSTFLAGS`, `RUSTFLAGS`,
`CARGO_BUILD_RUSTFLAGS`,
`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS`, `RUSTC`, `RUSTC_WRAPPER`,
`RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC`, `CARGO_BUILD_RUSTC_WRAPPER`,
`CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_TARGET` and `CARGO_HOME`.

Each entry has a `presence` of `absent`, `empty` or `set`, and a `sha256` only
when it is `set`. Cargo treats an empty wrapper as unset, so absence and an
empty value stay distinguishable without publishing any value.

## Planned parity and observed parity

Schema 3 carries two distinct objects. They answer different questions and
neither replaces the other.

`pgo_training.phase_parity` keeps its schema-2 role: it compares the inputs
Temper *planned* for the instrumentation and optimization stages, including the
Cargo arguments, base profile overrides, configuration graph, and Cargo, rustc
and `llvm-profdata` identities. It is decided before the optimized build and
catches drift in Temper's own plan.

`compiler_parity` is new and authoritative for compiler inputs. It compares the
inputs the compiler *received*.

| Field | Meaning |
|---|---|
| `normalization` | Version of the digest and normalization contract |
| `scope` | `pgo_training` or `pgo_confirmation` |
| `allowlist` | The closed normalization allowlist |
| `stages` | One bounded summary per available stage |
| `matched` | True only when every comparison completed with no difference |
| `reason` | `compiler_input_mismatch` when `matched` is false |
| `differences` | Bounded difference entries, at most 64 |
| `differences_truncated` | Explicit truncation marker |

A stage summary carries `stage`, `record_count`, `target_compilations`,
`capture_digest` and `normalized_aggregate_digest`. The normalized aggregate
digest is stable across fresh stage roots within one workspace; it is not
stable across different workspace paths.

`pgo_training` scope compares the pass-through reference stage against the
generation stage and against the optimized stage. `pgo_confirmation` scope
compares the accepted optimized stage against the confirmation rebuild and is
decided before promotion.

### The normalization allowlist

The allowlist is closed and was derived from the executed EP-001 matrix in
[`docs/evidence/interposition/2026-07-28/README.md`](evidence/interposition/2026-07-28/README.md):

1. `stage_target_root_paths` — every substring equal to the stage target root is
   normalized to `<STAGE_ROOT>`. EP-001 observed exactly three argv positions
   that depend on it: `--out-dir`, `-L dependency=` and `--extern <crate>=`.
2. `profile_generate_vs_profile_use` — the phase control the shim appends.
3. `pgo_warn_missing_function_in_use` — the use-only missing-function warning.

Cargo artifact identity fields are **not** normalized. EP-001 proved
`-Cmetadata` and `-Cextra-filename` byte-identical across all three phases,
because Cargo computes them before the shim appends anything. They are compared.

Comparison is keyed by logical crate identity, the pair of `--crate-name` and
`-Cmetadata`. That identity separates a package's library from its binary and
two semver-distinct versions of one crate, while staying stable across stages.
Capture creation order is not a difference.

### Difference classes

Every rejection maps to exactly one bounded class:

`evidence_unavailable`, `tool_changed`, `ambiguous_crate_identity`,
`crate_count`, `crate_added`, `crate_missing`, `artifact_metadata_changed`,
`artifact_extra_filename_changed`, `crate_kind_changed`, `argument_count`,
`argument_added`, `argument_removed`, `argument_order`.

An incomplete comparison can never serialize `matched: true`. A missing
reference, generation, optimization or confirmation aggregate records
`evidence_unavailable` and prevents screening or promotion.

## Compiler decisions

`compiler_decisions` is a bounded list, at most 8 entries, of the fail-closed
compiler decisions a run made. Each entry carries:

| Field | Meaning |
|---|---|
| `reason` | Stable reason code |
| `scope` | `pgo_only` or `fatal` |
| `stage` | Interposition stage, when the decision has one |
| `crate_name` | Affected logical crate, when safely available |
| `source` | Affected Cargo configuration source, when safely available |
| `difference_classes` | Bounded parity difference classes |
| `message` | Bounded diagnostic, at most 64 KiB |
| `message_truncated` | Explicit truncation marker |
| `remediation` | One actionable instruction |

Truncation never removes the stable reason, the stage or the scope.

### Reason codes

| Reason | Meaning |
|---|---|
| `compiler_protocol_failure` | The shim aborted before executing rustc and left a bounded marker |
| `ambient_compiler_override` | A nonempty ambient wrapper or `CARGO_BUILD_RUSTC*` variable was set |
| `unproven_compiler_override` | `build.rustc`, `build.rustc-wrapper` or `build.rustc-workspace-wrapper` was declared in the config graph, directly or through `include` |
| `pgo_compiler_input_conflict` | The resolved argv already carried a profiling or coverage control |
| `pgo_compiler_input_ambiguous` | The resolved argv shape is unsupported and was not guessed |
| `compiler_capture_missing` | A stage produced no usable capture evidence |
| `compiler_capture_corrupt` | A capture entry was a symlink, malformed, foreign or identity-mismatched |
| `compiler_capture_limit` | A stage exceeded its record, byte or record-size budget |
| `compiler_injection_unexpected` | A compilation received phase controls the stage did not plan |
| `compiler_input_mismatch` | Observed compiler inputs differed outside the allowlist |
| `cargo_config_include_missing` | A required include does not exist |
| `cargo_config_include_cycle` | The include graph contains a cycle |
| `cargo_config_include_malformed` | An include entry has an unsupported shape |
| `cargo_config_include_unsupported_version` | Stable `include` on Cargo below 1.94 |
| `cargo_config_source_mutated` | A configuration source changed while it was hashed |
| `cargo_config_source_limit` | The configuration graph exceeded its bounds |
| `cargo_config_read_failed`, `cargo_config_parse_failed`, `cargo_version_unrecognized` | The configuration graph could not be established |

A `pgo_only` decision rejects PGO and leaves valid static candidates eligible.
A `fatal` decision belongs to a baseline build, whose artifact is the run's
comparison anchor and therefore cannot be dropped.

When the shim cannot resolve a canonical capture directory inside the current
run, it writes no marker and the parent reports `compiler_capture_missing`
instead of `compiler_protocol_failure`. The marker sharpens the reason; it is
never the mechanism that makes a failure closed.

A shim that refuses an invocation aborts the compile, so Cargo exits nonzero.
`compiler_protocol_failure`, `pgo_compiler_input_conflict` and
`pgo_compiler_input_ambiguous` are the cause of that exit and therefore replace
the generic `cargo_build_failed` reason. Every other capture defect leaves the
Cargo reason in place, because a genuine compilation failure can also produce an
incomplete capture set.

## Durability

Durability ordering is unchanged from schema 2. The emergency manifest is
written before `run.json`, `run.json` is written before any promotion, and
`latest.json` is published only after a confirmed promotion is durable. Schema-3
evidence is therefore always persisted before a success pointer exists.

`--json` emits exactly one schema-3 object on stdout for success and for a
post-run failure. Shim diagnostics, Cargo diagnostics and progress messages
stay on stderr or in bounded fields.
