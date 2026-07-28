#!/usr/bin/env python3
"""US-003 paired cold-build overhead measurement.

Each pair rebuilds the fixture from an absent target directory twice: once with
the real compiler and once through the pass-through shim. Pairs alternate order
so a monotonic host drift cannot bias one arm. Raw monotonic timings are
retained; only the median paired ratio is summarised.

usage: bench_overhead.py <project> <bin-dir> <work> <real-rustc> <target> <package> <binary> <pairs>
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import statistics
import subprocess
import sys
import time


def build(project: pathlib.Path, target_dir: pathlib.Path, environment: dict, arguments: list[str]) -> int:
    if target_dir.exists():
        shutil.rmtree(target_dir)
    started = time.monotonic_ns()
    completed = subprocess.run(
        arguments + ["--target-dir", str(target_dir)],
        cwd=project,
        env=environment,
        capture_output=True,
    )
    elapsed = time.monotonic_ns() - started
    if completed.returncode != 0:
        raise SystemExit(completed.stderr.decode("utf-8", "backslashreplace"))
    return elapsed


def main() -> int:
    (
        project,
        bin_dir,
        work,
        real_rustc,
        triple,
        package,
        binary,
        pairs,
    ) = sys.argv[1:9]
    project = pathlib.Path(project)
    work = pathlib.Path(work)
    work.mkdir(parents=True, exist_ok=True)
    pairs = int(pairs)

    arguments = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "--target",
        triple,
        "--package",
        package,
        "--bin",
        binary,
    ]

    direct_environment = dict(os.environ)
    direct_environment.pop("RUSTC", None)

    captures = work / "bench-captures"
    shim_environment = dict(direct_environment)
    shim_environment.update(
        {
            "RUSTC": str(pathlib.Path(bin_dir) / "shim"),
            "TEMPER_EXP_REAL_RUSTC": real_rustc,
            "TEMPER_EXP_CAPTURE_DIR": str(captures),
            "TEMPER_EXP_STAGE": "benchmark",
            "TEMPER_EXP_TARGET": triple,
            "TEMPER_EXP_INJECT": "none",
        }
    )

    samples = []
    for index in range(pairs):
        if captures.exists():
            shutil.rmtree(captures)
        captures.mkdir(parents=True)
        direct_first = index % 2 == 0
        if direct_first:
            direct = build(project, work / "bench-direct", direct_environment, arguments)
            shim = build(project, work / "bench-shim", shim_environment, arguments)
        else:
            shim = build(project, work / "bench-shim", shim_environment, arguments)
            direct = build(project, work / "bench-direct", direct_environment, arguments)
        samples.append(
            {
                "pair": index,
                "direct_first": direct_first,
                "direct_ns": direct,
                "pass_through_ns": shim,
                "ratio": shim / direct,
                "shim_invocations": len(list(captures.glob("*.json"))),
            }
        )

    ratios = [sample["ratio"] for sample in samples]
    report = {
        "pairs": pairs,
        "median_ratio": statistics.median(ratios),
        "median_overhead_percent": (statistics.median(ratios) - 1.0) * 100.0,
        "max_ratio": max(ratios),
        "min_ratio": min(ratios),
        "median_direct_ms": statistics.median(
            sample["direct_ns"] for sample in samples
        )
        / 1e6,
        "median_pass_through_ms": statistics.median(
            sample["pass_through_ns"] for sample in samples
        )
        / 1e6,
        "samples": samples,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
