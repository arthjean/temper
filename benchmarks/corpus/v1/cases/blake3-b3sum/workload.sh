#!/bin/sh
set -eu

case_root=${0%/*}
input="$case_root/source/test_vectors/test_vectors.json"
work_dir="$case_root/.temper-work"
output="$work_dir/output.$$"
mkdir -p "$work_dir"
trap 'rm -f "$output"' EXIT HUP INT TERM

require_sha256() {
    path=$1
    expected=$2
    actual=$(sha256sum "$path")
    actual=${actual%% *}
    if [ "$actual" != "$expected" ]; then
        echo "input checksum mismatch: $path" >&2
        exit 1
    fi
}

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
            echo "b3sum oracle mismatch" >&2
            exit 1
        fi
        iteration=$((iteration + 1))
    done
}

: "${TEMPER_BINARY:?TEMPER_BINARY is required}"
require_sha256 "$input" "dcb91ea8accc77e6d6e632af7cdc1a99a9f3ae78cf648da595c7d064db32f624"
run_scenario 60 "2165a523b134985df7b998889cb0cd90d1ffb06e873d4b5002147f60bf84c259" --no-names --length 32 "$input"
run_scenario 40 "9d1642f4c0490e63017d881624df5e93e5c5fe96d732724ad4dab080a0fa8a20" --no-names --length 64 "$input"
