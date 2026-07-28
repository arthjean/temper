#!/usr/bin/env python3
"""US-002 process-level probes for the candidate compiler shim.

Every probe drives the shim directly with a deterministic stand-in compiler so
argument bytes, standard streams, exit status, signal termination and protocol
failure can be observed without depending on rustc behaviour.

usage: probe_process.py <bin-dir> <work-dir>
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import struct
import subprocess
import sys

TARGET = "x86_64-unknown-linux-gnu"
NON_UTF8 = b"--cfg=temper_\xff\xfe_sentinel"


def framed(arguments: list[bytes]) -> bytes:
    payload = struct.pack(">Q", len(arguments))
    for argument in arguments:
        payload += struct.pack(">Q", len(argument)) + argument
    return payload


class Probes:
    def __init__(self, bin_dir: pathlib.Path, work: pathlib.Path) -> None:
        self.shim = bin_dir / "shim"
        self.fake = bin_dir / "fake-rustc"
        self.work = work
        self.checks: list[dict] = []
        self.canary = work / "canary"
        self.canary.mkdir(parents=True, exist_ok=True)
        self.canary_marker = work / "canary-executed"
        canary_rustc = self.canary / "rustc"
        canary_rustc.write_text(
            f"#!/bin/sh\ntouch {self.canary_marker}\nexit 0\n"
        )
        canary_rustc.chmod(0o755)

    def check(self, name: str, passed: bool, detail) -> None:
        self.checks.append({"check": name, "passed": bool(passed), "detail": detail})

    def capture_dir(self, name: str) -> pathlib.Path:
        directory = self.work / "captures" / name
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
        return directory

    def environment(self, captures: pathlib.Path | None, **overrides) -> dict:
        environment = dict(os.environ)
        environment["PATH"] = f"{self.canary}:{environment['PATH']}"
        environment["TEMPER_EXP_REAL_RUSTC"] = str(self.fake)
        environment["TEMPER_EXP_TARGET"] = TARGET
        environment["TEMPER_EXP_STAGE"] = "probe"
        environment["TEMPER_EXP_INJECT"] = "none"
        if captures is not None:
            environment["TEMPER_EXP_CAPTURE_DIR"] = str(captures)
        for key, value in overrides.items():
            if value is None:
                environment.pop(key, None)
            else:
                environment[key] = value
        return environment

    def records(self, captures: pathlib.Path) -> list[dict]:
        return [json.loads(path.read_text()) for path in sorted(captures.glob("*.json"))]

    # -- probes ------------------------------------------------------------

    def argument_bytes_survive(self) -> None:
        captures = self.capture_dir("bytes")
        argv_out = self.work / "fake-argv.bin"
        arguments = [
            b"--crate-name",
            b"temper_units_app",
            b"--target",
            TARGET.encode(),
            b"--emit=dep-info,link",
            NON_UTF8,
        ]
        stdin = b"stdin-bytes-\x00\xff-end"
        completed = subprocess.run(
            [bytes(self.shim)] + arguments,
            env=self.environment(
                captures,
                TEMPER_EXP_FAKE_ARGV_OUT=str(argv_out),
                TEMPER_EXP_FAKE_STDOUT="fake-stdout-prefix:",
            ),
            input=stdin,
            capture_output=True,
        )
        received = argv_out.read_bytes()
        self.check(
            "shim forwards non-UTF-8 argument bytes without conversion",
            received == framed(arguments),
            {
                "expected_sha_len": len(framed(arguments)),
                "received_len": len(received),
                "equal": received == framed(arguments),
            },
        )
        self.check(
            "shim preserves stdin, stdout and stderr across process replacement",
            completed.stdout == b"fake-stdout-prefix:" + stdin
            and b"fake-rustc-stderr-marker" in completed.stderr,
            {
                "stdout": completed.stdout.decode("utf-8", "backslashreplace"),
                "stderr": completed.stderr.decode("utf-8", "backslashreplace").strip(),
            },
        )
        record = self.records(captures)[0]
        self.check(
            "a non-UTF-8 argument is still digested and recorded",
            len(record["argv_hex"]) == len(arguments)
            and bytes.fromhex(record["argv_hex"][-1]) == NON_UTF8,
            {"argv_display_tail": record["argv_display"][-1]},
        )

    def exit_semantics(self) -> None:
        for mode, expected in (("ok", 0), ("exit42", 42), ("abort", -6)):
            captures = self.capture_dir(f"exit-{mode}")
            direct = subprocess.run(
                [str(self.fake), "--emit=link"],
                env=self.environment(None, TEMPER_EXP_FAKE_MODE=mode),
                input=b"",
                capture_output=True,
            )
            through = subprocess.run(
                [str(self.shim), "--emit=link"],
                env=self.environment(captures, TEMPER_EXP_FAKE_MODE=mode),
                input=b"",
                capture_output=True,
            )
            self.check(
                f"process replacement preserves the `{mode}` termination status",
                direct.returncode == expected and through.returncode == expected,
                {"direct": direct.returncode, "through_shim": through.returncode},
            )

    def protocol_failures(self) -> None:
        cases = {
            "missing_real_rustc": {"TEMPER_EXP_REAL_RUSTC": None},
            "empty_real_rustc": {"TEMPER_EXP_REAL_RUSTC": ""},
            "relative_real_rustc": {"TEMPER_EXP_REAL_RUSTC": "rustc"},
            "absent_real_rustc": {"TEMPER_EXP_REAL_RUSTC": "/nonexistent/rustc"},
            "missing_capture_dir": {"TEMPER_EXP_CAPTURE_DIR": None},
            "absent_capture_dir": {
                "TEMPER_EXP_CAPTURE_DIR": str(self.work / "absent-capture-dir")
            },
            "missing_stage": {"TEMPER_EXP_STAGE": None},
            "missing_target": {"TEMPER_EXP_TARGET": None},
            "malformed_injection": {"TEMPER_EXP_INJECT": "generate"},
            "relative_injection": {"TEMPER_EXP_INJECT": "generate=relative.profraw"},
        }
        for name, overrides in cases.items():
            captures = self.capture_dir(f"protocol-{name}")
            if self.canary_marker.exists():
                self.canary_marker.unlink()
            completed = subprocess.run(
                [str(self.shim), "--emit=link", "--target", TARGET],
                env=self.environment(captures, **overrides),
                input=b"",
                capture_output=True,
            )
            self.check(
                f"protocol failure `{name}` fails closed without a fallback compiler",
                completed.returncode == 97
                and not self.canary_marker.exists()
                and b"protocol failure" in completed.stderr,
                {
                    "exit": completed.returncode,
                    "stderr": completed.stderr.decode("utf-8", "backslashreplace").strip(),
                    "canary_executed": self.canary_marker.exists(),
                },
            )

    def classification_predicate(self) -> None:
        profile = self.work / "profiles"
        profile.mkdir(parents=True, exist_ok=True)
        cases = {
            "target_compile": (
                [b"--crate-name", b"app", b"--target", TARGET.encode(), b"--emit=link"],
                True,
            ),
            "target_compile_equals_form": (
                [b"--crate-name", b"app", f"--target={TARGET}".encode(), b"--emit=link"],
                True,
            ),
            "print_probe_with_target": (
                [b"--print", b"target-libdir", b"--target", TARGET.encode()],
                False,
            ),
            "print_probe_crate_name": (
                [b"--print", b"crate-name", b"--target", TARGET.encode(), b"--emit=link"],
                False,
            ),
            "host_compile": ([b"--crate-name", b"build_script_build", b"--emit=link"], False),
            "other_target_triple": (
                [
                    b"--crate-name",
                    b"app",
                    b"--target",
                    b"aarch64-unknown-linux-gnu",
                    b"--emit=link",
                ],
                False,
            ),
            "host_triple_only_in_a_path": (
                [
                    b"--crate-name",
                    b"build_script_build",
                    b"--emit=link",
                    b"--out-dir",
                    f"/tmp/{TARGET}/release/deps".encode(),
                ],
                False,
            ),
            "no_emit": ([b"--crate-name", b"app", b"--target", TARGET.encode()], False),
        }
        for name, (arguments, expect_injection) in cases.items():
            captures = self.capture_dir(f"classify-{name}")
            argv_out = self.work / f"fake-argv-{name}.bin"
            subprocess.run(
                [bytes(self.shim)] + arguments,
                env=self.environment(
                    captures,
                    TEMPER_EXP_INJECT=f"generate={profile}",
                    TEMPER_EXP_FAKE_ARGV_OUT=str(argv_out),
                ),
                input=b"",
                capture_output=True,
            )
            record = self.records(captures)[0]
            received = argv_out.read_bytes()
            injected_flag = f"-Cprofile-generate={profile}".encode()
            self.check(
                f"classification `{name}` injects PGO controls only when it must",
                bool(record["injected"]) == expect_injection
                and (injected_flag in received) == expect_injection,
                {
                    "class": record["class"],
                    "injected": record["injected"],
                    "expected_injection": expect_injection,
                },
            )
            if not expect_injection:
                self.check(
                    f"classification `{name}` forwards the original argv unchanged",
                    received == framed(arguments),
                    {"equal": received == framed(arguments)},
                )
            else:
                self.check(
                    f"classification `{name}` appends exactly one control after the Cargo arguments",
                    received == framed(arguments + [injected_flag]),
                    {"equal": received == framed(arguments + [injected_flag])},
                )

    def run(self) -> dict:
        self.argument_bytes_survive()
        self.exit_semantics()
        self.protocol_failures()
        self.classification_predicate()
        return {
            "probe": "us-002-process",
            "passed": all(item["passed"] for item in self.checks),
            "checks_total": len(self.checks),
            "checks_failed": [item for item in self.checks if not item["passed"]],
            "checks": self.checks,
        }


def main() -> int:
    probes = Probes(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]))
    report = probes.run()
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
