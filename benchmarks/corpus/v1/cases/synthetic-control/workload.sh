#!/bin/sh
set -eu

case_root=${0%/*}
work_dir="$case_root/.temper-work"
output="$work_dir/output.$$"
mkdir -p "$work_dir"
trap 'rm -f "$output"' EXIT HUP INT TERM

run_scenario() {
    count=$1
    expected=$2
    shift 2
    iteration=0
    while [ "$iteration" -lt "$count" ]; do
        "$TEMPER_BINARY" "$@" >"$output"
        actual=$(sha256sum "$output")
        actual=${actual%% *}
        if [ "$actual" != "$expected" ]; then
            echo "synthetic oracle mismatch" >&2
            exit 1
        fi
        iteration=$((iteration + 1))
    done
}

: "${TEMPER_BINARY:?TEMPER_BINARY is required}"
run_scenario 50 "4b3bd169d6d348fe4bfae168b0da9b5beca6a51c8d69dcba50cf12f942a8cc8e" mix 200000
run_scenario 50 "e973fe88d28f9cca24b36f11f95847e6a9379adf1f0fda5a0d10fb3689beba9e" rotate 200000
