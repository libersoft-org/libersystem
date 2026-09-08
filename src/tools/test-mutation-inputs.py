#!/usr/bin/env python3
"""Exercise the mutation gate's production input copy while guest scratch disappears."""

import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

HERE = Path(__file__).resolve().parent


class MutationInputCopy(unittest.TestCase):
    def exercise(self, mutation=None):
        source = (HERE / "check-implementation-mutations.sh").read_text()
        start = source.index("seed_mutation_inputs() {\n")
        end = source.index("\n}\n", start) + len("\n}\n")
        helper = source[start:end]
        if mutation == "copy-scratch":
            helper = '''seed_mutation_inputs() {
    for part in boot state image cache; do
        mkdir -p "$WORK/.build/$part"
        rsync -a --delete "$REPO/.build/$part/" "$WORK/.build/$part/"
    done
}
'''
        elif mutation == "unlocked":
            self.assertIn("\t\tflock 9\n", helper)
            helper = helper.replace("\t\tflock 9\n", "", 1)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, work = root / "repo", root / "work"
            target = "x86_64-unknown-none"
            boot = repo / ".build/boot"
            state = repo / ".build/state"
            staged = repo / f".build/image/{target}"
            records = repo / f".build/cache/{target}"
            for directory in (boot, state, staged, records):
                directory.mkdir(parents=True)
            for name in ("init-x86_64.pkg", "volume-x86_64.pkg", "system-volume-x86_64.img", "system-volume-x86_64.uuid"):
                (boot / name).write_bytes(name.encode())
            # The sender has enumerated later files before this transfer finishes. Removing the
            # later console capture now reproduces rsync's actual vanished-file error (24).
            (boot / "init-x86_64.pkg").write_bytes(b"I" * (4 * 1024 * 1024))
            bootstrap = boot / "bootstrap-x86_64/libexec"
            bootstrap.mkdir(parents=True)
            (bootstrap / "system_manager").write_bytes(b"bootstrap")
            (staged / "fixture").write_bytes(b"staged input")
            (records / "executable-fixture.sha256").write_text("recorded digest\n")
            (records / "unused-object.o").write_bytes(b"not a preflight input")
            scratch = [boot / "virtio-console.424242.out", state / "kernel-test-x86_64.424242.elf"]
            for path in scratch:
                path.write_bytes(b"another guest's scratch")
            lock = state / f"build-{target}.lock"
            lock.touch()
            finished = threading.Event()
            observed = []

            def remove_guest_scratch():
                destination = work / ".build/boot"
                while not finished.wait(0.001):
                    if list(destination.glob(".init-x86_64.pkg.*")):
                        held = subprocess.run(["flock", "-n", str(lock), "true"], capture_output=True).returncode != 0
                        for path in scratch:
                            path.unlink()
                        observed.append(held)
                        return

            watcher = threading.Thread(target=remove_guest_scratch)
            watcher.start()
            wrapper = '''rsync() {
    if [[ "${REMOVE_REQUIRED:-0}" == 1 && "$*" == *"/.build/boot/"* ]]; then
        rm -f "$REPO/.build/boot/init-x86_64.pkg"
    fi
    command rsync --bwlimit=4096 "$@"
}
'''
            script = "set -euo pipefail\nfail() { echo \"$*\" >&2; exit 1; }\n" + wrapper + helper + "\nseed_mutation_inputs\n"
            try:
                result = subprocess.run(["bash", "-c", script], env=dict(os.environ, REPO=str(repo), WORK=str(work), REMOVE_REQUIRED="1" if mutation == "missing-required" else "0"), capture_output=True, text=True, timeout=10)
            finally:
                finished.set()
                watcher.join(timeout=2)
            copied = {str(path.relative_to(work / ".build")): path.read_bytes() for path in (work / ".build").rglob("*") if path.is_file()}
            return result, copied, observed

    def test_guest_scratch_can_disappear_without_entering_the_copy(self):
        result, copied, observed = self.exercise()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(observed, [True], "the real copy must hold the staged-image producer lock")
        self.assertEqual(copied["boot/init-x86_64.pkg"], b"I" * (4 * 1024 * 1024))
        self.assertEqual(copied["boot/bootstrap-x86_64/libexec/system_manager"], b"bootstrap")
        self.assertEqual(copied["image/x86_64-unknown-none/fixture"], b"staged input")
        self.assertEqual(copied["cache/x86_64-unknown-none/executable-fixture.sha256"], b"recorded digest\n")
        self.assertFalse(any("424242" in path or path.endswith(".o") for path in copied))
        broken, _, observed = self.exercise("copy-scratch")
        self.assertEqual(observed, [False])
        self.assertEqual(broken.returncode, 24, broken.stderr)
        self.assertIn("virtio-console.424242.out", broken.stderr)

    def test_producer_lock_is_part_of_acquiring_the_inputs(self):
        result, _, observed = self.exercise("unlocked")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(observed, [False], "removing production flock must expose the missing exclusion")

    def test_missing_required_input_still_fails_the_actual_copy(self):
        result, _, _ = self.exercise("missing-required")
        self.assertNotEqual(result.returncode, 0, "a required source disappearing must stay fatal")
        self.assertIn("init-x86_64.pkg", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
