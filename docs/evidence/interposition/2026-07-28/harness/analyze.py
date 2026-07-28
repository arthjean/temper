#!/usr/bin/env python3
"""Analysis for the Temper v0.0.3 EP-001 compiler-interposition experiments.

The shim writes one JSON record per rustc invocation. This program aggregates
those records, classifies them, and implements the candidate schema-3 parity
normalization so the allowlist can be adversarially mutated before any
production code exists. Python 3 standard library only.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

STAGE_PRECEDENCE = (
    "confirmation",
    "pgo-instrumented",
    "pgo",
    "fat-lto-cgu1",
    "thin-lto",
    "baseline",
)

PGO_GENERATE_PREFIX = b"-Cprofile-generate="
PGO_USE_PREFIX = b"-Cprofile-use="
PGO_MISSING_FUNCTION = b"-Cllvm-args=-pgo-warn-missing-function"


def load(directory: str) -> list[dict]:
    records = []
    for path in sorted(pathlib.Path(directory).glob("*.json")):
        record = json.loads(path.read_text())
        record["_file"] = path.name
        record["_argv"] = [bytes.fromhex(item) for item in record["argv_hex"]]
        record["_temper_stage"] = temper_stage(record.get("out_dir"))
        records.append(record)
    return records


def temper_stage(out_dir: str | None) -> str:
    if not out_dir:
        return "unattributed"
    parts = out_dir.split("/")
    for name in STAGE_PRECEDENCE:
        if name in parts:
            return name
    return "unattributed"


def final_argv(record: dict) -> list[bytes]:
    return record["_argv"] + [item.encode() for item in record["injected"]]


def framed(arguments: list[bytes]) -> bytes:
    payload = len(arguments).to_bytes(8, "big")
    for argument in arguments:
        payload += len(argument).to_bytes(8, "big") + argument
    return payload


def command_digests(args) -> None:
    import hashlib

    mismatches = []
    records = load(args.captures)
    for record in records:
        expected_pre = hashlib.sha256(framed(record["_argv"])).hexdigest()
        expected_post = hashlib.sha256(framed(final_argv(record))).hexdigest()
        if expected_pre != record["pre_digest"] or expected_post != record["post_digest"]:
            mismatches.append(
                {
                    "capture_file": record["_file"],
                    "recorded_pre": record["pre_digest"],
                    "recomputed_pre": expected_pre,
                    "recorded_post": record["post_digest"],
                    "recomputed_post": expected_post,
                }
            )
    emit(
        {
            "records": len(records),
            "mismatches": mismatches,
            "independent_digest_agreement": not mismatches,
            "injected_records": sum(1 for record in records if record["injected"]),
        },
        args,
    )


def contains(record: dict, needle: str) -> bool:
    needle_bytes = needle.encode()
    return any(needle_bytes in argument for argument in record["_argv"])


def command_sentinel(args) -> None:
    records = load(args.captures)
    stages: dict[str, dict] = {}
    for record in records:
        stage = stages.setdefault(
            record["_temper_stage"],
            {
                "target_compiles": 0,
                "target_compiles_with_sentinel": 0,
                "host_compiles": 0,
                "host_compiles_with_sentinel": 0,
                "probes": 0,
                "other": 0,
                "target_crates": [],
                "host_crates": [],
            },
        )
        present = contains(record, args.sentinel)
        if record["class"] == "target_compile":
            stage["target_compiles"] += 1
            stage["target_compiles_with_sentinel"] += int(present)
            stage["target_crates"].append(
                {"crate": record["crate_name"], "sentinel": present}
            )
        elif record["class"] == "host_compile":
            stage["host_compiles"] += 1
            stage["host_compiles_with_sentinel"] += int(present)
            stage["host_crates"].append(
                {"crate": record["crate_name"], "sentinel": present}
            )
        elif record["class"] == "probe":
            stage["probes"] += 1
        else:
            stage["other"] += 1
    emit({"sentinel": args.sentinel, "records": len(records), "stages": stages}, args)


def command_classify(args) -> None:
    records = load(args.captures)
    classes: dict[str, list] = {}
    for record in records:
        classes.setdefault(record["class"], []).append(
            {
                "crate": record["crate_name"],
                "crate_types": record["crate_types"],
                "exact_target": record["exact_target"],
                "emit": record["emit"],
                "print": record["print"],
                "injected": record["injected"],
                "stage": record["_temper_stage"],
                "argv_length": len(record["_argv"]),
            }
        )
    summary = {
        "records": len(records),
        "counts": {name: len(items) for name, items in sorted(classes.items())},
        "injected_records": sum(1 for record in records if record["injected"]),
        "injected_by_class": {
            name: sum(1 for item in items if item["injected"])
            for name, items in sorted(classes.items())
        },
        "detail": {name: items for name, items in sorted(classes.items())},
    }
    emit(summary, args)


# --- parity -----------------------------------------------------------------


def normalize_argument(argument: bytes, root: bytes) -> bytes:
    return argument.replace(root, b"<STAGE_ROOT>") if root else argument


def normalized_record(record: dict, root: bytes) -> dict:
    arguments = []
    allowed = []
    # Parity compares the compiler inputs rustc actually received, so the
    # observed argv is the Cargo-resolved argv plus whatever the shim appended.
    for argument in final_argv(record):
        if argument.startswith(PGO_GENERATE_PREFIX):
            allowed.append("profile_generate")
            continue
        if argument.startswith(PGO_USE_PREFIX):
            allowed.append("profile_use")
            continue
        if argument == PGO_MISSING_FUNCTION:
            allowed.append("missing_function_warning")
            continue
        arguments.append(normalize_argument(argument, root))
    return {
        "key": record["crate_name"] or "<unnamed>",
        "crate_types": tuple(record["crate_types"]),
        "metadata": record["metadata"],
        "extra_filename": record["extra_filename"],
        "tool": record["real_rustc"],
        "arguments": arguments,
        "allowed": allowed,
    }


def normalized_set(directory: str, root: str) -> list[dict]:
    records = [
        record for record in load(directory) if record["class"] == "target_compile"
    ]
    return [normalized_record(record, root.encode()) for record in records]


def display(argument: bytes) -> str:
    return argument.decode("utf-8", "backslashreplace")


def root_dependent(items: list[dict]) -> list[str]:
    """Enumerate which argv positions depend on the stage target root."""
    classes = set()
    for item in items:
        previous = b""
        for argument in item["arguments"]:
            if b"<STAGE_ROOT>" in argument:
                prefix = argument.split(b"<STAGE_ROOT>", 1)[0]
                classes.add(f"{display(previous)} {display(prefix)}<STAGE_ROOT>…".strip())
            previous = argument
    return sorted(classes)


COMPARED_FIELDS = (
    ("crate_types", "crate_kind_changed"),
    ("metadata", "artifact_metadata_changed"),
    ("extra_filename", "artifact_extra_filename_changed"),
    ("tool", "tool_changed"),
)


def index_by_key(items: list[dict], label: str, differences: list[dict]) -> dict:
    indexed: dict[str, dict] = {}
    for item in items:
        if item["key"] in indexed:
            differences.append(
                {
                    "class": "ambiguous_crate_identity",
                    "phase": label,
                    "detail": item["key"],
                }
            )
        indexed[item["key"]] = item
    return indexed


def compare(reference: list[dict], other: list[dict], label: str) -> list[dict]:
    differences: list[dict] = []
    by_key_reference = index_by_key(reference, "reference", differences)
    by_key_other = index_by_key(other, label, differences)
    reference_keys = sorted(by_key_reference)
    other_keys = sorted(by_key_other)
    if reference_keys != other_keys:
        if len(reference_keys) != len(other_keys):
            differences.append(
                {
                    "class": "crate_count",
                    "phase": label,
                    "detail": f"{len(reference_keys)} vs {len(other_keys)}",
                }
            )
        for key in [key for key in reference_keys if key not in by_key_other]:
            differences.append(
                {"class": "crate_missing", "phase": label, "detail": str(key)}
            )
        for key in [key for key in other_keys if key not in by_key_reference]:
            differences.append(
                {"class": "crate_added", "phase": label, "detail": str(key)}
            )
        return differences

    for key in reference_keys:
        left = by_key_reference[key]
        right = by_key_other[key]
        for field, difference_class in COMPARED_FIELDS:
            if left[field] != right[field]:
                differences.append(
                    {"class": difference_class, "phase": label, "detail": str(key)}
                )
        if left["arguments"] == right["arguments"]:
            continue
        if sorted(left["arguments"]) == sorted(right["arguments"]):
            differences.append(
                {"class": "argument_order", "phase": label, "detail": str(key)}
            )
            continue
        if len(left["arguments"]) != len(right["arguments"]):
            differences.append(
                {
                    "class": "argument_count",
                    "phase": label,
                    "detail": f"{key}: {len(left['arguments'])} vs {len(right['arguments'])}",
                }
            )
        removed = [item for item in left["arguments"] if item not in right["arguments"]]
        added = [item for item in right["arguments"] if item not in left["arguments"]]
        for item in removed:
            differences.append(
                {
                    "class": "argument_removed",
                    "phase": label,
                    "detail": f"{key[0]}: {display(item)}",
                }
            )
        for item in added:
            differences.append(
                {
                    "class": "argument_added",
                    "phase": label,
                    "detail": f"{key[0]}: {display(item)}",
                }
            )
    return differences


def change_one_argument(arguments: list[bytes]) -> list[bytes]:
    if b"opt-level=3" in arguments:
        return replace_value(arguments, b"opt-level=3", b"opt-level=2")
    return arguments[:-1] + [arguments[-1] + b"-temper-mutated"]


MUTATIONS = {
    "argument_added": lambda items: mutate_arguments(
        items, lambda arguments: arguments + [b"--cfg=temper_mutation"]
    ),
    "argument_removed": lambda items: mutate_arguments(
        items, lambda arguments: arguments[:-1]
    ),
    "argument_changed": lambda items: mutate_arguments(items, change_one_argument),
    "argument_order": lambda items: mutate_arguments(
        items, lambda arguments: arguments[:1] + arguments[1:][::-1]
    ),
    "escaped_stage_root_path": lambda items: mutate_arguments(
        items,
        lambda arguments: [
            argument.replace(b"<STAGE_ROOT>", b"/elsewhere") for argument in arguments
        ],
    ),
    "crate_added": lambda items: items + [duplicate(items[0], "temper_mutation_crate")],
    "crate_removed": lambda items: items[1:],
    "crate_name_changed": lambda items: [duplicate(items[0], "temper_renamed")]
    + items[1:],
    "crate_kind_changed": lambda items: mutate_field(items, "crate_types", ("lib",)),
    "artifact_metadata_changed": lambda items: mutate_field(
        items, "metadata", "deadbeefdeadbeef"
    ),
    "artifact_extra_filename_changed": lambda items: mutate_field(
        items, "extra_filename", "-temper-mutation"
    ),
    "tool_changed": lambda items: mutate_field(items, "tool", "/usr/bin/other-rustc"),
    "record_order": lambda items: items[::-1],
}

# `record_order` must remain matched: the comparison is keyed by logical crate
# identity, not by capture order.
EXPECTED_MATCHED = {"record_order"}


def replace_value(arguments: list[bytes], old: bytes, new: bytes) -> list[bytes]:
    return [new if argument == old else argument for argument in arguments]


def mutate_arguments(items: list[dict], transform) -> list[dict]:
    mutated = [dict(item) for item in items]
    mutated[0]["arguments"] = transform(list(mutated[0]["arguments"]))
    return mutated


def mutate_field(items: list[dict], field: str, value) -> list[dict]:
    mutated = [dict(item) for item in items]
    mutated[0][field] = value
    return mutated


def duplicate(item: dict, crate_name: str) -> dict:
    clone = dict(item)
    clone["key"] = crate_name
    return clone


def command_parity(args) -> None:
    reference = normalized_set(args.reference_captures, args.reference_root)
    generate = normalized_set(args.generate_captures, args.generate_root)
    use = normalized_set(args.use_captures, args.use_root)

    if not reference or not generate or not use:
        emit(
            {
                "matched": False,
                "reason": "incomplete_compiler_evidence",
                "counts": {
                    "reference": len(reference),
                    "generate": len(generate),
                    "use": len(use),
                },
                "differences": [],
            },
            args,
        )
        return

    result = {
        "counts": {
            "reference": len(reference),
            "generate": len(generate),
            "use": len(use),
        },
        "stage_root_dependent_arguments": {
            "reference": root_dependent(reference),
            "generate": root_dependent(generate),
            "use": root_dependent(use),
        },
        "allowed_differences": {
            "reference": sorted({name for item in reference for name in item["allowed"]}),
            "generate": sorted({name for item in generate for name in item["allowed"]}),
            "use": sorted({name for item in use for name in item["allowed"]}),
        },
        "mutations": {},
    }

    differences = compare(reference, generate, "generate") + compare(
        reference, use, "use"
    )
    result["matched"] = not differences
    result["differences"] = differences

    if args.mutate:
        for name, mutation in MUTATIONS.items():
            mutated = mutation(use)
            mutated_differences = compare(reference, mutated, "use")
            result["mutations"][name] = {
                "matched": not mutated_differences,
                "expected_matched": name in EXPECTED_MATCHED,
                "difference_classes": sorted(
                    {item["class"] for item in mutated_differences}
                ),
            }
        result["mutations_rejected"] = sum(
            1 for outcome in result["mutations"].values() if not outcome["matched"]
        )
        result["mutations_total"] = len(result["mutations"])
        result["mutations_behaved_as_expected"] = all(
            outcome["matched"] == outcome["expected_matched"]
            for outcome in result["mutations"].values()
        )

    emit(result, args)


def command_records(args) -> None:
    records = load(args.captures)
    output = []
    for record in records:
        published = {
            key: value
            for key, value in record.items()
            if key not in {"_argv", "_file", "argv_hex"}
        }
        published["temper_stage"] = record["_temper_stage"]
        published["capture_file"] = record["_file"]
        output.append(published)
    output.sort(
        key=lambda item: (
            item["temper_stage"],
            item["class"],
            item["crate_name"] or "",
            item["pre_digest"],
        )
    )
    emit(output, args)


def emit(payload, args) -> None:
    text = json.dumps(payload, indent=2, sort_keys=True, default=str)
    if args.out:
        pathlib.Path(args.out).write_text(text + "\n")
    else:
        print(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out")
    subparsers = parser.add_subparsers(dest="command", required=True)

    sentinel = subparsers.add_parser("sentinel")
    sentinel.add_argument("captures")
    sentinel.add_argument("sentinel")
    sentinel.set_defaults(handler=command_sentinel)

    classify = subparsers.add_parser("classify")
    classify.add_argument("captures")
    classify.set_defaults(handler=command_classify)

    records = subparsers.add_parser("records")
    records.add_argument("captures")
    records.set_defaults(handler=command_records)

    digests = subparsers.add_parser("digests")
    digests.add_argument("captures")
    digests.set_defaults(handler=command_digests)

    parity = subparsers.add_parser("parity")
    parity.add_argument("reference_captures")
    parity.add_argument("reference_root")
    parity.add_argument("generate_captures")
    parity.add_argument("generate_root")
    parity.add_argument("use_captures")
    parity.add_argument("use_root")
    parity.add_argument("--mutate", action="store_true")
    parity.set_defaults(handler=command_parity)

    args = parser.parse_args()
    args.handler(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
