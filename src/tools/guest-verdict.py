#!/usr/bin/env python3
"""Run one guest until that case's assertions and observation interval are complete.

The case inventory is docs/verification/P02M0177-guest-cases.md. Loader panic is an
expected terminal refusal only in the named refusal cases. Health cases keep their
existing full observation intervals, including failures after their positive lines.
"""

import argparse
from dataclasses import dataclass
from pathlib import Path
import os
import re
import signal
import subprocess
import sys
import time


@dataclass(frozen=True)
class Case:
    required: tuple[str, ...]
    forbidden: tuple[str, ...] = ()
    timeout: float = 120
    observe: float = 0
    health: bool = False


LOADED = r"loader: kernel loaded"
STARTED = r"LiberSystem kernel is starting"
PANIC = r"loader panic"
# read_pairing refuses the medium before kernel selection and calls arch::halt directly.
# Its earlier verifier reason is not final; this complete FATAL line is.
MANIFEST_END = PANIC + r"|loader: FATAL - the boot medium's signed manifest (was refused|is present and could not be read), so which volume it names cannot be established"
REASON = r"refusing to boot from it|does not check out|was refused"
HANDOFF_REFUSED = r"refusing to hand off"
HEALTH_FAILURES = (r"KERNEL PANIC", r"loader: FATAL")
CASES = {
    "signed-clean": Case((LOADED,)),
    "signed-manifest": Case((REASON, MANIFEST_END), (LOADED,)),
    "signed-payload": Case((r"the live system volume is not what the boot medium's signed manifest records", PANIC), (STARTED,)),
    "signed-selected-volume": Case((r"signed manifest is there and could not be read|does not check out|was refused", PANIC), (LOADED,)),
    "signed-context": Case((r"refusing to boot from it", MANIFEST_END), (LOADED,)),
    "signed-volume-pairing": Case((r"signed for a different volume than the one this medium is paired with", HANDOFF_REFUSED), (STARTED,)),
    "signed-absent-list": Case((r"this source was chosen and its bootstrap list is not on it", HANDOFF_REFUSED), (STARTED,)),
    "signed-mixed-release": Case((r"belongs to a different release than the one already verified in this boot", PANIC), (STARTED,)),
    "signed-downgrade-test": Case((r"THIS KERNEL IS NOT AUTHENTICATED", LOADED)),
    "signed-downgrade-release": Case((r"carries no SIGNED manifest, and this build authenticates what it boots", PANIC), (LOADED,)),
    "signed-port-clean": Case((LOADED,), timeout=300),
    "signed-port-manifest": Case((REASON, MANIFEST_END), (LOADED,), timeout=300),
    "secure-signed": Case((r"loader: firmware SecureBoot=1 SetupMode=0 \(enforcing\)",)),
    # Firmware silence is not an early verdict. These still observe the complete 120 s.
    "secure-unsigned": Case((), (r"loader: TEST TRUST|loader: release trust",), observe=120),
    "secure-altered-loader": Case((), (r"loader: TEST TRUST|loader: release trust",), observe=120),
    "secure-altered-manifest": Case((REASON, MANIFEST_END), (LOADED,)),
    "perf-trace": Case((r"boot OK", r"PERF tsc_hz"), timeout=90),
    "perf-plain": Case((r"boot OK",), (r"PERF tsc_hz",), timeout=90),
    "iommu-default": Case(
        (r"LiberSystem UEFI loader", r"dma: every bus-mastering device is translated", r"driver\.virtio-gpu: online \(", r"ConsoleService: a frame reached the display"),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION", r"dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY", r"iommu: FAULT", r"DeviceManager: restarting virtio-gpu", r"ConsoleService: a frame did NOT reach the display"),
        observe=120, health=True,
    ),
    "iommu-plain": Case(
        (r"LiberSystem UEFI loader", r"iommu: no virtio-iommu on this machine", r"dma: DEGRADED ISOLATION"),
        HEALTH_FAILURES, observe=120, health=True,
    ),
    "iommu-traffic": Case(
        (r"iommu: virtio-iommu is translating", r"iommu: .* attached to domain", r"driver\.virtio-net: online \(", r"network: configured via DHCP"),
        HEALTH_FAILURES, timeout=300, observe=300, health=True,
    ),
}


def verdict(case: Case, text: str, elapsed: float, running: bool) -> tuple[str, str]:
    # Only complete serial lines count. A partial terminal line is still being emitted.
    text = text[:text.rfind("\n") + 1]
    for pattern in case.forbidden:
        if re.search(pattern, text):
            return "fail", f"forbidden signal: {pattern}"
    if case.health:
        if text.count("LiberSystem UEFI loader") > 1:
            return "fail", "guest reset/reboot: multiple loader banners"
        if len(re.findall(r"driver\.virtio-gpu: online \(", text)) > 1:
            return "fail", "display driver restarted"
        if not running:
            return "fail", "guest exited before completing its observation interval"
    complete = all(re.search(pattern, text) for pattern in case.required)
    if complete and elapsed >= case.observe:
        # With no positive signal, silence is only judged at the calibrated backstop.
        if not case.required and not running:
            return "fail", "guest exited before the firmware refusal backstop"
        return "pass", "case assertions and observation complete"
    if elapsed >= case.timeout:
        missing = [pattern for pattern in case.required if not re.search(pattern, text)]
        return "fail", f"timeout; missing signals: {missing}"
    if not running:
        return "fail", "guest exited without its case's final verdict"
    return "wait", ""


def stop_group(proc: subprocess.Popen) -> None:
    # Own process group only; never a machine-wide QEMU name match.
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()


def run(case_name: str, log: Path, command: list[str]) -> int:
    case = CASES[case_name]
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_bytes(b"")
    started = time.monotonic()
    proc = subprocess.Popen(command, start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    previous = {}
    def interrupted(signum, _frame):
        raise KeyboardInterrupt
    for sig in (signal.SIGTERM, signal.SIGINT):
        previous[sig] = signal.signal(sig, interrupted)
    try:
        while True:
            contents = log.read_text(encoding="utf-8", errors="replace")
            status, reason = verdict(case, contents, time.monotonic() - started, proc.poll() is None)
            if status != "wait":
                print(f"guest-verdict: {case_name}: {status}: {reason}; log: {log}", file=sys.stderr)
                if status == "fail":
                    print("\n".join(contents.splitlines()[-40:]), file=sys.stderr)
                return 0 if status == "pass" else 1
            time.sleep(0.05)
    except KeyboardInterrupt:
        return 130
    finally:
        stop_group(proc)
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=CASES)
    parser.add_argument("log", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("a guest command is required after --")
    return run(args.case, args.log, command)


if __name__ == "__main__":
    sys.exit(main())
