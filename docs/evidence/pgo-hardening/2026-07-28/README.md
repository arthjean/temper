# Temper v0.0.2 PGO-hardening evidence

Date: 2026-07-28.

This directory retains correctness evidence only. It makes no optimization-gain or production-performance claim. The four supported cases completed instrumentation, training, strict merge, optimized build and screening. The incompatible-profile control changed fixture source only after training and was rejected as `pgo_missing_profile_data` with zero PGO screening samples.

Each case retains the exact fixture source, workload and raw `run.json`. `summary.json` records the host/toolchain identity, baseline Git commit, the dirty implementation-tree checksum and independent artifact/profile checksum rechecks performed before temporary build directories were released.
