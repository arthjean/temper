# Temper

**Temper makes Rust software faster.**

Temper is an experimental optimization toolchain for Rust. Its goal is to measure
how an application behaves under a representative workload, search for better
compilation strategies, and produce a faster binary only when the improvement
can be reproduced.

> [!IMPORTANT]
> Temper v0.0.3 is implemented as an experimental Linux Cargo subcommand. It is
> installable from source but is not published, licensed for public release, or
> covered by a stable interface promise.

## Current implementation

The implemented loop builds the existing release baseline, screens ThinLTO and
Fat LTO with one codegen unit, trains one LLVM PGO candidate, and promotes a
binary only after independent confirmation:

```shell
cargo temper optimize \
  --package my-package \
  --bin my-binary \
  -- ./scripts/temper-workload --dataset representative
```

v0.0.3 hands every compiler input back to Cargo. A private compiler shim appends
the PGO phase controls after Cargo has resolved the project's rustflags, so
flags reaching rustc through a Cargo `include` file, a cfg-keyed target table or
the environment are preserved instead of being reconstructed. Parity is then
decided on the compiler arguments Temper actually observed, and any compiler
wrapper rejects PGO alone while the static candidates stay eligible.

See the [v0.0.3 release contract](docs/v0.0.3.md), the
[v0.0.1 user contract](docs/v0.0.1.md), the
[report schema](docs/schema-v3.md), the
[measurement protocol](docs/measurement-v1.md), the
[compatibility matrix](docs/pgo-compatibility-matrix-v1.md), and the
[verification ledger](docs/verification-v0.0.1.md). The ledger distinguishes
automated tests, synthetic dogfooding, and performance claims.

## Why Temper?

Rust release builds use general-purpose optimization settings. Real applications
have different workloads, bottlenecks, and deployment targets, while techniques
such as link-time optimization, profile-guided optimization, post-link
optimization, and CPU-specific code generation remain difficult to combine and
evaluate reliably.

Temper's intended feedback loop will ground every compilation decision in
measurements against the application's actual workload.

## Target experience

The long-term goal is a single command:

```shell
temper optimize --workload "cargo bench"
```

Temper will:

1. Build and measure a release baseline.
2. Profile the application under the supplied workload.
3. Explore relevant compiler, linker, and post-link strategies.
4. Compare each candidate against the baseline.
5. Keep the best verified binary and produce an optimization report.

## Principles

- **Measured:** every performance claim must be backed by reproducible evidence.
- **Workload-aware:** optimization decisions must reflect real application
  behavior.
- **Cargo-compatible first:** existing Rust projects should work without source
  changes.
- **Explainable:** users should know which decisions improved or degraded the
  result.
- **Reproducible:** the same inputs, workload, and environment should produce
  comparable results.
- **Simple:** advanced optimization should not require advanced compiler
  knowledge.

## Scope

Temper is a compilation toolchain for Rust applications. Its role is compilation
and optimization; applications will not link or build against it as a framework
or library.

Its first objective is runtime performance for compute-intensive and
performance-sensitive Rust applications. Faster builds, deeper compiler
integration, and a modern Cargo-compatible build system belong to the longer-term
vision.

Cargo compatibility defines the initial path. Consistently faster binaries within
the existing Rust ecosystem will provide the foundation for a broader build
system.

## Roadmap

- Publish a representative benchmark corpus and its evaluation methodology.
- Ship a Cargo-compatible prototype that optimizes existing projects without
  source changes.
- Validate runtime improvements across different workloads and deployment
  targets.
- Deepen compiler integration only where benchmark evidence justifies it.
- Explore an integrated build system compatible with existing `Cargo.toml` and
  `Cargo.lock` files after the optimizer proves its value.

## Initial success criterion

Temper's first technical milestone is to improve a predeclared primary runtime
metric across a representative application corpus, compared with the same source
built using `cargo build --release`. The benchmark methodology, optimization
environment, and correctness checks must be public and reproducible.

## Why the name?

Tempering is a controlled process that improves a material's properties. Temper
applies the same idea to Rust software: observe, refine, measure, and keep only
what makes the result stronger.

## Contributing

Temper is being shaped in public. If you work on Rust compiler internals,
profiling, benchmarking, linkers, or performance engineering, you can contribute
by opening an issue with a concrete use case, workload, or technical proposal.

## License

Temper will receive an open-source license before the first implementation is
published. The license has not yet been selected.
