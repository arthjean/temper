#!/usr/bin/env python3
"""US-002 verdict over the retained classification, parity and process evidence."""

from __future__ import annotations

import json
import pathlib
import sys

EXPECTED_TARGET_CRATES = {"temper_units_app", "temper_units_lib"}
EXPECTED_HOST_CRATES = {"build_script_build", "temper_units_macro"}


def load(path: pathlib.Path):
    return json.loads(path.read_text())


def main() -> int:
    case_dir = pathlib.Path(sys.argv[1])
    checks: list[dict] = []

    def check(name: str, passed: bool, detail) -> None:
        checks.append({"check": name, "passed": bool(passed), "detail": detail})

    for stage in ("reference", "generate", "use"):
        classification = load(case_dir / f"{stage}-classification.json")
        detail = classification["detail"]
        target_crates = {item["crate"] for item in detail.get("target_compile", [])}
        host_crates = {item["crate"] for item in detail.get("host_compile", [])}
        probes = detail.get("probe", [])

        check(
            f"{stage}: Cargo target compilations are exactly the target dependency and the selected binary",
            target_crates == EXPECTED_TARGET_CRATES,
            sorted(target_crates),
        )
        check(
            f"{stage}: the build script and the proc macro are classified as host compilations",
            host_crates == EXPECTED_HOST_CRATES,
            sorted(host_crates),
        )
        check(
            f"{stage}: Cargo's initial rustc probes are observed",
            len(probes) >= 1 and all(item["print"] for item in probes),
            {"probes": len(probes)},
        )
        check(
            f"{stage}: at least one probe carries the supported target and is still not a compilation",
            any(item["exact_target"] and not item["injected"] for item in probes),
            [
                {"exact_target": item["exact_target"], "emit": item["emit"]}
                for item in probes
            ],
        )

        digests = load(case_dir / f"{stage}-digests.json")
        check(
            f"{stage}: an independent implementation reproduces every length-framed argv digest",
            digests["independent_digest_agreement"] is True,
            {"records": digests["records"], "mismatches": digests["mismatches"][:3]},
        )

        injected_by_class = classification["injected_by_class"]
        expected_injections = 0 if stage == "reference" else len(EXPECTED_TARGET_CRATES)
        check(
            f"{stage}: PGO controls reach only target compilations",
            injected_by_class.get("target_compile", 0) == expected_injections
            and injected_by_class.get("host_compile", 0) == 0
            and injected_by_class.get("probe", 0) == 0
            and injected_by_class.get("other", 0) == 0,
            injected_by_class,
        )

    parity = load(case_dir / "parity.json")
    check(
        "the normalization allowlist matches reference, generate and use compiler inputs",
        parity["matched"] is True,
        {"differences": parity["differences"][:5]},
    )
    check(
        "only the documented phase controls differ between the stages",
        parity["allowed_differences"]["reference"] == []
        and parity["allowed_differences"]["generate"] == ["profile_generate"]
        and parity["allowed_differences"]["use"]
        == ["missing_function_warning", "profile_use"],
        parity["allowed_differences"],
    )
    check(
        "every adversarial mutation class behaves as documented",
        parity["mutations_behaved_as_expected"] is True,
        {
            "rejected": parity["mutations_rejected"],
            "total": parity["mutations_total"],
            "unexpected": {
                name: outcome
                for name, outcome in parity["mutations"].items()
                if outcome["matched"] != outcome["expected_matched"]
            },
        },
    )
    check(
        "at least ten non-allowlisted difference classes are rejected",
        parity["mutations_rejected"] >= 10,
        {"rejected": parity["mutations_rejected"]},
    )

    identity = load(case_dir / "artifact-identity.json")
    check(
        "a pass-through build produces a byte-identical executable",
        identity["identical_executable"] is True,
        {
            "direct": identity["direct_sha256"],
            "pass_through": identity["pass_through_sha256"],
        },
    )
    check(
        "a pass-through build produces identical Cargo artifact identities",
        identity["identical_artifact_identity"] is True,
        {"direct": identity["direct_artifact_names"]},
    )

    process = load(case_dir / "process-probes.json")
    check(
        "every process-level probe passes",
        process["passed"] is True,
        {"failed": process["checks_failed"], "total": process["checks_total"]},
    )
    checks.extend(process["checks"])

    verdict = {
        "story": "US-002",
        "passed": all(item["passed"] for item in checks),
        "checks_total": len(checks),
        "checks_failed": [item for item in checks if not item["passed"]],
        "checks": checks,
    }
    print(json.dumps(verdict, indent=2, sort_keys=True))
    return 0 if verdict["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
