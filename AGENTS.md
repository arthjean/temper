# Temper repository guidance

## Current state

- Temper v0.0.1 is implemented. The authoritative delivery status is
  [`tasks/prd-temper-v0.0.1-status.json`](tasks/prd-temper-v0.0.1-status.json).
- The next planned work is
  [`tasks/prd-temper-v0.0.2.md`](tasks/prd-temper-v0.0.2.md), tracked by
  [`tasks/prd-temper-v0.0.2-status.json`](tasks/prd-temper-v0.0.2-status.json).
  EP-003 corpus work is blocked until EP-001 and EP-002 are both `DONE`.
- Treat [`tasks/prd-temper-v0.0.1.md`](tasks/prd-temper-v0.0.1.md) as the
  product contract and [`docs/measurement-v1.md`](docs/measurement-v1.md) as
  the statistical contract.
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
- Prefer the integration test matching the changed epic before a broad pass:
  `tests/ep001_*.rs`, `tests/ep002_workload.rs`,
  `tests/ep003_strategy.rs`, or `tests/ep004_confirmation.rs`.

## Evidence discipline

- The 2026-07-28 dogfooding fixtures were temporary and are not present in this
  repository. Their aggregate results are historical evidence, not a reusable
  benchmark corpus.
- Never cite the forced-promotion scenario as an optimization gain. Its
  path-dependent sleeps deliberately manufactured the decision boundary.
- This repository currently has no production-representative benchmark corpus
  and no Cargo bench target. Say so directly.
- For new dogfooding or benchmarks, append a dated section to the verification
  ledger with the tested commit, toolchain, host, workload definition, raw
  result location, and whether the workload is synthetic or representative.
  Preserve raw `run.json` records when a score may be cited later.

## Product boundary

- The supported CLI, workload contract, strategy set, report paths, and
  non-goals are documented in [`docs/v0.0.1.md`](docs/v0.0.1.md).
- Temper v0.0.1 supports Linux `x86_64-unknown-linux-gnu` host Cargo binaries
  with an existing lockfile. Do not silently widen that boundary.
