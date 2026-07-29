#!/bin/sh
set -eu

# Collects the retained v0.0.3 release evidence into one dated directory.
#
# Usage: scripts/collect-v0.0.3-evidence.sh <YYYY-MM-DD>
#
# Correctness and cost evidence only. The include fixture is synthetic and the
# corpus workload stays a bounded local proxy; neither supports an optimization
# gain or a production-representativeness claim.

date=${1:?"usage: collect-v0.0.3-evidence.sh <YYYY-MM-DD>"}
script_directory=${0%/*}
repository=$(CDPATH= cd -- "$script_directory/.." && pwd)
cd "$repository"

root="docs/evidence/v0.0.3/$date"
if [ -e "$root" ]; then
    printf 'refusing to overwrite retained evidence at %s\n' "$root" >&2
    exit 1
fi
mkdir -p "$root"

printf '%s\n' \
    "Trust warning: corpus Cargo build scripts and workloads execute with host-user privileges; offline Cargo is not a sandbox." >&2

TEMPER_EVIDENCE_DATE="$date" \
TEMPER_EVIDENCE_DIR="$repository/$root/include-pgo" \
    cargo test --test ep004_release_evidence --locked \
    collect_v003_include_pgo_evidence -- --ignored --exact --nocapture

TEMPER_OVERHEAD_REPORT="$repository/$root/overhead.json" \
    cargo test --test ep004_wrapper_matrix --locked \
    paired_pass_through_builds_keep_their_identity_and_overhead_budget \
    -- --ignored --exact --nocapture

CARGO_NET_OFFLINE=true \
TEMPER_EVIDENCE_DATE="$date" \
TEMPER_EVIDENCE_DIR="$repository/$root/corpus-hexyl" \
TEMPER_CORPUS_CASE="${TEMPER_CORPUS_CASE:-hexyl}" \
    cargo test --test ep003_corpus --locked \
    collect_v003_corpus_case_evidence -- --ignored --exact --nocapture

printf 'retained v0.0.3 evidence in %s\n' "$root"
