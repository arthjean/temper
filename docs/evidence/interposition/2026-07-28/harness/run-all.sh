#!/usr/bin/env bash
# Reruns every EP-001 experiment and regenerates the retained evidence.
#
#   TEMPER_EP001_WORK=/path/to/scratch docs/evidence/interposition/2026-07-28/harness/run-all.sh
#
# The scratch directory holds fixtures, target directories and raw captures.
# Only the analysed evidence is written back into the repository.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

bash "$HARNESS_DIR/us001.sh"
bash "$HARNESS_DIR/us002.sh"
bash "$HARNESS_DIR/us003.sh"
python3 "$HARNESS_DIR/summarize.py" "$RESULT_DIR" "$EVIDENCE_DIR/summary.json"
log "EP-001 evidence regenerated"
cat "$EVIDENCE_DIR/summary.json"
