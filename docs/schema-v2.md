# Temper report schema 2

Schema 2 applies only to runs created by the v0.0.2 implementation. Temper does
not discover, rewrite, migrate, or reinterpret existing schema-1 `run.json` or
`latest.json` files. All historical schema-1 fields keep their documented
meaning.

The CLI identifies new human reports as `schema 2`. `--json`, `run.json`, and
new `latest.json` records declare `"schema_version": 2`.

## Build environment evidence

Each build invocation adds `environment_overrides`. An override records its
variable name and ordered `arguments`; it never serializes rustflags as a
space-joined string. Static strategies normally record an empty array. PGO uses
exactly one process-local `CARGO_ENCODED_RUSTFLAGS` entry whose arguments are
the proven project target rustflags followed by the phase-specific PGO flag.
The parent Temper process environment is unchanged.

Thin and fat LTO remain in `cargo_config_overrides` as Cargo release-profile
configuration. PGO does not add a list-valued `target.<triple>.rustflags`
override.

## PGO phase parity

Every `pgo_training` object contains `phase_parity`, including rejected attempts.
Its `instrumentation` and `optimization` inputs record:

- package ID, binary target, target triple, exact Cargo arguments and isolated
  target directory;
- base Cargo profile overrides and ordered project rustflags;
- every inspected Cargo configuration path and SHA-256;
- Cargo, rustc, and adjacent `llvm-profdata` identities;
- ordered process-local environment overrides.

`permitted_differences` contains exactly `target_directory`,
`profile_generate_vs_use`, and `pgo_warn_missing_function_in_use`. A complete
comparison sets `matched: true` only when `unexpected_differences` is empty.
Unavailable inputs or any changed package, target, profile, project flag,
configuration source/hash, Cargo identity, rustc identity, or
`llvm-profdata` identity set `matched: false`. Temper persists the rejection
before an optimized PGO candidate can be screened.

## Cargo stream failures

Schema-2 build failures include a stable `reason`. Relevant values are
`unrecognized_cargo_json`, `invalid_cargo_message`,
`cargo_executable_missing`, and `cargo_executable_ambiguous`. Plain non-JSON
Cargo output remains bounded diagnostic text.

Compiler diagnostics are also retained as bounded structured records containing
package ID, target name, target kind, target source path, level, optional code,
primary message and rendered text. The combined persisted diagnostic budget for
one Cargo build remains 64 KiB.

The optimized PGO build adds
`-Cllvm-args=-pgo-warn-missing-function`. A warning with no diagnostic code
whose primary message segment begins
`no profile data available for function ` rejects only PGO with reason
`pgo_missing_profile_data`. Its structured diagnostic and optimized invocation
remain in the PGO strategy rejection record; it receives no screening samples.

## PGO profile evidence

Each `raw_profile_files` item records a canonical in-run path, byte length and
SHA-256 in deterministic path order. Profile discovery rejects symbolic links,
non-regular `.profraw` entries and more than 10,000 inputs before merge.

The merge record persists exact ordered arguments beginning with
`merge --failure-mode=any -o`. Any nonzero exit or stdout/stderr rejects the
merge. A successful record requires one nonempty regular `.profdata` inside the
run PGO directory and records its canonical path, byte length and SHA-256.
