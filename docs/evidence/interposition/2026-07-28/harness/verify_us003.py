#!/usr/bin/env python3
"""US-003 verdict over the wrapper, fingerprint and overhead evidence."""

from __future__ import annotations

import json
import pathlib
import sys

TARGET_CRATES = {"temper_units_app", "temper_units_lib"}


def load(path: pathlib.Path):
    return json.loads(path.read_text())


def load_lines(path: pathlib.Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def pgo_rejection(run: dict) -> tuple[str | None, str | None]:
    training = run.get("pgo_training") or {}
    return training.get("outcome"), training.get("rejection_reason")


def static_candidates_survive(run: dict) -> bool:
    strategies = {item["identity"]: item for item in run.get("strategies", [])}
    return all(
        strategies.get(name, {}).get("build", {}).get("outcome") == "built"
        for name in ("thin-lto", "fat-lto-cgu1")
    )


def main() -> int:
    case_dir = pathlib.Path(sys.argv[1])
    checks: list[dict] = []

    def check(name: str, passed: bool, detail) -> None:
        checks.append({"check": name, "passed": bool(passed), "detail": detail})

    # 1. Documented wrapper chain order across the four wrapper fixtures.
    modes = {
        mode: load_lines(case_dir / f"wrapper-{mode}.jsonl")
        for mode in ("none", "general", "workspace", "nested")
    }

    def labelled(mode: str, label: str) -> list[dict]:
        return [item for item in modes[mode] if item["label"] == label]

    check(
        "a wrapper-free build invokes no wrapper at all",
        modes["none"] == [],
        {"records": len(modes["none"])},
    )
    check(
        "a general wrapper alone wraps the shim directly",
        bool(labelled("general", "general-wrapper"))
        and all(
            item["next_program"].endswith("shim")
            for item in labelled("general", "general-wrapper")
        )
        and not labelled("general", "workspace-wrapper"),
        {
            "next": sorted(
                {item["next_program"] for item in labelled("general", "general-wrapper")}
            )
        },
    )
    check(
        "a workspace wrapper alone wraps the shim directly",
        bool(labelled("workspace", "workspace-wrapper"))
        and all(
            item["next_program"].endswith("shim")
            for item in labelled("workspace", "workspace-wrapper")
        )
        and not labelled("workspace", "general-wrapper"),
        {
            "next": sorted(
                {
                    item["next_program"]
                    for item in labelled("workspace", "workspace-wrapper")
                }
            )
        },
    )
    general = labelled("nested", "general-wrapper")
    workspace = labelled("nested", "workspace-wrapper")
    check(
        "nested wrappers execute as $RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC",
        bool(general)
        and bool(workspace)
        and all(item["next_program"].endswith("workspace-wrapper") for item in general)
        and all(item["next_program"].endswith("shim") for item in workspace),
        {
            "general_next": sorted({item["next_program"] for item in general}),
            "workspace_next": sorted({item["next_program"] for item in workspace}),
        },
    )
    check(
        "no wrapper in any fixture ever sees a shim-appended PGO control",
        all(
            not any(
                "-Cprofile-generate" in argument or "-Cprofile-use" in argument
                for argument in item["received_argv"]
            )
            for records in modes.values()
            for item in records
        ),
        {mode: len(records) for mode, records in modes.items()},
    )
    check(
        "both nested wrappers see the same unit set, including Cargo's probes",
        {item["crate_name"] for item in general if item["crate_name"]}
        == {item["crate_name"] for item in workspace if item["crate_name"]}
        == TARGET_CRATES | {"build_script_build", "temper_units_macro", "___"},
        sorted({item["crate_name"] for item in workspace if item["crate_name"]}),
    )

    # 2. Outer cache key across PGO phases.
    cache = load_lines(case_dir / "cache-keys.jsonl")
    keys: dict[str, dict[str, str]] = {}
    for item in cache:
        if item["crate_name"] in TARGET_CRATES:
            keys.setdefault(item["crate_name"], {})[item["outcome"]] = item["cache_key"]
    shared = {
        name: len({key for key in outcomes.values()}) == 1
        for name, outcomes in keys.items()
    }
    check(
        "reference, generate and use produce one shared outer cache key per target crate",
        bool(shared) and all(shared.values()),
        {
            name: {"outcomes": sorted(outcomes), "distinct_keys": len(set(outcomes.values()))}
            for name, outcomes in keys.items()
        },
    )
    hits = [item for item in cache if item["outcome"] == "cache_hit"]
    check(
        "the instrumented and optimized phases are served from the reference cache entry",
        len(hits) >= 2 * len(TARGET_CRATES),
        {"cache_hits": len(hits)},
    )
    digests = {
        stage: (case_dir / f"cache-{stage}.sha256").read_text().strip()
        for stage in ("reference", "generate", "use")
    }
    check(
        "a cached PGO phase silently returns the reference executable",
        len(set(digests.values())) == 1,
        digests,
    )
    cached_profraw = int((case_dir / "cache-generate-profraw-count.txt").read_text())
    uncached_profraw = int((case_dir / "uncached-generate-profraw-count.txt").read_text())
    check(
        "the cached instrumented executable emits no profile data while an uncached one does",
        cached_profraw == 0 and uncached_profraw > 0,
        {"cached_profraw": cached_profraw, "uncached_profraw": uncached_profraw},
    )

    # 3. Cargo fingerprints ignore shim-appended flags.
    reuse = load(case_dir / "target-directory-reuse.json")
    check(
        "reusing a stage target directory hides the next PGO phase from Cargo entirely",
        reuse["first_invocations"] > 0
        and reuse["second_invocations"] == 0
        and reuse["artifact_unchanged"] is True,
        reuse,
    )
    check(
        "a symlinked alias of a stage target directory is equally invisible",
        reuse["alias_invocations"] == 0,
        {"alias_invocations": reuse["alias_invocations"], "alias_sha256": reuse["alias_sha256"]},
    )
    check(
        "Cargo accepts a precreated stage directory without any warning, so Temper must reject it itself",
        reuse["precreated_invocations"] > 0,
        {"precreated_invocations": reuse["precreated_invocations"]},
    )
    fresh = load(case_dir / "fresh-stage-directories.json")
    check(
        "a fresh wrapper-free per-stage target directory recompiles every target unit",
        all(
            item["target_compiles"] == len(TARGET_CRATES)
            for item in fresh["wrapper_free"].values()
        ),
        fresh["wrapper_free"],
    )
    cached_phases = [
        item
        for name, item in fresh["cached"].items()
        if name.endswith("generate") or name.endswith("use")
    ]
    check(
        "an outer cache removes every instrumented and optimized compilation from the shim",
        bool(cached_phases)
        and all(
            item["target_compiles"] == 0 and item["host_compiles"] == 0
            for item in cached_phases
        ),
        fresh["cached"],
    )

    # 4. v0.0.2 wrapper and compiler-override detection.
    for label, expectation in (
        ("wrapper-direct", True),
        ("wrapper-include", False),
        ("workspace-wrapper-direct", True),
        ("workspace-wrapper-include", False),
        ("rustc-direct", True),
        ("rustc-include", False),
    ):
        run = load(case_dir / f"detect-{label}-run.json")
        outcome, reason = pgo_rejection(run)
        detected = reason == "unproven_compiler_override"
        check(
            f"v0.0.2 {'detects' if expectation else 'does not detect'} the {label} compiler override",
            detected == expectation,
            {"outcome": outcome, "rejection_reason": reason},
        )
        if not expectation:
            wrapper_records = load_lines(case_dir / f"detect-{label}-wrapper.jsonl")
            captures = load(case_dir / f"detect-{label}-captures.json")
            active = (
                captures["counts"].get("target_compile", 0) > 0
                if label.startswith("rustc")
                else len(wrapper_records) > 0
            )
            check(
                f"{label}: the undetected override really compiled the run",
                active and outcome == "trained",
                {
                    "wrapper_records": len(wrapper_records),
                    "configured_compiler_invocations": captures["records"],
                    "target_compiles": captures["counts"].get("target_compile", 0),
                },
            )

    for label in ("general", "workspace"):
        ambient = load(case_dir / f"ambient-{label}-run.json")
        outcome, reason = pgo_rejection(ambient)
        check(
            f"an ambient {label} wrapper rejects only PGO and keeps static candidates eligible",
            reason == "ambient_compiler_override" and static_candidates_survive(ambient),
            {
                "rejection_reason": reason,
                "static_candidates_built": static_candidates_survive(ambient),
            },
        )

    # 5. Pass-through overhead budget.
    overhead = load(case_dir / "overhead.json")
    check(
        "at least ten paired cold builds were measured",
        overhead["pairs"] >= 10,
        {"pairs": overhead["pairs"]},
    )
    check(
        "median pass-through shim overhead is at or below 5%",
        overhead["median_overhead_percent"] <= 5.0,
        {
            "median_overhead_percent": round(overhead["median_overhead_percent"], 3),
            "median_direct_ms": round(overhead["median_direct_ms"], 1),
            "median_pass_through_ms": round(overhead["median_pass_through_ms"], 1),
        },
    )
    check(
        "every measured pass-through build actually went through the shim",
        all(sample["shim_invocations"] > 0 for sample in overhead["samples"]),
        {"min_invocations": min(sample["shim_invocations"] for sample in overhead["samples"])},
    )

    verdict = {
        "story": "US-003",
        "passed": all(item["passed"] for item in checks),
        "checks_total": len(checks),
        "checks_failed": [item for item in checks if not item["passed"]],
        "checks": checks,
    }
    print(json.dumps(verdict, indent=2, sort_keys=True))
    return 0 if verdict["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
