# Corpus changelog

## v1.0.0 - 2026-07-28

- Added BLAKE3 `b3sum` as the compute-heavy application.
- Added `xsv` as the parsing and transformation application.
- Added `hexyl` as the streaming application.
- Added `temper-corpus-control` as a non-representative synthetic harness
  control.
- Fixed source revisions, local inputs, scenario weights, correctness oracles
  and reference evidence paths.

Corpus entries are immutable. A source, input, workload, weight or oracle
change creates a new corpus version.
