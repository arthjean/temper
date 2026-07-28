#!/usr/bin/env python3
"""Aggregate the EP-001 story verdicts into one retained summary.

usage: summarize.py <results-dir> <output>
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import sys

STORIES = {
    "us-001": "Reproduce the stable Cargo include loss",
    "us-002": "Validate shim classification, transparency and parity inputs",
    "us-003": "Validate the wrapper-cache boundary, fingerprints and overhead",
}


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    results = pathlib.Path(sys.argv[1])
    output = pathlib.Path(sys.argv[2])
    harness = pathlib.Path(__file__).parent

    stories = {}
    for story, title in STORIES.items():
        verdict = json.loads((results / story / "verdict.json").read_text())
        stories[story] = {
            "title": title,
            "passed": verdict["passed"],
            "checks_total": verdict["checks_total"],
            "checks_failed": len(verdict["checks_failed"]),
        }

    overhead = json.loads((results / "us-003" / "overhead.json").read_text())
    identity = json.loads((results / "us-001" / "identity.json").read_text())
    parity = json.loads((results / "us-002" / "parity.json").read_text())
    artifact = json.loads((results / "us-002" / "artifact-identity.json").read_text())

    summary = {
        "epic": "EP-001",
        "prd": "tasks/prd-temper-v0.0.3.md",
        "date": "2026-07-28",
        "workload_class": "synthetic",
        "claims": {
            "optimization_gain": None,
            "note": "This directory retains correctness and cost evidence only. It makes no runtime optimization claim.",
        },
        "identity": identity,
        "stories": stories,
        "passed": all(story["passed"] for story in stories.values()),
        "headline": {
            "pass_through_executable_identical": artifact["identical_executable"],
            "parity_matched": parity["matched"],
            "parity_mutations_rejected": parity["mutations_rejected"],
            "parity_mutations_total": parity["mutations_total"],
            "median_overhead_percent": round(overhead["median_overhead_percent"], 3),
            "overhead_pairs": overhead["pairs"],
        },
        "harness_sha256": {
            path.name: digest(path)
            for path in sorted(harness.iterdir())
            if path.is_file()
        },
    }
    output.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(json.dumps(summary["headline"], indent=2, sort_keys=True))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
