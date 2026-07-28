#!/bin/sh
set -eu

script_directory=${0%/*}
repository=$(CDPATH= cd -- "$script_directory/.." && pwd)
cd "$repository"

printf '%s\n' \
    "Trust warning: corpus Cargo build scripts and workloads execute with host-user privileges; offline Cargo is not a sandbox." >&2
export CARGO_NET_OFFLINE=true
cargo test --test ep003_corpus --locked
TEMPER_CORPUS_COLLECT=1 cargo test \
    --test ep003_corpus \
    --locked \
    collect_corpus_v1_reference_evidence \
    -- \
    --ignored \
    --exact \
    --nocapture
