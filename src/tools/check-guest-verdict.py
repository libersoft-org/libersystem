#!/usr/bin/env python3
"""Host regressions for P02M0177 guest verdicts and contested loader production."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
spec = importlib.util.spec_from_file_location("guest_verdict", HERE / "guest-verdict.py")
watcher = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = watcher
spec.loader.exec_module(watcher)


class GuestVerdicts(unittest.TestCase):
    positive = "\n".join((
        "LiberSystem UEFI loader",
        "dma: every bus-mastering device is translated",
        "driver.virtio-gpu: online (1)",
        "ConsoleService: a frame reached the display",
        "",
    ))

    def decide(self, text, elapsed, running=True, name="iommu-default"):
        return watcher.verdict(watcher.CASES[name], text, elapsed, running)[0]

    def test_success_must_survive_the_existing_observation(self):
        self.assertEqual(self.decide(self.positive, 119), "wait")
        self.assertEqual(self.decide(self.positive, 120), "pass")
        self.assertEqual(watcher.CASES["iommu-traffic"].observe, 300)
        self.assertEqual(watcher.CASES["iommu-plain"].observe, 120)

    def test_success_then_panic_reset_or_retraction_fails(self):
        for late in (
            "KERNEL PANIC: later failure\n",
            "LiberSystem UEFI loader\n",
            "dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY\n",
            "DeviceManager: restarting virtio-gpu\n",
            "driver.virtio-gpu: online (2)\n",
            "iommu: FAULT\n",
            "ConsoleService: a frame did NOT reach the display\n",
        ):
            with self.subTest(late=late):
                self.assertEqual(self.decide(self.positive, 2), "wait")
                self.assertEqual(self.decide(self.positive + late, 12), "fail")
                self.assertEqual(self.decide(self.positive + late, 120), "fail")
        self.assertEqual(self.decide(self.positive, 12, running=False), "fail")

    def test_altered_payload_does_not_end_at_kernel_loaded(self):
        intermediate = "loader: kernel loaded\n"
        self.assertEqual(self.decide(intermediate, 2, name="signed-payload"), "wait")
        terminal = intermediate + "loader panic: loader: the live system volume is not what the boot medium's signed manifest records\n"
        self.assertEqual(self.decide(terminal, 3, name="signed-payload"), "pass")
        self.assertEqual(self.decide(terminal + "LiberSystem kernel is starting\n", 3, name="signed-payload"), "fail")
        # The withdrawn global-marker mutation demonstrably accepts the intermediate log.
        global_marker_mutant = "loader: kernel loaded" in intermediate
        self.assertTrue(global_marker_mutant)
        self.assertNotEqual(self.decide(intermediate, 2, name="signed-payload"), "pass")

    def test_bootstrap_refusal_must_reach_final_handoff_refusal(self):
        reason = "loader: this source was chosen and its bootstrap list is not on it\n"
        self.assertEqual(self.decide(reason, 1, name="signed-absent-list"), "wait")
        self.assertEqual(self.decide(reason + "loader panic: refusing to hand off\n", 2, name="signed-absent-list"), "pass")

    def test_manifest_reason_is_not_its_terminal_panic(self):
        reason = "loader: the manifest's signature does not check out - refusing to boot from it\n"
        self.assertEqual(self.decide(reason, 1, name="signed-manifest"), "wait")
        self.assertEqual(self.decide(reason + "loader panic: signed manifest was refused\n", 2, name="signed-manifest"), "pass")
        self.assertEqual(self.decide(reason + "loader panic", 2, name="signed-manifest"), "wait")
        final_halt = "loader: FATAL - the boot medium's signed manifest was refused, so which volume it names cannot be established\n"
        for name in ("signed-manifest", "signed-context", "signed-port-manifest", "secure-altered-manifest"):
            self.assertEqual(self.decide(reason, 2, name=name), "wait")
            self.assertEqual(self.decide(reason + final_halt, 2, name=name), "pass")
            self.assertEqual(self.decide(reason + "loader: FATAL - unrelated failure\n", 2, name=name), "wait")

    def test_silent_firmware_refusals_keep_their_backstop(self):
        for name in ("secure-unsigned", "secure-altered-loader"):
            self.assertEqual(self.decide("", 119, name=name), "wait")
            self.assertEqual(self.decide("", 120, name=name), "pass")
            self.assertEqual(self.decide("", 2, running=False, name=name), "fail")
            self.assertEqual(self.decide("loader: TEST TRUST\n", 120, name=name), "fail")

    def test_runner_stops_only_its_own_process_group(self):
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "guest.log"
            unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
            try:
                code = "import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('loader: kernel loaded\\n'); time.sleep(30)"
                result = subprocess.run([sys.executable, str(HERE / "guest-verdict.py"), "signed-clean", str(log), "--", sys.executable, "-c", code, str(log)], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIsNone(unrelated.poll())
            finally:
                unrelated.terminate()
                unrelated.wait(timeout=5)

    def test_inventory_names_every_guest_gate_profile_and_case(self):
        import re
        catalog = (ROOT / "src/tools/verify-model/src/catalog.rs").read_text()
        inventory = (ROOT / "docs/verification/P02M0177-guest-cases.md").read_text()
        for name in ("PROFILE_ROW_GATES", "GATES_THAT_BOOT_A_GUEST"):
            match = re.search(r"pub const " + name + r".*?=\s*\[(.*?)\];", catalog, re.S)
            self.assertIsNotNone(match)
            for gate in re.findall(r'"([^"]+)"', match.group(1)):
                self.assertIn(f"`{gate}`", inventory)
        self.assertIn("`concurrent-selection`", inventory)
        for case in watcher.CASES:
            self.assertIn(f"`{case}`", inventory)


class LoaderContention(unittest.TestCase):
    def exercise(self, shared_target=False, old_sequence=False):
        import json
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            helper = root / "src/tools/build-loader-private.sh"
            helper.parent.mkdir(parents=True)
            (root / "src/boot/loader").mkdir(parents=True)
            shared = root / ".build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi"
            shared.parent.mkdir(parents=True)
            shared.write_text("ordinary-loader")
            os.utime(shared, ns=(1800000000123456789, 1800000000123456789))
            original = (shared.read_bytes(), shared.stat().st_mtime_ns)
            (root / ".build/state").mkdir()
            (root / "bin").mkdir()
            mock = root / "bin/cargo"
            mock.write_text(r'''#!/usr/bin/env python3
import json,os,pathlib,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
target=pathlib.Path(os.environ.get('CARGO_TARGET_DIR', str(root/'.build/cargo/loader')))
loader=target/'x86_64-unknown-uefi/debug/libersystem-loader.efi'
loader.parent.mkdir(parents=True, exist_ok=True)
loader.write_text(os.environ['LIBER_TRUST_PROFILE'])
with (root/'builds.jsonl').open('a') as log:
    log.write(json.dumps({'target': str(target), 'profile': os.environ['LIBER_TRUST_PROFILE']})+'\n')
(root/'built').touch()
while not (root/'contending').exists(): time.sleep(.005)
''')
            mock.chmod(0o755)
            source = (HERE / "build-loader-private.sh").read_text()
            copy = '\tcp "$target/x86_64-unknown-uefi/debug/libersystem-loader.efi" "$output"\n'
            self.assertEqual(source.count(copy), 1)
            if shared_target or old_sequence:
                private_target = 'target="$(dirname "$output")/cargo-loader"'
                self.assertEqual(source.count(private_target), 1)
                source = source.replace(private_target, 'target="' + str(root / '.build/cargo/loader') + '"')
            if old_sequence:
                source = source.replace(copy, "")
                # Deterministically put the competing producer at the former unprotected copy
                # point; no production hook or probabilistic race is involved.
                source += '\nwhile [[ ! -f "$root/contender-done" ]]; do sleep .005; done\n' + copy
            helper.write_text(source)
            output = root / "private/loader-test-trust.efi"
            env = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}", FIXTURE_ROOT=str(root))
            env.pop("CARGO_TARGET_DIR", None)
            first = subprocess.Popen(["bash", str(helper), "test-trust", str(output)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 5
                while not (root / "built").exists():
                    self.assertLess(time.monotonic(), deadline, "first loader build never reached contention seam")
                    time.sleep(.005)
                ordinary_preserved = (shared.read_bytes(), shared.stat().st_mtime_ns) == original
                (root / "contending").touch()
                # flock blocks behind production's build-and-copy when corrected, and wins
                # before the copied-outside-lock negative mutation's delayed copy.
                subprocess.run(["flock", str(root / ".build/state/kernel-test-build.lock"), "bash", "-c", 'LIBER_TRUST_PROFILE=external-release cargo build; touch "$1"', "fixture", str(root / "contender-done")], env=env, check=True, timeout=5)
                stdout, stderr = first.communicate(timeout=5)
                self.assertEqual(first.returncode, 0, stdout + stderr)
                profile = output.read_text()
                self.assertEqual(shared.read_text(), "external-release")
                after_contender = (shared.read_bytes(), shared.stat().st_mtime_ns)
                release = output.with_name("loader-release.efi")
                result = subprocess.run(["bash", str(helper), "external-release", str(release)], env=env, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(release.read_text(), "external-release")
                ordinary_preserved &= (shared.read_bytes(), shared.stat().st_mtime_ns) == after_contender
                builds = [json.loads(line) for line in (root / "builds.jsonl").read_text().splitlines()]
                self.assertEqual(len(builds), 3)  # Two fixture profiles and the ordinary contender.
                private_target = str(output.parent / "cargo-loader")
                isolated = ordinary_preserved and builds[0]["target"] == builds[2]["target"] == private_target
                log = ("loader: THIS KERNEL IS NOT AUTHENTICATED\nloader: kernel loaded\n" if profile == "test-trust" else "loader panic: carries no SIGNED manifest, and this build authenticates what it boots\n")
                decision = watcher.verdict(watcher.CASES["signed-downgrade-test"], log, 120, True)[0]
                return profile, decision, isolated
            finally:
                if first.poll() is None:
                    first.kill()
                    first.communicate()

    def test_contested_build_retains_its_profile_and_verdict(self):
        self.assertEqual(self.exercise(), ("test-trust", "pass", True))
        self.assertEqual(self.exercise(shared_target=True), ("test-trust", "pass", False))
        self.assertEqual(self.exercise(old_sequence=True), ("external-release", "fail", False))


class LoaderTimestampIsolation(unittest.TestCase):
    def exercise(self, mode, direct_stamping=False):
        import re
        source = (ROOT / "src/harness/mkimage.sh").read_text()

        def function(name):
            match = re.search(r"^" + name + r"\(\) \{\n.*?^\}\n", source, re.M | re.S)
            self.assertIsNotNone(match, name)
            return match.group(0)

        image_var = "efi_img" if mode == "iso" else "esp"
        production = function("make_" + mode)
        staging = re.search(r'\tmformat -i "\$' + image_var + r'".*?\tmcopy -i "\$' + image_var + r'" "\$staged" ::/kernel\n', production, re.S)
        self.assertIsNotNone(staging)
        staging = staging.group(0)
        if direct_stamping:
            stage_copy = '\tlocal staged_loader="$BUILD/loader.$$.efi"\n\tstage_loader "$staged_loader"\n\tstamp_epoch "$staged"\n'
            self.assertEqual(staging.count(stage_copy), 1)
            staging = staging.replace(stage_copy, '\tstamp_epoch "$LOADER_EFI" "$staged"\n')
            staging = staging.replace('"$staged_loader"', '"$LOADER_EFI"')

        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            loader = root / "ordinary-loader.efi"
            loader.write_bytes(b"loader bytes must stay identical\x00\xff")
            os.utime(loader, ns=(1800000000123456789, 1800000000123456789))
            original = (loader.read_bytes(), loader.stat().st_mtime_ns)
            kernel = root / "kernel"
            kernel.write_text("staged kernel")
            fat = root / "boot.fat"
            with fat.open("wb") as file:
                file.truncate(4 * 1024 * 1024)
            env = dict(os.environ, BUILD=str(root), SLUG="fixture", LOADER_EFI=str(loader),
                       FAT_IMAGE=str(fat), STAGED_KERNEL=str(kernel), SOURCE_DATE_EPOCH="1735689600",
                       MTOOLS_FAT_SERIAL="0x4C696265", MTOOLS_SKIP_CHECK="1", TZ="UTC")
            # Run the production FAT staging operations, including the real mtools copies and
            # cleanup. No image-builder stub decides what file or timestamp reaches the FAT.
            script = ("set -euo pipefail\nCANDIDATES=()\n" + function("cleanup") +
                      "trap cleanup EXIT\n" + function("stamp_epoch") + function("stage_loader") +
                      'stage_fixture() {\n\tlocal ' + image_var + '="$FAT_IMAGE" staged="$STAGED_KERNEL"\n' + staging + '}\nstage_fixture\n')
            result = subprocess.run(["bash", "-c", script], env=env, text=True, capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            extracted = root / "extracted.efi"
            subprocess.run(["mcopy", "-m", "-i", str(fat), "::/EFI/BOOT/BOOTX64.EFI", str(extracted)], env=env, check=True, capture_output=True, timeout=5)
            self.assertEqual(extracted.read_bytes(), original[0])
            self.assertEqual(extracted.stat().st_mtime_ns, int(env["SOURCE_DATE_EPOCH"]) * 1_000_000_000)
            self.assertEqual(list(root.glob("loader.*.efi")), [], "private loader staging must be cleaned")
            return (loader.read_bytes(), loader.stat().st_mtime_ns) == original

    def test_iso_and_img_normalize_only_their_private_loader_copy(self):
        for mode in ("iso", "img"):
            with self.subTest(mode=mode):
                self.assertTrue(self.exercise(mode))
                self.assertFalse(self.exercise(mode, direct_stamping=True))


class PerfImageIsolation(unittest.TestCase):
    def exercise(self, omit_private_output=False, ignore_private_output=False):
        import json
        import re
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tools = root / "src/tools"
            harness = root / "src/harness"
            tools.mkdir(parents=True)
            harness.mkdir(parents=True)
            (root / "bin").mkdir()
            boot = root / ".build/boot"
            boot.mkdir(parents=True)
            kernel = root / ".build/cargo/kernel/x86_64-unknown-none/debug/kernel"
            kernel.parent.mkdir(parents=True)
            kernel.write_text("kernel fixture")
            (root / "product.conf").write_text((ROOT / "product.conf").read_text())
            (tools / "volume-pairing.sh").write_text((HERE / "volume-pairing.sh").read_text())
            watcher_script = tools / "guest-verdict.py"
            watcher_script.write_text((HERE / "guest-verdict.py").read_text())
            watcher_script.chmod(0o755)
            available = root / "bin/qemu-system-x86_64"
            available.write_text("#!/bin/sh\nexit 99\n")
            available.chmod(0o755)
            source = (HERE / "check-perf-anchor.sh").read_text()
            opt_in = 'LIBER_IMAGE_OUTPUT="$work/$profile.iso" '
            self.assertEqual(source.count(opt_in), 1)
            if omit_private_output:
                source = source.replace(opt_in, "")
            script = tools / "check-perf-anchor.sh"
            script.write_text(source)

            # Keep mkimage's real output selection, lock, cache check, atomic image rename,
            # and receipt publication. Stub only expensive payload validation/production.
            maker = (ROOT / "src/harness/mkimage.sh").read_text()
            if ignore_private_output:
                selected = 'output="${LIBER_IMAGE_OUTPUT:-$BUILD/$SLUG.iso}"'
                self.assertEqual(maker.count(selected), 1)
                maker = maker.replace(selected, 'output="$BUILD/$SLUG.iso"')
            def replace_function(name, body):
                nonlocal maker
                pattern = r"(^" + name + r"\(\) \{\n).*?(^\}\n)"
                maker, count = re.subn(pattern, lambda match: match.group(1) + body + match.group(2), maker, count=1, flags=re.M | re.S)
                self.assertEqual(count, 1)
            prologue = re.search(r"^make_iso\(\) \{\n(.*?)\tlocal iso_root=", maker, re.M | re.S).group(1)
            replace_function("make_iso", prologue + '\tprintf "profile=%s\\n" "$LIBER_BOOT_PROFILE" >"$out"\n\tmv "$out" "$final"\n\techo "$final"\n')
            replace_function("verify_boot_artifacts", '\tmanifest_rows=fixture\n')
            replace_function("image_input_key", '\tprintf "mode=%s\\n" "$mode_input"\n\thash_inputs "$kernel" "$LOADER_EFI"\n')
            maker_script = harness / "mkimage.sh"
            maker_script.write_text(maker)
            maker_script.chmod(0o755)
            for name in ("system-volume-bootable-x86_64.img", "system-volume-bootable-x86_64.uuid"):
                (boot / name).write_text("fixture")
            shipping = [boot / ("libersystem.iso" + suffix) for suffix in ("", ".build-key", ".build-digest")]
            for path in shipping:
                path.write_text("shipping input: " + path.name)
            before = [path.read_bytes() for path in shipping]

            runner = harness / "qemu-run.sh"
            runner.write_text(r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
assert not os.environ.get('BOOT_IMAGE'), 'perf must still assemble its fresh internal ISO'
profile = os.environ['LIBER_BOOT_PROFILE']
loader = root / (profile + '.efi')
loader.write_text(profile)
env = dict(os.environ, LOADER_EFI=str(loader))
image = pathlib.Path(subprocess.check_output([str(root / 'src/harness/mkimage.sh'), 'iso', sys.argv[2]], cwd=root, env=env, text=True).strip())
assert image.is_file()
assert pathlib.Path(str(image) + '.build-key').is_file()
assert pathlib.Path(str(image) + '.build-digest').is_file()
with (root / 'produced.jsonl').open('a') as record:
    record.write(json.dumps({'profile': profile, 'image': str(image), 'key': pathlib.Path(str(image) + '.build-key').read_text()}) + '\n')
log = pathlib.Path(os.environ['SERIAL'].removeprefix('file:'))
log.write_text('boot OK\n' + ('PERF tsc_hz 123\n' if profile == 'development-trace' else ''))
''')
            runner.chmod(0o755)
            env = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}", FIXTURE_ROOT=str(root))
            env.pop("BOOT_IMAGE", None)
            env.pop("LIBER_IMAGE_OUTPUT", None)
            result = subprocess.run(["bash", str(script)], cwd=root, env=env, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            records = [json.loads(line) for line in (root / "produced.jsonl").read_text().splitlines()]
            self.assertEqual([row["profile"] for row in records], ["development-trace", "development"])
            # The same oracle must fail when either caller opt-in or producer support is removed.
            unchanged = before == [path.read_bytes() for path in shipping]
            private = all(Path(row["image"]) not in shipping for row in records)
            distinct = records[0]["image"] != records[1]["image"]
            return unchanged and private and distinct

    def test_perf_boots_cannot_replace_the_shipping_iso_or_its_receipts(self):
        self.assertTrue(self.exercise())
        self.assertFalse(self.exercise(omit_private_output=True))
        self.assertFalse(self.exercise(ignore_private_output=True))


class KernelBuildOnly(unittest.TestCase):
    def exercise(self, remove_return):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            harness = root / "src/harness"
            harness.mkdir(parents=True)
            (root / "src/kernel").mkdir()
            (root / "bin").mkdir()
            source = (ROOT / "src/harness/test-kernel.sh").read_text()
            if remove_return:
                start = source.index('if [[ "$BUILD_ONLY" == "1" ]]; then\n\tif [[ -n "${LIBER_TIMING_LOG:-}"')
                end = source.index('\nfi\n', start) + len('\nfi\n')
                source = source[:start] + source[end:]
            script = harness / "test-kernel.sh"
            script.write_text(source)
            runner = harness / "qemu-run.sh"
            runner.write_text('#!/usr/bin/env bash\ntouch "$FIXTURE_ROOT/qemu-started"\nexit 1\n')
            runner.chmod(0o755)
            cargo = root / "bin/cargo"
            cargo.write_text('''#!/usr/bin/env python3
import json,os,pathlib,subprocess
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
assert '--tests' in __import__('sys').argv
out=root/'.build/fixture-kernel'
# A real ELF exercises staging and nm; the compiler is stubbed, not counted as a
# cold production-kernel compilation. That separate acceptance run remains owed.
code='const char _RNvNvCs0_6kernel7fixture4CASE = 1; int main(void) { return 0; }'
subprocess.run(['cc','-x','c','-','-o',str(out)],input=code,text=True,check=True)
print(json.dumps({'reason':'compiler-artifact','executable':str(out),'target':{'name':'kernel'}}))
''')
            cargo.chmod(0o755)
            env = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}", FIXTURE_ROOT=str(root))
            result = subprocess.run(["bash", str(script), "x86_64", "--build-only"], env=env, capture_output=True, text=True, timeout=5)
            symbols = subprocess.check_output(["nm", str(root / ".build/fixture-kernel")], text=True)
            self.assertIn("_RNvNvCs0_6kernel7fixture4CASE", symbols)
            self.assertFalse((root / ".build/boot").exists())
            self.assertFalse(list((root / ".build/state").glob("kernel-test-*.elf")))
            return result.returncode, (root / "qemu-started").exists(), result.stdout

    def test_build_only_stages_descriptors_and_returns_before_qemu(self):
        status, started, output = self.exercise(False)
        self.assertEqual(status, 0, output)
        self.assertFalse(started)
        self.assertIn("BUILD PASS", output)
        status, started, _ = self.exercise(True)
        self.assertNotEqual(status, 0)
        self.assertTrue(started)


if __name__ == "__main__":
    unittest.main(verbosity=2)
