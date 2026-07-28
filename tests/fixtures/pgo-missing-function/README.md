# Missing-function diagnostic contract

This fixture was validated on 2026-07-28 with:

- `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6;
- `cargo 1.97.1 (c980f4866 2026-06-30)`;
- adjacent `llvm-profdata 22.1.6-rust-1.97.1-stable`.

The integration test builds `src/main.rs` with `-Cprofile-generate`, executes
it, merges the raw profile with `--failure-mode=any`, replaces it with
`src/optimized.rs`, then rebuilds with `-Cprofile-use` and
`-Cllvm-args=-pgo-warn-missing-function`.

Cargo exits successfully and emits a raw JSON `compiler-message` parsed by
`cargo_metadata` 0.23.1 with:

- `level: warning`;
- no diagnostic code;
- the fixture package ID and binary target identity;
- a primary message shaped as
  `<codegen-unit>: no profile data available for function <symbol> ...`;
- bounded rendered warning text.

The production fingerprint requires warning level, no code, and a message
segment after the first `: ` that starts with
`no profile data available for function `. The fixture also emits an unrelated
`dead_code` warning, which must not match.

