#!/bin/sh
set -eu

case_root=${0%/*}
input="$case_root/inputs/records.csv"
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
            echo "xsv oracle mismatch" >&2
            exit 1
        fi
        iteration=$((iteration + 1))
    done
}

: "${TEMPER_BINARY:?TEMPER_BINARY is required}"
require_sha256 "$input" "b5d1d4947fd29d8bd520077f209b6846c90b72793d7e7d1e9b76fa85b226d316"
run_scenario 55 "ec9913fb6f5f3944b0230e7c8594cf3e6ed24f5726758574c7e5aac5015441fb" stats --everything "$input"
run_scenario 45 "15f325a2d926419e506f18b2cd37109c33dcea5c7f5bdd20725b47542d8a7ad8" select name,value "$input"
