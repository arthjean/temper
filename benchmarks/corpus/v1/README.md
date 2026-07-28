# Temper benchmark corpus v1

Corpus v1 is immutable benchmark input for Temper v0.0.2. It contains three
redistributed real Rust applications and one synthetic harness control. The
control is never included in representative-case counts or performance claims.

The checked-in `manifest.json` is the machine-readable contract. Every path is
relative to this directory. Source snapshots, lockfiles, workload inputs and
oracle outputs are SHA-256 identified. Scenario weights are integer invocation
counts: one workload call executes each named scenario exactly its weight.

Run the fixed reference collection once with:

```sh
./scripts/run-corpus-v1.sh
```

The runner first checks that EP-001, EP-002 and US-001 through US-009 are
`DONE`, validates the manifest and all checksums, then stages each case in a
clean temporary Git repository. Cargo is forced offline. The workload and Cargo
build scripts are trusted arbitrary code, not sandboxed code.

Results are written under `results/reference/2026-07-28`. Existing results are
never overwritten. Any change to source, input, workload, scenario weight,
oracle or manifest entry requires corpus v2; v1 manifests and results remain
historical records.
