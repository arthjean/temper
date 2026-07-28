# Cargo rustflags composition fixture

Observed with `cargo 1.97.1 (c980f4866 2026-06-30)` on
`x86_64-unknown-linux-gnu`.

This fixture records how **Cargo itself** composes `CARGO_ENCODED_RUSTFLAGS`
with configured target rustflags. It is a Cargo-behaviour contract, exercised
by [`tests/ep001_v002_contract.rs`](../../ep001_v002_contract.rs) through a
capturing `RUSTC` wrapper rather than through Temper.

| Target rustflags source | Cargo composition observed |
|---|---|
| Absent | The encoded environment value is the only rustflags source |
| String without quoting or escaping | ASCII-whitespace arguments, then the encoded value |
| Array of strings | Exact array elements, including embedded spaces, then the encoded value |

Since v0.0.3 Temper no longer reconstructs any of this: Cargo resolves every
compiler input and the private rustc shim appends the PGO phase controls after
it, so no rustflags shape is rejected for composition reasons any more.

The workspace includes an application build script and a proc macro so the
integration test can prove that explicit `--target` keeps the target PGO flag
off host rustc invocations.
