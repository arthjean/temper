#!/usr/bin/env bash
# Shared helpers for the Temper v0.0.3 EP-001 experiments.

set -euo pipefail

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EVIDENCE_DIR="$(cd "$HARNESS_DIR/.." && pwd)"
REPO_DIR="$(cd "$EVIDENCE_DIR/../../../.." && pwd)"
FIXTURE_DIR="$EVIDENCE_DIR/fixtures"
RESULT_DIR="$EVIDENCE_DIR/results"
TARGET_TRIPLE="x86_64-unknown-linux-gnu"

WORK="${TEMPER_EP001_WORK:-${TMPDIR:-/tmp}/temper-ep001}"
BIN_DIR="$WORK/bin"

# The scratch directory is repeatedly cleared, so refuse anything that is not a
# deep absolute path outside the repository.
case "$WORK" in
  /*/*/*) ;;
  *) printf 'refusing an unsafe scratch directory: %s\n' "$WORK" >&2; exit 2 ;;
esac
if [ "${WORK#"$REPO_DIR"}" != "$WORK" ]; then
  printf 'refusing a scratch directory inside the repository: %s\n' "$WORK" >&2
  exit 2
fi

mkdir -p "$RESULT_DIR" "$BIN_DIR"

log() {
  printf '[ep001] %s\n' "$*" >&2
}

build_harness() {
  local program
  for program in shim fake-rustc wrapper; do
    rustc -O --edition 2024 -o "$BIN_DIR/$program" "$HARNESS_DIR/$program.rs"
  done
}

# Builds the unmodified Temper implementation at the current commit.
build_temper() {
  ( cd "$REPO_DIR" && cargo build --release --locked --quiet )
  printf '%s\n' "$REPO_DIR/target/release/cargo-temper"
}

real_rustc() {
  rustc --print sysroot | tr -d '\n' | sed 's|$|/bin/rustc|'
}

# prepare_fixture <fixture-name> <destination> [config-case]
prepare_fixture() {
  local fixture="$1" destination="$2" config="${3:-}"
  rm -rf -- "$destination"
  mkdir -p "$destination"
  ( cd "$FIXTURE_DIR/$fixture" && tar --exclude=./configs -cf - . ) | ( cd "$destination" && tar -xf - )
  if [ -n "$config" ]; then
    mkdir -p "$destination/.cargo"
    cp -r "$FIXTURE_DIR/$fixture/configs/$config/." "$destination/.cargo/"
  fi
  git -C "$destination" init --quiet
  git -C "$destination" config user.name 'Temper EP-001'
  git -C "$destination" config user.email 'temper-ep001@example.invalid'
  git -C "$destination" add .
  git -C "$destination" commit --quiet -m "ep001 fixture"
}

# fresh_capture <path>
fresh_capture() {
  rm -rf -- "$1"
  mkdir -p "$1"
}

json_escape() {
  python3 -c 'import json,sys; print(json.dumps(sys.stdin.read().rstrip("\n")))'
}

host_identity_json() {
  python3 - "$REPO_DIR" <<'PYTHON'
import json, os, platform, subprocess, sys

repo = sys.argv[1]


def run(*command):
    return subprocess.run(command, capture_output=True, text=True).stdout.strip()


print(json.dumps({
    "cargo": run("cargo", "--version"),
    "rustc": run("rustc", "--version"),
    "rustc_verbose": run("rustc", "-vV"),
    "host_triple": next(
        (line.split(": ", 1)[1] for line in run("rustc", "-vV").splitlines() if line.startswith("host: ")),
        "",
    ),
    "kernel": platform.release(),
    "cpu_model": next(
        (line.split(": ", 1)[1].strip() for line in open("/proc/cpuinfo") if line.startswith("model name")),
        "",
    ),
    "logical_cores": os.cpu_count(),
    "temper_commit": run("git", "-C", repo, "rev-parse", "HEAD"),
    "temper_tracked_diff": run("git", "-C", repo, "status", "--porcelain", "--untracked-files=no"),
}, indent=2, sort_keys=True))
PYTHON
}
