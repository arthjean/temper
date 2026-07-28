#!/usr/bin/env python3
"""US-001 verdict: turn the retained captures into explicit pass/fail checks."""

from __future__ import annotations

import json
import pathlib
import sys

INCLUDE_CASES = ("required-include", "nested-include", "optional-missing")
PRESERVING_STAGES = ("baseline", "thin-lto", "fat-lto-cgu1")
PGO_STAGES = ("pgo-instrumented", "pgo")


def load(path: pathlib.Path):
    return json.loads(path.read_text())


def stage(report: dict, name: str) -> dict:
    return report["stages"].get(name, {})


def target_sentinel(report: dict, name: str) -> tuple[int, int]:
    data = stage(report, name)
    return data.get("target_compiles", 0), data.get("target_compiles_with_sentinel", 0)


def main() -> int:
    case_dir = pathlib.Path(sys.argv[1])
    checks: list[dict] = []

    def check(name: str, passed: bool, detail) -> None:
        checks.append({"check": name, "passed": bool(passed), "detail": detail})

    for case in ("direct",) + INCLUDE_CASES:
        direct = load(case_dir / f"{case}-direct-sentinel.json")
        compiles, with_sentinel = target_sentinel(direct, "unattributed")
        check(
            f"{case}: direct cargo build carries the sentinel on every target compile",
            compiles > 0 and compiles == with_sentinel,
            {"target_compiles": compiles, "with_sentinel": with_sentinel},
        )
        host = stage(direct, "unattributed").get("host_compiles_with_sentinel", 0)
        check(
            f"{case}: direct cargo build never applies the sentinel to host units",
            host == 0,
            {"host_compiles_with_sentinel": host},
        )

        temper = load(case_dir / f"{case}-temper-sentinel.json")
        for name in PRESERVING_STAGES:
            compiles, with_sentinel = target_sentinel(temper, name)
            check(
                f"{case}: {name} preserves the sentinel",
                compiles > 0 and compiles == with_sentinel,
                {"target_compiles": compiles, "with_sentinel": with_sentinel},
            )

        expected_pgo_sentinel = case == "direct"
        for name in PGO_STAGES:
            compiles, with_sentinel = target_sentinel(temper, name)
            if expected_pgo_sentinel:
                passed = compiles > 0 and compiles == with_sentinel
            else:
                passed = compiles > 0 and with_sentinel == 0
            check(
                f"{case}: {name} sentinel presence is "
                + ("preserved" if expected_pgo_sentinel else "lost"),
                passed,
                {"target_compiles": compiles, "with_sentinel": with_sentinel},
            )

        run = load(case_dir / f"{case}-run.json")
        parity = run.get("pgo_training", {}).get("phase_parity", {})
        check(
            f"{case}: schema-2 phase parity reports no unexpected difference",
            parity.get("matched") is True and not parity.get("unexpected_differences"),
            {
                "matched": parity.get("matched"),
                "unexpected_differences": parity.get("unexpected_differences"),
                "schema_version": run.get("schema_version"),
            },
        )

    boundary = load(case_dir / "version-boundary-sentinel.json")
    compiles, with_sentinel = target_sentinel(boundary, "unattributed")
    boundary_log = (case_dir / "version-boundary.log").read_text()
    check(
        "cargo 1.93 silently ignores stable include instead of applying it",
        compiles > 0 and with_sentinel == 0,
        {"target_compiles": compiles, "with_sentinel": with_sentinel},
    )
    check(
        "cargo 1.93 does not fail the build, so the version boundary is not observable from its exit status",
        "exit_status: 0" in boundary_log,
        {"log_excerpt": boundary_log.strip().splitlines()[:3]},
    )

    for case, expectation in (
        ("missing-required", "could not load Cargo configuration"),
        ("cycle-include", "could not load Cargo configuration"),
    ):
        log = (case_dir / f"{case}.log").read_text()
        check(
            f"{case}: Cargo rejects the include graph",
            "exit_status: 0" not in log and expectation in log,
            {"log_excerpt": [line for line in log.splitlines() if "error" in line][:2]},
        )

    strict = load(case_dir / "strict-run.json")
    baseline_built = strict.get("baseline", {}).get("outcome")
    training = strict.get("pgo_training", {})
    instrumentation_failure = training.get("instrumentation_failure") or {}
    diagnostics = instrumentation_failure.get("bounded_diagnostics", "")
    check(
        "strict control: the baseline build succeeds with the included flag",
        baseline_built == "built",
        {"baseline_outcome": baseline_built},
    )
    check(
        "strict control: the PGO instrumentation build fails without the included flag",
        training.get("failure_stage") == "instrumentation"
        and instrumentation_failure.get("reason") == "cargo_build_failed",
        {
            "failure_stage": training.get("failure_stage"),
            "rejection_reason": training.get("rejection_reason"),
            "build_failure_reason": instrumentation_failure.get("reason"),
        },
    )
    check(
        "strict control: the failure names the lost included compiler input",
        "the included target rustflag did not reach this compilation" in diagnostics,
        {"diagnostics_excerpt": diagnostics[:200]},
    )

    verdict = {
        "story": "US-001",
        "passed": all(item["passed"] for item in checks),
        "checks_total": len(checks),
        "checks_failed": [item for item in checks if not item["passed"]],
        "checks": checks,
    }
    print(json.dumps(verdict, indent=2, sort_keys=True))
    return 0 if verdict["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
