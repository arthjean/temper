#!/usr/bin/env bash
# EP-001 / US-001: reproduce the stable Cargo `include` rustflag loss on the
# unmodified Temper v0.0.2 implementation.
#
# The observation channel is a pass-through `RUSTC` shim. Temper v0.0.2 does not
# inspect or reject an ambient `RUSTC`, so the shim records the compiler inputs
# Cargo actually resolved for every build stage without changing Temper.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

CASE_DIR="$RESULT_DIR/us-001"
SENTINEL="temper_included_sentinel"
SUPPORTED_CASES=(direct required-include nested-include optional-missing)
WORKLOAD="$WORK/us001-workload.sh"

mkdir -p "$CASE_DIR"

build_harness
TEMPER_BIN="$(build_temper)"
REAL_RUSTC="$(real_rustc)"

cat >"$WORKLOAD" <<'SHELL'
#!/bin/sh
exec "$TEMPER_BINARY"
SHELL
chmod +x "$WORKLOAD"

host_identity_json >"$CASE_DIR/identity.json"
sha256sum "$TEMPER_BIN" | awk '{print $1}' >"$CASE_DIR/cargo-temper.sha256"

run_direct_build() {
  local project="$1" captures="$2" cargo_bin="${3:-cargo}"
  fresh_capture "$captures"
  # Every direct build starts from an absent target directory: Cargo replays
  # cached diagnostics without invoking rustc when a stage directory is reused.
  rm -rf -- "$captures.target"
  # Cargo discovers `.cargo/config.toml` from the current directory, not from
  # `--manifest-path`, so every direct build runs inside the fixture.
  ( cd "$project" && env \
    RUSTC="$BIN_DIR/shim" \
    TEMPER_EXP_REAL_RUSTC="$REAL_RUSTC" \
    TEMPER_EXP_CAPTURE_DIR="$captures" \
    TEMPER_EXP_STAGE="direct" \
    TEMPER_EXP_TARGET="$TARGET_TRIPLE" \
    TEMPER_EXP_INJECT="none" \
    $cargo_bin build --release --locked --target "$TARGET_TRIPLE" \
    --manifest-path "$project/Cargo.toml" \
    --target-dir "$captures.target" ) \
    >"$captures.stdout" 2>"$captures.stderr"
}

run_temper() {
  local project="$1" captures="$2"
  fresh_capture "$captures"
  ( cd "$project" && env \
    RUSTC="$BIN_DIR/shim" \
    TEMPER_EXP_REAL_RUSTC="$REAL_RUSTC" \
    TEMPER_EXP_CAPTURE_DIR="$captures" \
    TEMPER_EXP_STAGE="temper" \
    TEMPER_EXP_TARGET="$TARGET_TRIPLE" \
    TEMPER_EXP_INJECT="none" \
    "$TEMPER_BIN" temper optimize --manifest-path "$project/Cargo.toml" \
    -- "$WORKLOAD" ) >"$captures.stdout" 2>"$captures.stderr" || true
}

for case_name in "${SUPPORTED_CASES[@]}"; do
  log "US-001 case $case_name"
  project="$WORK/us001-$case_name"
  prepare_fixture include-loss "$project" "$case_name"

  run_direct_build "$project" "$WORK/captures/us001-$case_name-direct"
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$case_name-direct-sentinel.json" \
    sentinel "$WORK/captures/us001-$case_name-direct" "$SENTINEL"

  run_temper "$project" "$WORK/captures/us001-$case_name-temper"
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$case_name-temper-sentinel.json" \
    sentinel "$WORK/captures/us001-$case_name-temper" "$SENTINEL"
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$case_name-temper-records.json" \
    records "$WORK/captures/us001-$case_name-temper"

  run_json="$(find "$project/.temper/runs" -name run.json | head -1)"
  cp "$run_json" "$CASE_DIR/$case_name-run.json"
  mkdir -p "$CASE_DIR/$case_name-config"
  cp -r "$project/.cargo/." "$CASE_DIR/$case_name-config/"
done

# Cargo feature-version boundary: 1.93 predates stable `include`.
log "US-001 Cargo version boundary"
project="$WORK/us001-version-boundary"
prepare_fixture include-loss "$project" required-include
set +e
run_direct_build "$project" "$WORK/captures/us001-version-boundary" "cargo +1.93"
boundary_status=$?
set -e
python3 "$HARNESS_DIR/analyze.py" \
  --out "$CASE_DIR/version-boundary-sentinel.json" \
  sentinel "$WORK/captures/us001-version-boundary" "$SENTINEL"
{
  printf 'cargo_version: '
  cargo +1.93 --version
  printf 'exit_status: %s\n' "$boundary_status"
  printf '%s\n' '--- stderr ---'
  cat "$WORK/captures/us001-version-boundary.stderr"
} >"$CASE_DIR/version-boundary.log"

# Cargo include source failures.
for case_name in missing-required cycle-include; do
  log "US-001 include failure $case_name"
  project="$WORK/us001-$case_name"
  prepare_fixture include-loss "$project" "$case_name"
  set +e
  run_direct_build "$project" "$WORK/captures/us001-$case_name"
  status=$?
  set -e
  {
    printf 'exit_status: %s\n' "$status"
    printf '%s\n' '--- stderr ---'
    cat "$WORK/captures/us001-$case_name.stderr"
  } >"$CASE_DIR/$case_name.log"
done

# Behavioural control: a fixture that cannot compile without the included flag.
log "US-001 strict compile_error control"
project="$WORK/us001-strict"
prepare_fixture include-strict "$project" required-include
run_temper "$project" "$WORK/captures/us001-strict"
python3 "$HARNESS_DIR/analyze.py" \
  --out "$CASE_DIR/strict-sentinel.json" \
  sentinel "$WORK/captures/us001-strict" "$SENTINEL"
run_json="$(find "$project/.temper/runs" -name run.json | head -1)"
cp "$run_json" "$CASE_DIR/strict-run.json"

python3 "$HARNESS_DIR/verify_us001.py" "$CASE_DIR" >"$CASE_DIR/verdict.json"
log "US-001 complete"
cat "$CASE_DIR/verdict.json"
