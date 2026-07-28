#!/bin/sh
set -eu

case_root=${0%/*}
input="$case_root/source/tests/examples/hello_world_elf64"
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
            echo "hexyl oracle mismatch" >&2
            exit 1
        fi
        iteration=$((iteration + 1))
    done
}

: "${TEMPER_BINARY:?TEMPER_BINARY is required}"
require_sha256 "$input" "c6f999f766c94ded605701e5a3ad3aced09796af3410c7d88c6605d0d73edb91"
run_scenario 60 "d2673e9c1cba4bf8331a4c768a7d9649fa2080aa08c28d6a91504237340b6509" --plain "$input"
run_scenario 40 "05548de4a225c006ed06af02dd478006b4095affb15799d79116be599b534622" --plain --skip 1024 --length 4096 "$input"
