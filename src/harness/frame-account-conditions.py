#!/usr/bin/env python3
"""THE CONDITIONS OF AN EMULATED FRAME-ACCOUNT RUN, captured while its guest runs.

`check-qemu-2d-account.sh --arch aarch64|riscv64` boots its guest cold, through `lab.sh scenario-cold`, and every number
the port rows publish is a number under these conditions: the boot ESP and the kernel the cold runner staged for this
QEMU, the kernel cargo built, the manifest-selected driver, DisplayService and demo artifacts, the running system
volume, QEMU's command line and version, and the host CPU. They are read off the one running QEMU of the architecture
- its process id names the staged files - so they are the files that guest is running, not the ones on disk afterwards.

usage: frame-account-conditions.py ARCH OUT.json
"""

import datetime
import hashlib
import json
import pathlib
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGETS = {"aarch64": "aarch64-unknown-none", "riscv64": "riscv64gc-unknown-none-elf"}


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            sha.update(block)
    return {"path": str(path), "bytes": path.stat().st_size, "sha256": sha.hexdigest()}


def running_qemu(arch):
    found = []
    for proc in pathlib.Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            args = (proc / "cmdline").read_bytes().rstrip(b"\0").decode().split("\0")
        except (OSError, UnicodeDecodeError):
            continue
        if args and pathlib.Path(args[0]).name == "qemu-system-" + arch:
            found.append((proc, args))
    if len(found) != 1:
        raise SystemExit(f"frame-account-conditions: expected one {arch} QEMU, found {len(found)}")
    return found[0]


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in TARGETS:
        raise SystemExit("usage: frame-account-conditions.py aarch64|riscv64 OUT.json")
    arch, out = sys.argv[1], pathlib.Path(sys.argv[2])
    target = TARGETS[arch]
    proc, args = running_qemu(arch)
    cpu = next((line.split(":", 1)[1].strip() for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), None)
    record = {
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "arch": arch,
        "pid": int(proc.name),
        "qemu_args": args,
        "qemu_version": subprocess.check_output([args[0], "--version"], text=True).splitlines()[0],
        "host_cpu": cpu,
        "guest_profile": "development-trace",
        "clock": "CNTVCT_EL0" if arch == "aarch64" else "global time / rdtime",
        "artifacts": {},
    }
    for label, path in [
        ("boot_esp", ROOT / f".build/boot/esp-{arch}.{proc.name}.img"),
        ("boot_kernel", ROOT / f".build/boot/kernel-{arch}.{proc.name}.staged"),
        ("kernel_dev", ROOT / f".build/cargo/kernel/{target}/debug/kernel"),
    ]:
        record["artifacts"][label] = digest(path)
    manifest = tomllib.loads((ROOT / "src/user/services/manifest.toml").read_text())
    for name in ["virtio_gpu", "display_service", "test2d-sw"]:
        program = next(program for program in manifest["programs"] if program["name"] == name)
        dynamic = program.get("linkage") == "dynamic"
        path = ROOT / f".build/image/{target}" / program["destination"].removesuffix(".lsexe") if dynamic else ROOT / f".build/cargo/user/{target}/debug/{name}"
        record["artifacts"][name] = digest(path) | {"profile": "release PIE" if dynamic else "cargo dev profile"}
    for arg in args:
        for field in arg.split(","):
            if field.startswith("file="):
                path = pathlib.Path(field[5:])
                if path.name.startswith(f"virtio-blk-{arch}.") and path.suffix == ".img":
                    record["artifacts"]["running_system_volume"] = digest(path)
    record["boot_fd_inputs"] = {}
    for arg in args:
        if "/proc/self/fd/" in arg:
            fd = arg.split("/proc/self/fd/", 1)[1].split(",", 1)[0]
            path = proc / "fd" / fd
            record["boot_fd_inputs"][fd] = digest(path) | {"target": str(path.resolve())}
    dma = ROOT / f".build/boot/dma-mode.{proc.name}.record"
    record["dma_record"] = dma.read_text() if dma.exists() else None
    out.write_text(json.dumps(record, indent=1) + "\n")
    print(f"frame-account-conditions: {arch} pid {proc.name}, boot ESP {record['artifacts']['boot_esp']['sha256']}")


if __name__ == "__main__":
    main()
