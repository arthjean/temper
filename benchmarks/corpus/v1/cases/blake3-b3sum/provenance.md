# BLAKE3 b3sum provenance

- Upstream: https://github.com/BLAKE3-team/BLAKE3
- Revision: `8aa5145039b972ba30e98e788752d37d14568824`
- Binary: `b3sum`
- Classification: real, compute-heavy
- License: `CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception`
- Review date: 2026-07-28

The snapshot retains `LICENSE_CC0`, `LICENSE_A2` and `LICENSE_A2LLVM`.
Redistribution is permitted under the upstream alternatives with their notices
preserved. The shallow-clone metadata was removed, and three internal license
symlinks were materialized as regular copies so the corpus contains no
symlinks.

Manual review found no secret, credential, personal dataset or runtime/build
network operation in the selected `b3sum` path. The runner still treats the
snapshot and its build script as trusted arbitrary code and forces Cargo
offline.
