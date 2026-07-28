#!/usr/bin/env bash
# EP-001 / US-002: validate the shim classification predicate, byte-transparent
# process replacement, pass-through artifact identity and the candidate parity
# normalization allowlist before any of it becomes production behaviour.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

CASE_DIR="$RESULT_DIR/us-002"
PROJECT="$WORK/us002-units"
PACKAGE="temper-units-app"
BINARY="temper-units-app"

mkdir -p "$CASE_DIR"

build_harness
REAL_RUSTC="$(real_rustc)"
LLVM_PROFDATA="$(dirname "$(rustc --print target-libdir)")/bin/llvm-profdata"

host_identity_json >"$CASE_DIR/identity.json"
prepare_fixture units "$PROJECT"

cargo_build() {
  local target_dir="$1" logs="$2"
  shift 2
  rm -rf -- "$target_dir"
  ( cd "$PROJECT" && env "$@" \
    cargo build --release --locked --target "$TARGET_TRIPLE" \
    --package "$PACKAGE" --bin "$BINARY" --target-dir "$target_dir" ) \
    >"$logs.stdout" 2>"$logs.stderr"
}

shim_build() {
  local stage="$1" inject="$2" target_dir="$3" captures="$4"
  fresh_capture "$captures"
  cargo_build "$target_dir" "$captures" \
    RUSTC="$BIN_DIR/shim" \
    TEMPER_EXP_REAL_RUSTC="$REAL_RUSTC" \
    TEMPER_EXP_CAPTURE_DIR="$captures" \
    TEMPER_EXP_STAGE="$stage" \
    TEMPER_EXP_TARGET="$TARGET_TRIPLE" \
    TEMPER_EXP_INJECT="$inject"
}

executable_of() {
  printf '%s\n' "$1/$TARGET_TRIPLE/release/$BINARY"
}

DIRECT_DIR="$WORK/us002-target-direct"
REFERENCE_DIR="$WORK/us002-target-reference"
GENERATE_DIR="$WORK/us002-target-generate"
USE_DIR="$WORK/us002-target-use"
PROFILE_RAW="$WORK/us002-profraw"
PROFILE_DATA="$WORK/us002-merged.profdata"

log "US-002 direct build without interposition"
cargo_build "$DIRECT_DIR" "$WORK/captures/us002-direct" RUSTC_BOOTSTRAP=

log "US-002 pass-through reference build"
shim_build reference none "$REFERENCE_DIR" "$WORK/captures/us002-reference"

log "US-002 instrumented build"
rm -rf -- "$PROFILE_RAW"
mkdir -p "$PROFILE_RAW"
shim_build generate "generate=$PROFILE_RAW" "$GENERATE_DIR" "$WORK/captures/us002-generate"
for _ in 1 2 3; do
  LLVM_PROFILE_FILE="$PROFILE_RAW/%p-%m.profraw" "$(executable_of "$GENERATE_DIR")" >/dev/null
done
"$LLVM_PROFDATA" merge --failure-mode=any -o "$PROFILE_DATA" "$PROFILE_RAW"/*.profraw

log "US-002 optimized build"
shim_build use "use=$PROFILE_DATA" "$USE_DIR" "$WORK/captures/us002-use"

for stage in reference generate use; do
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$stage-classification.json" \
    classify "$WORK/captures/us002-$stage"
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$stage-records.json" \
    records "$WORK/captures/us002-$stage"
  python3 "$HARNESS_DIR/analyze.py" \
    --out "$CASE_DIR/$stage-digests.json" \
    digests "$WORK/captures/us002-$stage"
done

log "US-002 parity normalization and adversarial mutations"
python3 "$HARNESS_DIR/analyze.py" \
  --out "$CASE_DIR/parity.json" \
  parity \
  "$WORK/captures/us002-reference" "$REFERENCE_DIR" \
  "$WORK/captures/us002-generate" "$GENERATE_DIR" \
  "$WORK/captures/us002-use" "$USE_DIR" \
  --mutate

log "US-002 pass-through artifact identity"
python3 - "$DIRECT_DIR" "$REFERENCE_DIR" "$TARGET_TRIPLE" "$BINARY" \
  >"$CASE_DIR/artifact-identity.json" <<'PYTHON'
import hashlib, json, pathlib, sys

direct, reference, triple, binary = (pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3], sys.argv[4])


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def artifact_names(root):
    return sorted(item.name for item in (root / triple / "release" / "deps").iterdir() if item.is_file())


direct_binary = direct / triple / "release" / binary
reference_binary = reference / triple / "release" / binary
print(json.dumps({
    "direct_sha256": digest(direct_binary),
    "pass_through_sha256": digest(reference_binary),
    "identical_executable": digest(direct_binary) == digest(reference_binary),
    "direct_artifact_names": artifact_names(direct),
    "pass_through_artifact_names": artifact_names(reference),
    "identical_artifact_identity": artifact_names(direct) == artifact_names(reference),
}, indent=2, sort_keys=True))
PYTHON

log "US-002 process-level probes"
set +e
python3 "$HARNESS_DIR/probe_process.py" "$BIN_DIR" "$WORK/us002-probe" \
  >"$CASE_DIR/process-probes.json"
set -e

python3 "$HARNESS_DIR/verify_us002.py" "$CASE_DIR" >"$CASE_DIR/verdict.json"
log "US-002 complete"
python3 -c "import json,sys; v=json.load(open(sys.argv[1])); print('passed', v['passed'], 'checks', v['checks_total'], 'failed', len(v['checks_failed']))" "$CASE_DIR/verdict.json"
