# Temper v0.0.1 session handoff

This is the compact restart point for a new agent session. It captures the
state reached on 2026-07-28 without duplicating the PRD or verification ledger.

## Current outcome

Temper v0.0.1 is implemented and the PRD status is `DONE`. The required Rust
quality gates passed, followed by a 12-scenario synthetic dogfooding matrix. No
product defect was found.

Use these durable sources:

- [`../tasks/prd-temper-v0.0.1-status.json`](../tasks/prd-temper-v0.0.1-status.json)
  for delivery status;
- [`../tasks/prd-temper-v0.0.1.md`](../tasks/prd-temper-v0.0.1.md) for the
  product contract;
- [`measurement-v1.md`](measurement-v1.md) for the statistical contract;
- [`verification-v0.0.1.md`](verification-v0.0.1.md) for tests, dogfooding
  results, scores, audit evidence, and known limits;
- [`v0.0.1.md`](v0.0.1.md) for the supported user-facing boundary.

## Documentation state

The 2026-07-28 documentation pass established:

- `AGENTS.md` as the canonical repository map for future Codex sessions;
- `CLAUDE.md` as a minimal import bridge to `AGENTS.md`;
- `docs/verification-v0.0.1.md` as the historical evidence ledger;
- links from `README.md` and `docs/v0.0.1.md` to current implementation and
  verification status.

That pass changed no Rust source, test, manifest, or lockfile.

## Critical interpretation rules

- The temporary dogfooding projects and raw `.temper` trees were moved to the
  system trash. Only the aggregate verification ledger is durable.
- The forced-promotion result used path-dependent sleeps. It validates
  promotion mechanics and is not a compiler performance result.
- There is no checked-in benchmark corpus, Cargo bench target, or
  production-representative speedup evidence yet.
- New performance evidence should retain raw `run.json`, workload source,
  project revision, environment fingerprint, and artifact checksums.

## Starting the next session

1. Read `AGENTS.md`, then this handoff.
2. Read only the contract or evidence document relevant to the requested task.
3. Inspect `git status` and preserve any existing worktree changes.
4. Do not rerun broad validation unless the task or delivery request authorizes
   it.
5. Append new dated evidence to the verification ledger instead of replacing
   historical results.

## Suggested skills

- `code-review`: review implementation changes against the fixed PRD boundary.
- `diagnosing-bugs`: investigate a reproducible orchestration or measurement
  failure before editing.
- `rust-doctor`: perform a broader Rust health audit when explicitly requested.
- `implement`: execute a new scoped PRD or ticket after the v0.0.1 baseline.
- `handoff`: refresh this restart document when transferring substantial work.
