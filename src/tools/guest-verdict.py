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
    # THE DMA-MODE ROWS. An enforcing boot admits the driver that requires translation; a degraded
    # boot refuses it BY NAME AND VALUE and admits the trusted rows into the visible degraded
    # inventory; a loader that cannot state the mode halts before it loads a kernel; a kernel handed
    # no mode refuses every claim. `dma-port-*` are the emulated ports' rows, with their own budget.
    "dma-admits": Case(
        (r"dma: boot DMA mode enforcing-required \(harness provenance", r"dma: every bus-mastering device is translated", r"driver\.virtio-net: online \(", r"network: configured via DHCP"),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION", r"REFUSED - the entry declares iommu-required"), timeout=300, observe=120, health=True,
    ),
    "dma-degraded": Case(
        (r"dma: boot DMA mode no-iommu \(harness provenance", r"REFUSED - the entry declares iommu-required and the boot mode is no-iommu", r"dma: DEGRADED ISOLATION", r"driver\.virtio-blk: online \(", r"network: no network provider on this boot - NetworkService is up without a link"),
        HEALTH_FAILURES + (r"driver\.virtio-net: online \(", r"dma: every bus-mastering device is translated"), timeout=300, observe=120, health=True,
    ),
    "dma-signed-admits": Case(
        (r"loader: DMA mode enforcing-required \(signed", r"dma: boot DMA mode enforcing-required \(signed provenance", r"dma: every bus-mastering device is translated", r"network: configured via DHCP"),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION",), timeout=300, observe=120, health=True,
    ),
    "dma-signed-degraded": Case(
        (r"loader: DMA mode no-iommu \(signed", r"dma: boot DMA mode no-iommu \(signed provenance", r"REFUSED - the entry declares iommu-required and the boot mode is no-iommu", r"dma: DEGRADED ISOLATION", r"driver\.virtio-blk: online \(", r"network: no network provider on this boot - NetworkService is up without a link"),
        HEALTH_FAILURES + (r"driver\.virtio-net: online \(",), timeout=300, observe=120, health=True,
    ),
    # THE ROLLBACK FLOOR'S ROWS. A boot the floor accepts prints its verdict before the hand-off;
    # one it refuses halts before a kernel is loaded; an unprovisioned machine says so and boots. A
    # loader the FIRMWARE refuses never prints its banner, which `secure-unsigned` already covers.
    "rollback-accepted": Case((r"loader: rollback floor [0-9]+ - generation [0-9]+ accepted", LOADED), (r"loader: FATAL",), timeout=120),
    "rollback-refused": Case((r"loader: FATAL - rollback floor",), (r"loader: rollback floor [0-9]+ - generation [0-9]+ accepted", STARTED), timeout=120),
    "rollback-unprovisioned": Case((r"loader: rollback floor - this machine is UNPROVISIONED", LOADED), (r"loader: FATAL",), timeout=120),
    "rollback-manifest-refused": Case((r"refusing to (boot from it|compose)",), (r"loader: rollback floor [0-9]+ - generation [0-9]+ accepted", STARTED), timeout=120),
    "rollback-not-enforced": Case((r"loader: rollback floor - not enforced by this build", LOADED), (r"loader: FATAL", r"ROLLBACK FLOOR ENFORCED"), timeout=120),
    "dma-loader-refused": Case((r"loader: FATAL - (no DMA mode can be handed to the kernel|.* is present beside this entry path's DMA-mode input)",), (LOADED, STARTED), timeout=300),
    "dma-kernel-refused": Case((r"dma: NO DMA MODE reached this kernel|dma: the loader's hand-off is REFUSED",), (r"driver\.[a-z-]*: online \(",), timeout=300, observe=60),
    # THE PORTS' ENFORCING PROFILES (P02M0173), and the two entry paths differ in what a boot can
    # show. A DIRECT `-kernel` boot has no loader, so the kernel's boot code selects ROOT_NONE and no
    # system volume is promoted: the service graph waits on a root that never comes, so there is no
    # DHCP and no mounted volume, and the enforcing evidence is the kernel's own DMA audit - the
    # controller translating with bypass read back off, every bus-mastering device translated, the
    # endpoints attached to their domains, no fault, no degraded admission, and the block driver
    # bound behind the controller. A UEFI boot runs the loader, which promotes a LiberFS volume as
    # ROOT_BLOCK, so the services come up and traffic can be shown: a DHCP lease through virtio-net
    # and the system volume read through virtio-blk, both through translated endpoints. That second
    # case is where the network the degraded profile refuses is proved to have come back.
    "iommu-port-ordinary-direct": Case(
        (r"iommu: virtio-iommu is translating - bypass is off and read back as off", r"dma: boot DMA mode enforcing-required \(harness provenance", r"dma: every bus-mastering device is translated", r"iommu: [0-9]+ endpoint\(s\) attached, [0-9]+ mapping\(s\) live, 0 quarantined, 0 fault", r"driver\.virtio-blk: online \("),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION", r"dma: ADMITTED UNTRANSLATED", r"REFUSED - the entry declares iommu-required", r"did not confirm its reset", r"present but NOT enforcing", r"iommu: FAULT"), timeout=1500, observe=180, health=True,
    ),
    "iommu-port-ordinary-uefi": Case(
        (r"iommu: virtio-iommu is translating - bypass is off and read back as off", r"dma: boot DMA mode enforcing-required \(harness provenance", r"dma: every bus-mastering device is translated", r"driver\.virtio-net: online \(", r"network: configured via DHCP", r"driver\.virtio-blk: online \(", r"storage: vol://system mounted through its block provider"),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION", r"dma: ADMITTED UNTRANSLATED", r"REFUSED - the entry declares iommu-required", r"did not confirm its reset", r"present but NOT enforcing", r"iommu: FAULT"), timeout=1500, observe=240, health=True,
    ),
    "iommu-port-transition": Case(
        (r"iommu: the controller at [0-9a-f:.]* masters the bus", r"iommu: quiesced [a-z]* at [0-9a-f:.]* - ", r"iommu: virtio-iommu is translating - bypass is off and read back as off", r"dma: every bus-mastering device is translated"),
        HEALTH_FAILURES + (r"dma: DEGRADED ISOLATION", r"did not confirm", r"present but NOT enforcing", r"iommu: FAULT"), timeout=1500, observe=120, health=True,
    ),
    # THE PORTS' ORDINARY ROWS ADMIT (the flip P02M0173 M7 owes P02M0172): the produced record says
    # enforcing-required, the machine is translated, and the driver the degraded row refuses by
    # name comes online and passes traffic.
    "dma-port-admits": Case(
        (r"dma: boot DMA mode enforcing-required \(harness provenance", r"dma: every bus-mastering device is translated", r"driver\.virtio-net: online \(", r"network: configured via DHCP"),
        (r"KERNEL PANIC", r"loader: FATAL", r"dma: DEGRADED ISOLATION", r"REFUSED - the entry declares iommu-required"), timeout=1500, observe=240, health=True,
    ),
    "dma-port-degraded": Case(
        (r"dma: boot DMA mode no-iommu \(harness provenance", r"REFUSED - the entry declares iommu-required and the boot mode is no-iommu", r"dma: DEGRADED ISOLATION", r"driver\.virtio-blk: online \(", r"network: no network provider on this boot - NetworkService is up without a link"),
        (r"KERNEL PANIC", r"loader: FATAL", r"driver\.virtio-net: online \("), timeout=1500, observe=240, health=True,
    ),
    "dma-port-loader-refused": Case((r"loader: FATAL - (no DMA mode can be handed to the kernel|.* is present beside this entry path's DMA-mode input)",), (LOADED, STARTED), timeout=1500),
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
