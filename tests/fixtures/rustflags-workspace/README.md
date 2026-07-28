# Cargo rustflags composition fixture

Observed with `cargo 1.97.1 (c980f4866 2026-06-30)` on
`x86_64-unknown-linux-gnu`.

| Target rustflags source | Temper composition |
|---|---|
| Absent | Owned encoded environment channel contains only the PGO flag |
| String without quoting or escaping | ASCII-whitespace arguments, then PGO |
| Array of strings | Exact array elements, including embedded spaces, then PGO |
| String requiring quoting or escaping | Rejected as `unsupported_string_rustflags_boundaries` |
| Ambient encoded/plain, build, cfg-selected, or multiple target sources | Rejected before the PGO Cargo build |

The workspace includes an application build script and a proc macro so the
integration test can prove that explicit `--target` keeps the target PGO flag
off host rustc invocations.
