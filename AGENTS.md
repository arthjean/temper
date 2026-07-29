# Temper repository guidance

## Current state

- Temper v0.0.3 is implemented and emits report schema 3. The authoritative
  delivery status is
  [`tasks/prd-temper-v0.0.3-status.json`](tasks/prd-temper-v0.0.3-status.json);
  v0.0.1 and v0.0.2 trackers are immutable history.
- Treat [`tasks/prd-temper-v0.0.1.md`](tasks/prd-temper-v0.0.1.md) plus
  [`tasks/prd-temper-v0.0.3.md`](tasks/prd-temper-v0.0.3.md) as the product
  contract and [`docs/measurement-v1.md`](docs/measurement-v1.md) as the
  statistical contract. Measurement-v1 is unchanged by v0.0.3.
- The compiler-input contract is
  [`docs/v0.0.3.md`](docs/v0.0.3.md), the report contract is
  [`docs/schema-v3.md`](docs/schema-v3.md), and the executable boundary is
  [`docs/pgo-compatibility-matrix-v1.md`](docs/pgo-compatibility-matrix-v1.md).
  Schema-1 and schema-2 records are never migrated or reinterpreted.
- Read [`docs/verification-v0.0.1.md`](docs/verification-v0.0.1.md) before
  claiming test coverage, dogfooding results, benchmark scores, or release
  readiness.
- For a compact session restart, read
  [`docs/handoff-v0.0.1.md`](docs/handoff-v0.0.1.md).
- For the post-v0.0.1 Rust and Cargo architecture map, integration boundaries,
  findings, and targeted follow-ups, read
  [`docs/research/rust-cargo-codebase-survey.md`](docs/research/rust-cargo-codebase-survey.md).

## Validation

- When broad validation is authorized, use the PRD quality gates exactly:
  `cargo fmt --check`, `cargo check --all-targets --locked`,
  `cargo clippy --all-targets --all-features --locked --no-deps -- -D warnings`,
  and `cargo test --all-targets --locked`.
- Use `--locked` for Cargo validation and builds. Do not regenerate
  `Cargo.lock` unless dependency changes require it.
- Prefer the integration test matching the changed surface before a broad pass:
  `tests/ep002_interposition.rs` for the shim and capture protocol,
  `tests/ep003_schema.rs` for schema-3 evidence and parity,
  `tests/ep004_config_matrix.rs` for Cargo rustflags and `include` provenance,
  `tests/ep004_wrapper_matrix.rs` for the wrapper, isolation and evidence
  boundary, and `tests/ep004_confirmation.rs` for promotion and durability.
- Ignored collectors write retained evidence and are never part of a normal
  pass: `scripts/collect-v0.0.3-evidence.sh` and `scripts/run-corpus-v1.sh`.

## Evidence discipline

- The 2026-07-28 dogfooding fixtures were temporary and are not present in this
  repository. Their aggregate results are historical evidence, not a reusable
  benchmark corpus.
- Never cite the forced-promotion scenario as an optimization gain. Its
  path-dependent sleeps deliberately manufactured the decision boundary.
- This repository currently has no production-representative benchmark corpus
  and no Cargo bench target. Say so directly. Corpus-v1 workloads are bounded
  local proxies; never publish an aggregate cross-application score.
- For new dogfooding or benchmarks, append a dated section to the verification
  ledger with the tested commit, toolchain, host, workload definition, raw
  result location, and whether the workload is synthetic or representative.
  Preserve raw `run.json` records when a score may be cited later.

## Product boundary

- The supported CLI, workload contract, strategy set, report paths, and
  non-goals are documented in [`docs/v0.0.1.md`](docs/v0.0.1.md); v0.0.3 narrows
  the PGO boundary in [`docs/v0.0.3.md`](docs/v0.0.3.md).
- Temper supports Linux `x86_64-unknown-linux-gnu` host Cargo binaries with an
  existing lockfile. Do not silently widen that boundary.
- PGO additionally requires Cargo 1.94 or newer when a config declares
  `include`, and rejects every compiler wrapper and every `build.rustc`. That
  rejection is nonfatal: static candidates remain eligible. Do not add a
  wrapper bypass without a cache-key proof.
