#!/usr/bin/env bash
# EP-001 / US-003: validate the compiler-wrapper and cache boundary, Cargo
# fingerprint behaviour for shim-appended flags, target-directory isolation and
# the pass-through overhead budget.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

CASE_DIR="$RESULT_DIR/us-003"
UNITS="$WORK/us003-units"
PACKAGE="temper-units-app"
BINARY="temper-units-app"
WORKLOAD="$WORK/us003-workload.sh"
SHIM_ENV=()

mkdir -p "$CASE_DIR"

build_harness
TEMPER_BIN="$(build_temper)"
REAL_RUSTC="$(real_rustc)"
LLVM_PROFDATA="$(dirname "$(rustc --print target-libdir)")/bin/llvm-profdata"
cp "$BIN_DIR/wrapper" "$BIN_DIR/general-wrapper"
cp "$BIN_DIR/wrapper" "$BIN_DIR/workspace-wrapper"
cp "$BIN_DIR/wrapper" "$BIN_DIR/cache-wrapper"

cat >"$WORKLOAD" <<'SHELL'
#!/bin/sh
exec "$TEMPER_BINARY"
SHELL
chmod +x "$WORKLOAD"

host_identity_json >"$CASE_DIR/identity.json"
prepare_fixture units "$UNITS"

set_shim_env() {
  SHIM_ENV=(
    "RUSTC=$BIN_DIR/shim"
    "TEMPER_EXP_REAL_RUSTC=$REAL_RUSTC"
    "TEMPER_EXP_CAPTURE_DIR=$1"
    "TEMPER_EXP_STAGE=$2"
    "TEMPER_EXP_TARGET=$TARGET_TRIPLE"
    "TEMPER_EXP_INJECT=$3"
  )
}

units_build() {
  local target_dir="$1" logs="$2"
  shift 2
  rm -rf -- "$target_dir"
  ( cd "$UNITS" && env "$@" \
    cargo build --release --locked --target "$TARGET_TRIPLE" \
    --package "$PACKAGE" --bin "$BINARY" --target-dir "$target_dir" ) \
    >"$logs.stdout" 2>"$logs.stderr"
}

executable_of() {
  printf '%s\n' "$1/$TARGET_TRIPLE/release/$BINARY"
}

# --- 1. Documented wrapper chain order --------------------------------------
mkdir -p "$WORK/us003-profraw"
for mode in none general workspace nested; do
  log "US-003 wrapper chain order: $mode"
  WRAPPER_LOG="$WORK/us003-wrapper-$mode.jsonl"
  CAPTURES="$WORK/captures/us003-wrapper-$mode"
  rm -f -- "$WRAPPER_LOG"
  fresh_capture "$CAPTURES"
  set_shim_env "$CAPTURES" "wrapper-$mode" "generate=$WORK/us003-profraw"
  WRAPPER_ENV=("TEMPER_EXP_WRAPPER_LOG=$WRAPPER_LOG" "TEMPER_EXP_WRAPPER_MODE=log")
  case "$mode" in
    general) WRAPPER_ENV+=("RUSTC_WRAPPER=$BIN_DIR/general-wrapper") ;;
    workspace) WRAPPER_ENV+=("RUSTC_WORKSPACE_WRAPPER=$BIN_DIR/workspace-wrapper") ;;
    nested)
      WRAPPER_ENV+=(
        "RUSTC_WRAPPER=$BIN_DIR/general-wrapper"
        "RUSTC_WORKSPACE_WRAPPER=$BIN_DIR/workspace-wrapper"
      )
      ;;
  esac
  units_build "$WORK/us003-target-wrapper-$mode" "$CAPTURES" \
    "${SHIM_ENV[@]}" "${WRAPPER_ENV[@]}"
  if [ -f "$WRAPPER_LOG" ]; then
    cp "$WRAPPER_LOG" "$CASE_DIR/wrapper-$mode.jsonl"
  else
    : >"$CASE_DIR/wrapper-$mode.jsonl"
  fi
done
python3 "$HARNESS_DIR/analyze.py" --out "$CASE_DIR/wrapper-nested-records.json" \
  records "$WORK/captures/us003-wrapper-nested"

# --- 2. A real merged profile for the cache experiment ----------------------
log "US-003 instrumented reference profile"
PROFILE_RAW="$WORK/us003-instr-profraw"
PROFILE_DATA="$WORK/us003-instr.profdata"
rm -rf -- "$PROFILE_RAW"
mkdir -p "$PROFILE_RAW"
CAPTURES="$WORK/captures/us003-instr"
fresh_capture "$CAPTURES"
set_shim_env "$CAPTURES" instrumented "generate=$PROFILE_RAW"
units_build "$WORK/us003-target-instr" "$CAPTURES" "${SHIM_ENV[@]}"
for _ in 1 2 3; do
  LLVM_PROFILE_FILE="$PROFILE_RAW/%p-%m.profraw" \
    "$(executable_of "$WORK/us003-target-instr")" >/dev/null
done
"$LLVM_PROFDATA" merge --failure-mode=any -o "$PROFILE_DATA" "$PROFILE_RAW"/*.profraw

# --- 3. Outer cache key across PGO phases -----------------------------------
log "US-003 outer cache key across PGO phases"
CACHE_LOG="$WORK/us003-cache.jsonl"
CACHE_DIR="$WORK/us003-cache"
CACHE_PROFRAW="$WORK/us003-cache-profraw"
rm -rf -- "$CACHE_DIR" "$CACHE_PROFRAW"
rm -f -- "$CACHE_LOG"
mkdir -p "$CACHE_DIR" "$CACHE_PROFRAW"
for stage in reference generate use; do
  case "$stage" in
    reference) inject="none" ;;
    generate) inject="generate=$CACHE_PROFRAW" ;;
    use) inject="use=$PROFILE_DATA" ;;
  esac
  target_dir="$WORK/us003-target-cache-$stage"
  captures="$WORK/captures/us003-cache-$stage"
  fresh_capture "$captures"
  set_shim_env "$captures" "cache-$stage" "$inject"
  units_build "$target_dir" "$captures" \
    "${SHIM_ENV[@]}" \
    "RUSTC_WRAPPER=$BIN_DIR/cache-wrapper" \
    "TEMPER_EXP_WRAPPER_LOG=$CACHE_LOG" \
    "TEMPER_EXP_WRAPPER_MODE=cache" \
    "TEMPER_EXP_WRAPPER_CACHE_DIR=$CACHE_DIR" \
    "TEMPER_EXP_WRAPPER_STRIP=$target_dir"
  sha256sum "$(executable_of "$target_dir")" | awk '{print $1}' \
    >"$CASE_DIR/cache-$stage.sha256"
done
cp "$CACHE_LOG" "$CASE_DIR/cache-keys.jsonl"

# Does the artifact the instrumented phase produced actually instrument?
PROFILE_PROBE="$WORK/us003-cache-runtime"
rm -rf -- "$PROFILE_PROBE"
mkdir -p "$PROFILE_PROBE"
LLVM_PROFILE_FILE="$PROFILE_PROBE/%p-%m.profraw" \
  "$(executable_of "$WORK/us003-target-cache-generate")" >/dev/null
find "$PROFILE_PROBE" -name '*.profraw' | wc -l \
  >"$CASE_DIR/cache-generate-profraw-count.txt"
LLVM_PROFILE_FILE="$PROFILE_PROBE/control-%p-%m.profraw" \
  "$(executable_of "$WORK/us003-target-instr")" >/dev/null
find "$PROFILE_PROBE" -name 'control-*.profraw' | wc -l \
  >"$CASE_DIR/uncached-generate-profraw-count.txt"

# --- 4. Cargo fingerprints ignore shim-appended flags -----------------------
log "US-003 target-directory reuse and Cargo freshness"
REUSE_DIR="$WORK/us003-target-reuse"
FIRST="$WORK/captures/us003-reuse-first"
SECOND="$WORK/captures/us003-reuse-second"
fresh_capture "$FIRST"
set_shim_env "$FIRST" reuse-first none
units_build "$REUSE_DIR" "$FIRST" "${SHIM_ENV[@]}"
first_sha="$(sha256sum "$(executable_of "$REUSE_DIR")" | awk '{print $1}')"
fresh_capture "$SECOND"
set_shim_env "$SECOND" reuse-second "generate=$WORK/us003-profraw"
# Deliberately reuse the same target directory for a different PGO phase.
( cd "$UNITS" && env "${SHIM_ENV[@]}" \
  cargo build --release --locked --target "$TARGET_TRIPLE" \
  --package "$PACKAGE" --bin "$BINARY" --target-dir "$REUSE_DIR" ) \
  >"$SECOND.stdout" 2>"$SECOND.stderr"
second_sha="$(sha256sum "$(executable_of "$REUSE_DIR")" | awk '{print $1}')"
# A canonical alias of the same directory is equally invisible to Cargo.
ALIAS_LINK="$WORK/us003-target-reuse-alias"
ALIAS="$WORK/captures/us003-reuse-alias"
rm -f -- "$ALIAS_LINK"
ln -s "$REUSE_DIR" "$ALIAS_LINK"
fresh_capture "$ALIAS"
set_shim_env "$ALIAS" reuse-alias "use=$PROFILE_DATA"
( cd "$UNITS" && env "${SHIM_ENV[@]}" \
  cargo build --release --locked --target "$TARGET_TRIPLE" \
  --package "$PACKAGE" --bin "$BINARY" --target-dir "$ALIAS_LINK" ) \
  >"$ALIAS.stdout" 2>"$ALIAS.stderr"
alias_sha="$(sha256sum "$(executable_of "$REUSE_DIR")" | awk '{print $1}')"

# A precreated stage directory is accepted by Cargo without any warning.
PRECREATED="$WORK/us003-target-precreated"
PRE="$WORK/captures/us003-precreated"
rm -rf -- "$PRECREATED"
mkdir -p "$PRECREATED/$TARGET_TRIPLE/release/deps"
fresh_capture "$PRE"
set_shim_env "$PRE" precreated none
( cd "$UNITS" && env "${SHIM_ENV[@]}" \
  cargo build --release --locked --target "$TARGET_TRIPLE" \
  --package "$PACKAGE" --bin "$BINARY" --target-dir "$PRECREATED" ) \
  >"$PRE.stdout" 2>"$PRE.stderr"

python3 - "$FIRST" "$SECOND" "$first_sha" "$second_sha" "$ALIAS" "$alias_sha" "$PRE" \
  >"$CASE_DIR/target-directory-reuse.json" <<'PYTHON'
import json, pathlib, sys

first, second, first_sha, second_sha, alias, alias_sha, precreated = sys.argv[1:8]


def invocations(path):
    return len(list(pathlib.Path(path).glob("*.json")))


print(json.dumps({
    "first_invocations": invocations(first),
    "second_invocations": invocations(second),
    "alias_invocations": invocations(alias),
    "precreated_invocations": invocations(precreated),
    "first_sha256": first_sha,
    "second_sha256": second_sha,
    "alias_sha256": alias_sha,
    "artifact_unchanged": first_sha == second_sha == alias_sha,
}, indent=2, sort_keys=True))
PYTHON

# Fresh per-stage directories must recompile every target unit, and an outer
# cache must be shown to remove the shim from the phase entirely.
python3 - "$CASE_DIR/fresh-stage-directories.json" \
  "wrapper_free:$WORK/captures/us003-reuse-first" \
  "wrapper_free:$WORK/captures/us003-instr" \
  "wrapper_free:$WORK/captures/us003-wrapper-order" \
  "cached:$WORK/captures/us003-cache-reference" \
  "cached:$WORK/captures/us003-cache-generate" \
  "cached:$WORK/captures/us003-cache-use" <<'PYTHON'
import json, pathlib, sys

output = pathlib.Path(sys.argv[1])
report = {"wrapper_free": {}, "cached": {}}
for argument in sys.argv[2:]:
    group, path = argument.split(":", 1)
    records = [json.loads(item.read_text()) for item in pathlib.Path(path).glob("*.json")]
    report[group][pathlib.Path(path).name] = {
        "target_compiles": sum(1 for item in records if item["class"] == "target_compile"),
        "host_compiles": sum(1 for item in records if item["class"] == "host_compile"),
        "shim_invocations": len(records),
    }
output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
PYTHON

# --- 5. v0.0.2 wrapper and compiler-override detection ----------------------
log "US-003 v0.0.2 wrapper and compiler override detection"
detect_case() {
  local config="$1" label="$2"
  local project="$WORK/us003-detect-$label"
  local captures="$WORK/captures/us003-detect-$label"
  prepare_fixture include-loss "$project" "$config"
  fresh_capture "$captures"
  # A configured `build.rustc` points at the shim so the evidence shows which
  # compiler Cargo actually used while Temper identified the ambient one.
  sed -i "s|WRAPPER_PATH|$BIN_DIR/general-wrapper|" "$project"/.cargo/*.toml
  sed -i "s|RUSTC_PATH|$BIN_DIR/shim|" "$project"/.cargo/*.toml
  git -C "$project" add .
  git -C "$project" commit --quiet -m "compiler configuration"
  local wrapper_log="$WORK/us003-detect-$label.jsonl"
  rm -f -- "$wrapper_log"
  ( cd "$project" && env \
    "TEMPER_EXP_REAL_RUSTC=$REAL_RUSTC" \
    "TEMPER_EXP_CAPTURE_DIR=$captures" \
    "TEMPER_EXP_STAGE=detect-$label" \
    "TEMPER_EXP_TARGET=$TARGET_TRIPLE" \
    "TEMPER_EXP_INJECT=none" \
    "TEMPER_EXP_WRAPPER_LOG=$wrapper_log" \
    "TEMPER_EXP_WRAPPER_MODE=log" \
    "$TEMPER_BIN" temper optimize --manifest-path "$project/Cargo.toml" \
    -- "$WORKLOAD" ) >"$WORK/us003-detect-$label.stdout" 2>"$WORK/us003-detect-$label.stderr" || true
  cp "$(find "$project/.temper/runs" -name run.json | head -1)" "$CASE_DIR/detect-$label-run.json"
  if [ -f "$wrapper_log" ]; then
    cp "$wrapper_log" "$CASE_DIR/detect-$label-wrapper.jsonl"
  else
    : >"$CASE_DIR/detect-$label-wrapper.jsonl"
  fi
  python3 "$HARNESS_DIR/analyze.py" --out "$CASE_DIR/detect-$label-captures.json" \
    classify "$captures"
}

detect_case wrapper-direct wrapper-direct
detect_case wrapper-include wrapper-include
detect_case workspace-wrapper-direct workspace-wrapper-direct
detect_case workspace-wrapper-include workspace-wrapper-include
detect_case rustc-direct rustc-direct
detect_case rustc-include rustc-include

# --- 6. Ambient wrapper rejection in v0.0.2 ---------------------------------
ambient_case() {
  local label="$1" variable="$2"
  log "US-003 ambient wrapper rejection: $label"
  local project="$WORK/us003-ambient-$label"
  prepare_fixture include-loss "$project" required-include
  ( cd "$project" && env \
    "$variable=$BIN_DIR/general-wrapper" \
    "TEMPER_EXP_WRAPPER_LOG=$WORK/us003-ambient-$label.jsonl" \
    "TEMPER_EXP_WRAPPER_MODE=log" \
    "$TEMPER_BIN" temper optimize --manifest-path "$project/Cargo.toml" \
    -- "$WORKLOAD" ) >"$WORK/us003-ambient-$label.stdout" 2>"$WORK/us003-ambient-$label.stderr" || true
  cp "$(find "$project/.temper/runs" -name run.json | head -1)" \
    "$CASE_DIR/ambient-$label-run.json"
}

ambient_case general RUSTC_WRAPPER
ambient_case workspace RUSTC_WORKSPACE_WRAPPER

# --- 7. Pass-through overhead ------------------------------------------------
log "US-003 paired cold-build overhead"
python3 "$HARNESS_DIR/bench_overhead.py" \
  "$UNITS" "$BIN_DIR" "$WORK/us003-bench" "$REAL_RUSTC" "$TARGET_TRIPLE" \
  "$PACKAGE" "$BINARY" 12 >"$CASE_DIR/overhead.json"

python3 "$HARNESS_DIR/verify_us003.py" "$CASE_DIR" >"$CASE_DIR/verdict.json"
log "US-003 complete"
python3 -c "import json,sys; v=json.load(open(sys.argv[1])); print('passed', v['passed'], 'checks', v['checks_total'], 'failed', len(v['checks_failed']))" "$CASE_DIR/verdict.json"
