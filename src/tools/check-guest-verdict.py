#!/usr/bin/env python3
"""Host regressions for guest verdicts and shared producer acquisition."""

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


class ExistingQemuAdmission(unittest.TestCase):
    """The real guard inspects real open FDs; discovery names only this fixture."""

    def exercise(self, paths, child=False, mutation=None):
        import re
        source = (ROOT / "test.sh").read_text()
        match = re.search(r"^require_no_stray_qemu\(\) \{\n.*?^\}\n", source, re.M | re.S)
        self.assertIsNotNone(match)
        guard = match.group(0)
        if mutation == "reject_private":
            exemption = '\t\t\t[[ -n "$owner" && "$parents" == *" $owner "* ]] && continue\n'
            self.assertEqual(guard.count(exemption), 1)
            guard = guard.replace(exemption, "")
        elif mutation == "trust_fixture_name":
            anchor = '\t\t\t[[ "$target" == "$build/"* ]] || continue\n'
            self.assertEqual(guard.count(anchor), 1)
            guard = guard.replace(anchor, anchor + '\t\t\t[[ "${target##*/}" == fat-media*.img ]] && continue\n')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / ".build"
            root.mkdir()
            descriptors = []
            task = None
            try:
                for name, writable in paths:
                    path = root / name.format(owner=os.getpid(), wrong_owner=os.getpid() + 10000000, key="a" * 64)
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"fixture")
                    descriptors.append(os.open(path, os.O_RDWR if writable else os.O_RDONLY))
                pid = os.getpid()
                if child:
                    # qemu-run.sh sometimes waits for QEMU, and test-kernel.sh's
                    # inherited logs belong to an ancestor rather than QEMU itself.
                    task = subprocess.Popen(["sleep", "30"], pass_fds=descriptors)
                    pid = task.pid
                script = r'''
set -euo pipefail
pgrep() { printf '%s\n' "$FIXTURE_PID"; }
die() { printf '%s\n' "$*" >&2; exit 23; }
note() { printf '%s\n' "$*" >&2; }
''' + guard + '\nrequire_no_stray_qemu aarch64\n'
                result = subprocess.run(["bash", "-c", script],
                                        env=dict(os.environ, BUILD_DIR=str(root), FIXTURE_PID=str(pid)),
                                        capture_output=True, text=True, timeout=5)
                if result.returncode:
                    self.assertEqual(result.returncode, 23, result.stderr)
                    self.assertIn("already holds this tree's disk images", result.stderr)
                    self.assertIn(str(pid), result.stderr)
                return result.returncode
            finally:
                if task is not None:
                    task.terminate()
                    task.wait(timeout=5)
                for descriptor in descriptors:
                    os.close(descriptor)

    def test_private_writable_images_and_inherited_logs_allow_parallel_guests(self):
        paths = [(name, True) for name in (
            "boot/virtio-blk-aarch64.{key}.{owner}.img",
            "boot/virtio-blk-test.{key}.{owner}.img",
            "boot/usb-media-riscv64.{key}.{owner}.img",
            "boot/esp-aarch64.{owner}.img",
            "boot/ovmf-vars.{owner}.fd",
            "boot/aavmf-vars.{owner}.fd",
            "boot/virtio-console-test.{owner}.out",
            "logs/test/aarch64-20260908T003816Z-{owner}-run.log",
            "logs/test/x86_64-20260908T004126Z-{owner}-guest.log",
        )]
        self.assertEqual(self.exercise(paths), 0)
        self.assertEqual(self.exercise(paths, child=True), 0)
        self.assertNotEqual(self.exercise(paths, mutation="reject_private"), 0)

    def test_actual_read_only_shared_descriptors_are_safe(self):
        paths = [(name, False) for name in (
            "boot/fat-media-aarch64.{key}.img",
            "boot/iso-media.{key}.iso",
            "boot/udf-media.{key}.udf",
            "boot/libersystem.iso",
            "boot/kernel-aarch64.staged",
        )]
        self.assertEqual(self.exercise(paths), 0)
        self.assertEqual(self.exercise([("../another-project/disk.img", True)]), 0)

    def test_shared_writable_images_still_refuse_even_with_fixture_names(self):
        for name in (
            "boot/virtio-blk-aarch64.{key}.img",
            "boot/usb-media-riscv64.{key}.img",
            "boot/usb-media-riscv64.123abc.img",
            "boot/fat-media-aarch64.{key}.img",
            "boot/libersystem.iso",
            "boot/esp-aarch64.img",
            "shared-image-without-extension",
            "logs/test/shared-run.log",
            "logs/test/aarch64-20260908T003816Z-{wrong_owner}-run.log",
            "boot/esp-aarch64.unrecognized.{owner}.img",
            "boot/virtio-blk-aarch64.{key}.{wrong_owner}.img",
        ):
            with self.subTest(path=name):
                paths = [("logs/test/aarch64-20260908T003816Z-{owner}-run.log", True), (name, True)]
                self.assertNotEqual(self.exercise(paths), 0)
        # The former basename exemption would silently allow writable FAT media.
        self.assertEqual(self.exercise([("boot/fat-media.{key}.img", True)],
                                       mutation="trust_fixture_name"), 0)


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


class SystemDiskAcquisition(unittest.TestCase):
    def production_function(self, name):
        import re
        source = (ROOT / "src/harness/qemu-run.sh").read_text()
        match = re.search(r"^" + name + r"\(\) \{\n.*?^\}\n", source, re.M | re.S)
        self.assertIsNotNone(match, name)
        return match.group(0)

    def contested_copy(self, unlink_before_publish=False):
        producer = self.production_function("qemu_prepare_system_disk")
        if unlink_before_publish:
            publish = '\tmv "$candidate" "$disk"\n'
            self.assertEqual(producer.count(publish), 1)
            producer = producer.replace(publish, '\trm -f "$disk"\n' + publish)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            payload = b"the current system volume\x00\xff"
            (root / "volume.img").write_bytes(payload)
            script = ("set -euo pipefail\n" + producer +
                      self.production_function("qemu_run_disk") +
                      self.production_function("scratch_sweep") +
                      self.production_function("media_sweep") + r'''
await_file() { while [[ ! -f "$1" ]]; do sleep .005; done; }
# Both producers finish their candidates before A publishes. B then pauses immediately
# before its normal rename while A acquires its copy. Only scheduling is controlled.
sync() {
    if [[ "$ROLE" == A ]]; then
        await_file "$FIXTURE/b-ready"
    else
        touch "$FIXTURE/b-ready"
        await_file "$FIXTURE/a-published"
    fi
}
mv() {
    if [[ "$ROLE" == B && "$1" == *.candidate ]]; then
        touch "$FIXTURE/b-publish-paused"
        await_file "$FIXTURE/a-copy-done"
    fi
    command mv "$@"
}
template="$(qemu_prepare_system_disk "$FIXTURE/volume.img" "$FIXTURE/disk.img")"
if [[ "$ROLE" == A ]]; then
    printf '%s\n' "$template" >"$FIXTURE/template-path"
    touch "$FIXTURE/a-published"
    await_file "$FIXTURE/b-publish-paused"
    status=0
    qemu_run_disk "$template" >"$FIXTURE/private-path" || status=$?
    touch "$FIXTURE/a-copy-done"
    exit "$status"
fi
''')
            tasks = [subprocess.Popen(["bash", "-c", script],
                                     env=dict(os.environ, FIXTURE=temp, ROLE=role),
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                     for role in ("A", "B")]
            try:
                results = [task.communicate(timeout=5) for task in tasks]
                self.assertEqual(tasks[1].returncode, 0, "".join(results[1]))
                if tasks[0].returncode == 0:
                    import hashlib
                    template = Path((root / "template-path").read_text().strip())
                    self.assertEqual(template, root / f"disk.{hashlib.sha256(payload).hexdigest()}.img")
                    private = Path((root / "private-path").read_text().strip())
                    self.assertNotEqual(private, template)
                    self.assertEqual(private.stat().st_size, 128 * 1024 * 1024)
                    with private.open("rb") as copied:
                        self.assertEqual(copied.read(len(payload)), payload)
                else:
                    self.assertIn("cannot stat", results[0][1])
                return tasks[0].returncode
            finally:
                for task in tasks:
                    if task.poll() is None:
                        task.kill()
                        task.communicate()

    def test_competing_publication_keeps_the_private_disk_acquirable(self):
        self.assertEqual(self.contested_copy(), 0)
        self.assertNotEqual(self.contested_copy(unlink_before_publish=True), 0)

    def caller_result(self, architecture, unchecked_substitution=False, acquisition_succeeds=False, discard_template=False):
        import re
        function = self.production_function("qemu_run_" + architecture)
        block = re.search(r'^\tif [^\n]*qemu_prepare_system_disk [^\n]*; then\n.*?^\tfi\n', function, re.M | re.S)
        self.assertIsNotNone(block, architecture)
        block = block.group(0)
        if discard_template:
            block, replacements = re.subn(r'virtio_disk="\$\((qemu_prepare_system_disk [^\n]*?)\)"', r'\1', block)
            self.assertEqual(replacements, 1)
        if unchecked_substitution:
            # Explicit controller fixtures attach through `qemu_attach_system_disk`; ordinary ports retain
            # virtio-blk. Neither attachment may run after private acquisition fails.
            attach = re.search(r'^\t{2,}qemu_attach_(?:virtio_blk|system_disk) qemu_args "\$run_disk"[^\n]*\n', block, re.M)
            self.assertIsNotNone(attach, architecture)
            attach = attach.group(0).replace('"$run_disk"', '"$(qemu_run_disk "$virtio_disk")"')
            block = block.splitlines(keepends=True)[0] + attach + '\tfi\n'
        # Execute the actual caller block with acquisition failing. A helper returning failure
        # inside an unchecked argument substitution does not make the attachment call fail.
        script = r'''set -euo pipefail
qemu_prepare_system_disk() { printf '%s\n' 'keyed template.img'; }
qemu_run_disk() {
    if [[ "$1" != 'keyed template.img' ]]; then
        echo "fixture: copied wrong template" >&2
        return 1
    fi
    if [[ "$COPY_SUCCEEDS" == 1 ]]; then printf '%s\n' 'private disk.img'; return 0; fi
    echo "fixture: private copy failed" >&2
    return 1
}
qemu_attach_virtio_blk() { printf 'ATTACHED <%s>\n' "$2"; }
qemu_attach_system_disk() { printf 'ATTACHED <%s>\n' "$2"; }
# The persistent-disk choice in front of the copy: with no `RUN_DISK` it IS the private copy, which is the
# acquisition under test, and with no paired volume beside the image there is nothing else to copy from.
qemu_run_system_disk() { qemu_run_disk "$1"; }
QEMU_BUILD_DIR=/nonexistent
caller() {
    local volume_image=volume volume_pkg=volume virtio_disk=disk virtio_opts="" dma_fixture=0
''' + block + '    echo "CONTINUED"\n}\ncaller\n'
        return subprocess.run(["bash", "-c", script], env=dict(os.environ, COPY_SUCCEEDS=str(int(acquisition_succeeds))),
                              capture_output=True, text=True, timeout=5)

    def test_explicit_port_controller_pairs_the_signed_manifest_to_its_volume(self):
        pairing_function = self.production_function("qemu_port_volume_pairing")
        manifest_function = self.production_function("stage_signed_boot_manifest")
        with tempfile.TemporaryDirectory() as temp:
            work = Path(temp)
            kernel = work / "kernel"
            kernel.write_bytes(b"the staged kernel")
            wanted = "00112233445566778899aabbccddeeff"
            environment = dict(os.environ, QEMU_BUILD_DIR=str(work), REPO_ROOT=str(ROOT),
                               HERE=str(ROOT / "src/harness"), STAGED_KERNEL=str(kernel),
                               TRACE=str(work / "arguments"))
            environment.pop("SYSTEM_DISK", None)
            script = "set -euo pipefail\n" + pairing_function + manifest_function + r'''
cargo() { printf '%s\n' "$@" >"$TRACE"; touch "${@: -1}"; }
mcopy() { :; }
stage_signed_boot_manifest unused /no-bootstrap "$FIXTURE_ARCH"
'''
            for arch in ("aarch64", "riscv64"):
                environment["FIXTURE_ARCH"] = arch
                volume = work / f"system-volume-bootable-{arch}.img"
                uuid = volume.with_suffix(".uuid")

                def run(controller=None):
                    env = environment.copy()
                    if controller is not None:
                        env["SYSTEM_DISK"] = controller
                    (work / "arguments").unlink(missing_ok=True)
                    return subprocess.run(["bash", "-c", script], env=env,
                                          text=True, capture_output=True, timeout=5)

                ordinary = run()
                self.assertEqual(ordinary.returncode, 0, ordinary.stderr)
                arguments = (work / "arguments").read_text().splitlines()
                self.assertEqual(arguments[arguments.index("--volume-uuid") + 1], "0" * 32)
                missing = run("nvme")
                self.assertNotEqual(missing.returncode, 0)
                self.assertFalse((work / "arguments").exists())
                volume.write_bytes(bytes(80) + bytes.fromhex(wanted) + bytes(32))
                uuid.write_text(wanted + "\n")
                for controller in ("virtio", "nvme", "ahci", "virtio-scsi"):
                    with self.subTest(arch=arch, controller=controller):
                        paired = run(controller)
                        self.assertEqual(paired.returncode, 0, paired.stderr)
                        arguments = (work / "arguments").read_text().splitlines()
                        self.assertEqual(arguments[arguments.index("--volume-uuid") + 1], wanted)
                uuid.write_text("11" * 16)
                mismatch = run("ahci")
                self.assertNotEqual(mismatch.returncode, 0)
                self.assertIn("do not agree", mismatch.stderr)
                self.assertFalse((work / "arguments").exists())
                # Even a leftover broken fixture must not change an ordinary port boot.
                ordinary = run()
                self.assertEqual(ordinary.returncode, 0, ordinary.stderr)
                arguments = (work / "arguments").read_text().splitlines()
                self.assertEqual(arguments[arguments.index("--volume-uuid") + 1], "0" * 32)

    def test_every_architecture_refuses_failed_private_disk_acquisition(self):
        for architecture in ("x86_64", "aarch64", "riscv64"):
            with self.subTest(architecture=architecture):
                success = self.caller_result(architecture, acquisition_succeeds=True)
                self.assertEqual(success.returncode, 0, success.stdout + success.stderr)
                self.assertEqual(success.stdout, "ATTACHED <private disk.img>\nCONTINUED\n")
                result = self.caller_result(architecture)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("fixture: private copy failed", result.stderr)
                self.assertNotIn("ATTACHED", result.stdout)
                self.assertNotIn("CONTINUED", result.stdout)
                mutant = self.caller_result(architecture, unchecked_substitution=True)
                self.assertEqual(mutant.returncode, 0, mutant.stdout + mutant.stderr)
                self.assertIn("ATTACHED", mutant.stdout)
                self.assertIn("CONTINUED", mutant.stdout)
                wrong_template = self.caller_result(architecture, discard_template=True, acquisition_succeeds=True)
                self.assertEqual(wrong_template.returncode, 1, wrong_template.stdout + wrong_template.stderr)
                self.assertIn("fixture: copied wrong template", wrong_template.stderr)
                self.assertNotIn("ATTACHED", wrong_template.stdout)
                self.assertNotIn("CONTINUED", wrong_template.stdout)


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
            # AND WHAT THE IMAGE BUILDER SNAPSHOTS. `mkimage.sh` copies the services manifest into
            # the run's own snapshot of its inputs, so a sandbox without it cannot assemble an image
            # at all - and the failure surfaces as "the guest produced no verdict", which reads as
            # the gate's subject rather than as a missing file.
            manifest = root / "src/user/services/manifest.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text((ROOT / "src/user/services/manifest.toml").read_text())
            stager = tools / "stage-kernel.sh"
            stager.write_text((HERE / "stage-kernel.sh").read_text())
            stager.chmod(0o755)
            (tools / "volume-pairing.sh").write_text((HERE / "volume-pairing.sh").read_text())
            # THE FIXTURE COPIES WHAT THE SCRIPT SOURCES, and `check-perf-anchor.sh` grew an
            # `evidence.sh` of its own. A sandbox missing one of a script's sources does not test the
            # script, it tests the sandbox - and the failure reads as the gate's subject rather than
            # as the copy list, which is how this one stayed red.
            (tools / "evidence.sh").write_text((HERE / "evidence.sh").read_text())
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
                # THE LINE IS MATCHED AS THE PRODUCER WRITES IT TODAY, and the producer grew a
                # DMA-mode suffix. A negative control that patches a line the file no longer contains
                # is a control that proves nothing, so the count is asserted rather than assumed -
                # which is what caught this.
                selected = 'output="${LIBER_IMAGE_OUTPUT:-$BUILD/$SLUG$DMA_SUFFIX.iso}"'
                self.assertEqual(maker.count(selected), 1)
                maker = maker.replace(selected, 'output="$BUILD/$SLUG$DMA_SUFFIX.iso"')
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
            # AND THE MODE THE VOLUME WAS SIGNED FOR, which `mkimage.sh` now requires to MATCH the
            # one the medium is being signed for - a volume and a medium that disagree about DMA
            # isolation is exactly what that check exists to refuse, and a fixture without the
            # sidecar cannot get past it to reach the subject of this gate.
            (boot / "system-volume-bootable-x86_64.dma-mode").write_text("enforcing-required")
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


class GuestGateImagePreparation(unittest.TestCase):
    """Execute the real shared gate boundary; only producers and guest launch are fakes."""

    def exercise(self, *, arch="x86_64", pinned=False, missing_pin=False, build_fails=False,
                 disk=None, mutate_disk=False):
        import json
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "tree with spaces"
            tools = root / "src/tools"
            harness = root / "src/harness"
            binaries = root / "bin"
            temporary = root / "private tmp"
            boot = root / ".build/boot"
            for path in (tools, harness, binaries, temporary, boot):
                path.mkdir(parents=True, exist_ok=True)
            (tools / "guest-gate.sh").write_text((HERE / "guest-gate.sh").read_text())
            for name in ("libersystem.iso", "libersystem-dev.iso"):
                (boot / name).write_text("stale public artifact")
            (boot / "system-volume-bootable-x86_64.img").write_text("unrelated globally replaced volume")
            image = root / "pinned image.iso"
            if pinned and not missing_pin:
                image.write_text("selected pinned volume")
            persistent = root / "persistent disk.img"
            if disk == "existing":
                persistent.write_text("preexisting state")

            def executable(path, text):
                path.write_text(text)
                path.chmod(0o755)

            executable(root / "image.sh", '''#!/usr/bin/env python3
import json,os,pathlib,sys
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
with (root/'build.jsonl').open('a') as out:
    out.write(json.dumps({'args':sys.argv[1:],'output':os.environ['LIBER_IMAGE_OUTPUT'],
                         'development':os.environ.get('LIBER_DEVELOPMENT'),
                         'rustflags':os.environ.get('RUSTFLAGS')})+'\\n')
if os.environ.get('FIXTURE_BUILD_FAIL') == '1':
    raise SystemExit(19)
image=pathlib.Path(os.environ['LIBER_IMAGE_OUTPUT'])
image.write_text('fresh paired volume')
pathlib.Path(str(image)+'.build-key').write_text('current inputs')
pathlib.Path(str(image)+'.build-digest').write_text('current output')
''')
            executable(root / "run.sh", '''#!/usr/bin/env python3
import json,os,pathlib,sys
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
args=sys.argv[1:]
image=pathlib.Path(args[args.index('--image')+1]) if '--image' in args else None
disk=pathlib.Path(os.environ['RUN_DISK']) if os.environ.get('RUN_DISK') else None
row={'args':args,'image_bytes':image.read_text() if image else None,
     'receipts':all(pathlib.Path(str(image)+suffix).is_file() for suffix in ('.build-key','.build-digest')) if image else False,
     'disk_bytes':disk.read_text() if disk and disk.exists() else None,
     'development':os.environ.get('LIBER_DEVELOPMENT'),'rustflags':os.environ.get('RUSTFLAGS')}
with (root/'runs.jsonl').open('a') as out: out.write(json.dumps(row)+'\\n')
''')
            executable(harness / "guest-console.py", '''#!/usr/bin/env python3
import pathlib,sys
args=sys.argv[1:]
pathlib.Path(args[args.index('--log')+1]).write_text('fixture: PASS\\n')
''')
            executable(binaries / "xorriso", '''#!/usr/bin/env python3
import pathlib,sys
args=sys.argv[1:]
assert args[:2] == ['-osirrox','on']
assert args[args.index('-extract')+1] == '/boot/efiboot.img'
pathlib.Path(args[-1]).write_bytes(pathlib.Path(args[args.index('-indev')+1]).read_bytes())
''')
            executable(binaries / "mcopy", '''#!/usr/bin/env python3
import pathlib,sys
args=sys.argv[1:]
assert args[0] == '-i' and args[2] == '::/system-volume.img'
pathlib.Path(args[3]).write_bytes(pathlib.Path(args[1]).read_bytes())
''')
            env = dict(os.environ, PATH=f"{binaries}:{os.environ['PATH']}",
                       FIXTURE_ROOT=str(root), FIXTURE_BUILD_FAIL=str(int(build_fails)),
                       TMPDIR=str(temporary), LIBER_DEVELOPMENT="1", RUSTFLAGS="--cfg fixture_flag")
            for name in ("BOOT_IMAGE", "RUN_DISK", "LIBER_IMAGE_OUTPUT"):
                env.pop(name, None)
            if pinned:
                env["BOOT_IMAGE"] = str(image)
            if disk:
                env["RUN_DISK"] = str(persistent)
            script = r'''
set -euo pipefail
root="$1/src"
source "$root/tools/guest-gate.sh"
guest_gate_arch --arch "$2"
guest_gate_run 'probe first' fixture
if [[ "$3" == 1 ]]; then printf '%s' 'state written by first guest' >"$RUN_DISK"; fi
guest_gate_run 'probe second' fixture
'''
            result = subprocess.run(["bash", "-c", script, "fixture", str(root), arch,
                                     str(int(mutate_disk))], cwd=root, env=env,
                                    text=True, capture_output=True, timeout=10)
            def records(name):
                path = root / name
                return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
            builds, runs = records("build.jsonl"), records("runs.jsonl")
            self.assertFalse(list(temporary.iterdir()), "owned image, receipts and temporary directories must be cleaned")
            self.assertEqual((boot / "libersystem.iso").read_text(), "stale public artifact")
            self.assertEqual((boot / "libersystem-dev.iso").read_text(), "stale public artifact")
            return result, builds, runs

    def test_stale_defaults_are_ignored_and_one_private_image_serves_both_boots(self):
        result, builds, runs = self.exercise()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(builds), 1)
        self.assertEqual(builds[0]["args"], ["--format", "iso", "--dma-mode", "harness"])
        self.assertEqual(len(runs), 2)
        self.assertEqual(runs[0]["args"], runs[1]["args"])
        self.assertIn("private tmp", runs[0]["args"][-1])
        for run in runs:
            self.assertEqual(run["image_bytes"], "fresh paired volume")
            self.assertTrue(run["receipts"])
            self.assertEqual(run["development"], "1")
            self.assertEqual(run["rustflags"], "--cfg fixture_flag")
        self.assertEqual(builds[0]["development"], "1")
        self.assertEqual(builds[0]["rustflags"], "--cfg fixture_flag")

    def test_explicit_pin_with_spaces_avoids_build_and_remains_authoritative(self):
        result, builds, runs = self.exercise(pinned=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(builds, [])
        self.assertEqual(len(runs), 2)
        self.assertTrue(all(run["image_bytes"] == "selected pinned volume" for run in runs))
        self.assertTrue(all(run["args"][-1].endswith("pinned image.iso") for run in runs))

    def test_failed_preparation_prevents_any_guest_launch_and_cleans_owned_files(self):
        result, builds, runs = self.exercise(build_fails=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(builds), 1)
        self.assertEqual(runs, [])
        self.assertIn("current x86 gate image did not build", result.stderr)

    def test_missing_explicit_pin_neither_builds_nor_launches(self):
        result, builds, runs = self.exercise(pinned=True, missing_pin=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((builds, runs), ([], []))

    def test_port_launches_keep_existing_arguments_and_do_not_prepare_an_iso(self):
        for arch in ("aarch64", "riscv64"):
            with self.subTest(arch=arch):
                result, builds, runs = self.exercise(arch=arch, pinned=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(builds, [])
                self.assertEqual([run["args"] for run in runs], [["--arch", arch, "--smp", "2"]] * 2)

    def test_existing_persistent_disk_is_not_replaced_by_preparation(self):
        result, builds, runs = self.exercise(disk="existing", mutate_disk=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(builds), 1)
        self.assertEqual([run["disk_bytes"] for run in runs], ["preexisting state", "state written by first guest"])

    def test_new_disk_comes_from_exact_selected_iso_and_survives_the_second_boot(self):
        for pinned in (False, True):
            with self.subTest(pinned=pinned):
                result, builds, runs = self.exercise(pinned=pinned, disk="missing", mutate_disk=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(len(builds), 0 if pinned else 1)
                self.assertEqual(runs[0]["disk_bytes"], "selected pinned volume" if pinned else "fresh paired volume")
                self.assertEqual(runs[1]["disk_bytes"], "state written by first guest")


class KernelBuildOnly(unittest.TestCase):
    def exercise(self, remove_return):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            harness = root / "src/harness"
            harness.mkdir(parents=True)
            (root / "src/kernel").mkdir()
            (root / "bin").mkdir()
            # WHAT THE SCRIPT SOURCES, which `test-kernel.sh` grew: the evidence machinery. Without
            # it the script dies at the `source` line - before the cleanup trap is installed, which
            # is how this fixture came to report a leaked staged kernel rather than a missing file.
            tools = root / "src/tools"
            tools.mkdir(parents=True)
            (tools / "evidence.sh").write_text((HERE / "evidence.sh").read_text())
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


class ThreeDimensionalGatePhases(unittest.TestCase):
    """Execute the real report/verdict loop; the guest command alone is a bounded stand-in."""

    def run_phase(self, phase, bad=None):
        source = (ROOT / "src/tools/check-qemu-3d-demo.sh").read_text()
        marker = "failed=0\n"
        self.assertEqual(source.count(marker), 1)
        measurement = source[source.index(marker):]
        with tempfile.TemporaryDirectory(prefix="3d phases ") as temporary:
            root = Path(temporary)
            frames = root / "frames"
            frames.mkdir()
            guest = root / "lab.sh"
            guest.write_text("""#!/usr/bin/env python3
import json,os,pathlib,re,sys
command=sys.argv[-1]
phase='extended' if '--postprocess' in command else 'core'
width=int(re.search(r'--width (\\d+)',command)[1])
height=int(re.search(r'--height (\\d+)',command)[1])
with (pathlib.Path(__file__).parent/'calls.jsonl').open('a') as handle:
    handle.write(json.dumps([phase,width,height])+ '\\n')
bad=os.environ.get('BAD_ROW','')==f'{phase}:{width}'
print('test3d-sw: presented 35 frame(s)')
print('allocation counter observes shared-library preparation')
print('steady samples 30 median_us', 1000000 if phase=='extended' else 33333, 'p99_us', 1000000 if phase=='extended' else 80000, 'render_alloc_max 0 loop_alloc_max 0')
print('steady elapsed_ns', 30000000000 if phase=='extended' else (1000000001 if bad else 1000000000), 'max_us', 1000000 if phase=='extended' else 80000)
print('test3d-sw: colour 1; test3d-sw: present 1; heap live_bytes 1; scene prepared bytes 1; present_max_us 1')
if phase=='extended':
    if not bad:
        print('HDR chain executed six downsamples, five upsamples and resolve')
    print('test3d-sw: postprocess 1; test3d-sw: shadow 1; HDR prepared bytes 1')
""")
            guest.chmod(0o755)
            script = ('set -euo pipefail\n'
                      'die() { printf "%s\\n" "$*" >&2; exit 1; }\n'
                      'note() { printf "%s\\n" "$*"; }\n' + measurement)
            result = subprocess.run(["bash", "-c", script], env={**os.environ,
                "phase": phase, "REPO_ROOT": str(root), "FRAMES_DIR": str(frames),
                "PROOF_DIR": str(frames), "BAD_ROW": bad or ""},
                capture_output=True, text=True, timeout=10)
            import json
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
            logs = {path.name: path.read_text() for path in frames.iterdir()}
            return result, calls, logs

    def test_core_has_five_rows_and_cannot_run_extended(self):
        result, calls, logs = self.run_phase("core", "extended:640")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(calls, [["core", 320, 240], ["core", 800, 600]] + [["core", 640, 480]] * 3)
        self.assertEqual(len(logs), 5)

    def test_extended_has_three_rows_and_no_core_floor(self):
        result, calls, logs = self.run_phase("extended", "core:640")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(calls, [["extended", 320, 240], ["extended", 800, 600], ["extended", 640, 480]])
        self.assertEqual(len(logs), 3)

    def test_core_missed_full_window_keeps_all_five_proofs(self):
        result, calls, logs = self.run_phase("core", "core:640")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stable 30 FPS floor missed", result.stderr)
        self.assertEqual(len(calls), 5)
        self.assertEqual(len(logs), 5)
        self.assertTrue(all("presented 35 frame(s)" in log for log in logs.values()))

    def test_extended_failure_keeps_all_three_proofs(self):
        result, calls, logs = self.run_phase("extended", "extended:320")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 3)
        self.assertEqual(len(logs), 3)
        self.assertTrue(all("presented 35 frame(s)" in log for log in logs.values()))

    def test_registered_wrapper_selects_only_extended(self):
        wrapper = (ROOT / "src/tools/check-qemu-3d-extended.sh").read_text()
        self.assertIn('check-qemu-3d-demo.sh" --extended "$@"', wrapper)
        source = (ROOT / "src/tools/check-qemu-3d-demo.sh").read_text()
        self.assertIn('[[ "$#" == 1 && "$1" == --extended ]]', source)
        self.assertIn('if [[ "$phase" == core ]]; then\n', source)
        self.assertNotIn('for phase in core extended', source)


class ThreeDimensionalResizePixels(unittest.TestCase):
    """Independent ray-pattern fixtures for the native aspect oracle, without a guest."""

    @staticmethod
    def frame(path, width, height, fixed_aspect=None, shifted=0, flat=False):
        pixels = bytearray()
        for y in range(height):
            v = y / height
            for x in range(width):
                ray = ((x / width - .5) * fixed_aspect if fixed_aspect else (x - width / 2) / height) + shifted
                if flat:
                    pixel = (30, 35, 40)
                elif abs(ray) < .17 and .27 < v < .62:
                    pixel = (int(110 + (ray + .2) * 200), int(60 + v * 100), int(70 + (ray + .2) * 120))
                elif v > .60:
                    checker = (int((ray + .5) * 18) + int(v * 19)) % 2
                    pixel = (90 + checker * 70, int(80 + v * 70), int(45 + (ray + .5) * 130))
                else:
                    pixel = (15, 20, 35)
                pixels.extend(pixel)
        path.write_bytes(f"P6\n{width} {height}\n255\n".encode() + pixels)
        spec = importlib.util.spec_from_file_location("resize_pixels", HERE / "check-3d-demo-frames.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module, module.Frame(path)

    def compare(self, **options):
        with tempfile.TemporaryDirectory(prefix="resize pixels ") as temporary:
            root = Path(temporary)
            module, first = self.frame(root / "desktop.ppm", 512, 384, flat=options.get("flat", False))
            _, second = self.frame(root / "portrait.ppm", 240, 400, **options)
            problems = []
            module.check_resize(first, second, 240, 400, problems)
            return problems

    def test_fixed_vertical_fov_survives_changed_native_aspect(self):
        self.assertEqual(self.compare(), [])

    def test_stretching_a_retained_four_by_three_scene_fails(self):
        self.assertTrue(self.compare(fixed_aspect=4 / 3))

    def test_changed_camera_state_fails(self):
        self.assertTrue(self.compare(shifted=.15))

    def test_flat_frame_is_not_resize_evidence(self):
        self.assertTrue(self.compare(flat=True))

    def test_old_scanout_dimensions_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            module, frame = self.frame(Path(temporary) / "old.ppm", 512, 384)
            problems = []
            module.check_resize(frame, frame, 240, 400, problems)
            self.assertTrue(problems)



class ThreeDimensionalNativeResizeVerdict(unittest.TestCase):
    """The complete native verdict must accept portrait projection and reject missing content."""

    @staticmethod
    def frame(path, width, height, *, flat=False, object_missing=False, panel_missing=False, stretched=False):
        pixels = bytearray()
        for y in range(height):
            v = y / height
            for x in range(width):
                u = x / width
                ray = (u - .5) * (4 / 3 if stretched else width / height)
                pixel = (15, 20, 35)
                if v > .60:
                    checker = (int((ray + 1) * 18) + int(v * 19)) % 2
                    shade = 70 + checker * 65 + int(v * 35)
                    pixel = (shade, shade + 3, shade + 20)
                if not object_missing and abs(ray) < .28 and .27 < v < .65:
                    pixel = (int(110 + (ray + .3) * 180), int(90 + v * 80), int(80 + (ray + .3) * 140))
                if not panel_missing and -.24 < ray < -.015 and .45 < v < .96:
                    pixel = tuple((a + b) // 2 for a, b in zip(pixel, (20, 180, 245)))
                if .04 < u < .43 and .03 < v < .19:
                    pixel = (115, 130, 170)
                    if .085 < u < .31 and .10 <= v <= .15 and int(u * 100) % 4 < 2:
                        pixel = (20, 25, 40)
                if flat:
                    pixel = (30, 35, 40)
                pixels.extend(pixel)
        path.write_bytes(f"P6\n{width} {height}\n255\n".encode() + pixels)

    def verdict(self, *, reference_options=None, **current_options):
        from contextlib import redirect_stdout
        from io import StringIO
        spec = importlib.util.spec_from_file_location("native_resize_verdict", HERE / "check-3d-demo-frames.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory(prefix="native aspect verdict ") as temporary:
            root = Path(temporary)
            reference, current = root / "reference.ppm", root / "portrait.ppm"
            self.frame(reference, 512, 384, **(reference_options or {}))
            self.frame(current, 240, 400, **current_options)
            output = StringIO()
            with redirect_stdout(output):
                code = module.main(["--resize", "240", "400", str(reference), str(current)])
            return code, output.getvalue()

    def test_native_portrait_with_a_wide_projected_object_passes_the_complete_verdict(self):
        code, output = self.verdict()
        self.assertEqual(code, 0, output)
        self.assertIn("resize preserves the static scene, camera projection and HUD", output)

    def test_blank_current_cannot_pass_as_a_resized_scene(self):
        code, output = self.verdict(flat=True)
        self.assertNotEqual(code, 0)
        self.assertIn("cleared surface rather than a scene", output)

    def test_object_removed_from_current_fails_camera_correspondence(self):
        code, output = self.verdict(object_missing=True)
        self.assertNotEqual(code, 0)
        self.assertIn("corresponding camera rays match after resize", output)

    def test_stretching_a_retained_scene_fails_camera_correspondence(self):
        code, output = self.verdict(stretched=True)
        self.assertNotEqual(code, 0)
        self.assertIn("corresponding camera rays match after resize", output)

    def test_blank_reference_cannot_supply_the_object_proof(self):
        code, output = self.verdict(reference_options=dict(flat=True))
        self.assertNotEqual(code, 0)
        self.assertIn("cleared surface rather than a scene", output)

    def test_object_absent_from_reference_cannot_supply_the_object_proof(self):
        code, output = self.verdict(reference_options=dict(object_missing=True, panel_missing=True))
        self.assertNotEqual(code, 0)
        self.assertIn("nothing stands in front of the horizon", output)


resize_tests_spec = importlib.util.spec_from_file_location("rfb_resize_tests", HERE / "test-3d-demo-resize.py")
resize_tests_module = importlib.util.module_from_spec(resize_tests_spec)
resize_tests_spec.loader.exec_module(resize_tests_module)
RfbResizeTests = resize_tests_module.RfbResizeTests
RfbOwnershipTests = resize_tests_module.RfbOwnershipTests
RfbAdmissionTests = resize_tests_module.RfbAdmissionTests


class ThreeDimensionalResizeOwnership(unittest.TestCase):
    """Real admission/cleanup shell, with the image and lab lifecycle substituted."""

    def exercise(self, running=False, phase="core", boot_fails=False, changed=False, owned_on_failure=True, quit_fails=False, remains_alive=False, live=False, appears_after_probe=False, identity_error=False):
        import json
        source = (HERE / "check-qemu-3d-demo.sh").read_text()
        marker = 'if [[ "$phase" == core ]]; then\n\tpython3 "$RESIZE"'
        self.assertEqual(source.count(marker), 1)
        prefix = source[:source.index(marker)]
        with tempfile.TemporaryDirectory(prefix="3d owned ") as temporary:
            root = Path(temporary)
            (root / "src/tools").mkdir(parents=True)
            (root / "lib.sh").write_text('set -euo pipefail\nREPO_ROOT="$TEST_ROOT"\nnote() { :; }\ndie() { echo "$*" >&2; exit 1; }\n')
            (root / "src/tools/check-3d-demo-package.py").write_text('pass\n')
            (root / "src/tools/3d-demo-resize.py").write_text("""import os,json,sys,pathlib
if '--live-state' in sys.argv:
    if os.environ['IDENTITY_ERROR']=='1': sys.exit(2)
    active=os.environ['EXISTING_LIVE']=='1' or (os.environ['APPEARS_AFTER_PROBE']=='1' and (pathlib.Path(os.environ['TEST_ROOT'])/'probed').exists())
    print(json.dumps({'pid':122,'pgid':122,'started':33} if active else None))
    sys.exit(0 if active else 1)
if '--owner-state' in sys.argv:
    active=(pathlib.Path(os.environ['TEST_ROOT'])/'active').exists()
    print(json.dumps({'pid':123,'pgid':123,'arguments':['private']} if active else None))
    sys.exit(0 if active else 1)
with open(os.environ['CALLS'],'a') as f: f.write(json.dumps(['resize']+sys.argv[1:])+'\\n')
""")
            lab = root / "lab.sh"
            lab.write_text('''#!/usr/bin/env python3
import os,json,sys,pathlib
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as f:
    f.write(json.dumps(args+[os.environ.get('SMP'),os.environ.get('VNC_ADDR')])+'\\n')
if args[:2]==['sh','uname']:
    (pathlib.Path(os.environ['TEST_ROOT'])/'probed').touch()
    sys.exit(0 if os.environ['RUNNING']=='1' else 1)
if args[0]=='boot':
    if os.environ['BOOT_FAILS']!='1' or os.environ['OWNED_ON_FAILURE']=='1':
        (pathlib.Path(os.environ['TEST_ROOT'])/'active').touch()
    sys.exit(1 if os.environ['BOOT_FAILS']=='1' else 0)
if args[0]=='quit':
    if os.environ['REMAINS_ALIVE']!='1':
        (pathlib.Path(os.environ['TEST_ROOT'])/'active').unlink(missing_ok=True)
    sys.exit(1 if os.environ['QUIT_FAILS']=='1' else 0)
if args==['monitor','info cpus']:
    for i in range(32): print(f' CPU #{i}: thread_id={i}')
if args==['monitor','info kvm']: print('kvm support: enabled')
''')
            lab.chmod(0o755)
            gate = root / "src/tools/check-qemu-3d-demo.sh"
            tail = '\n'
            if changed:
                tail += 'ORIGINAL_WIDTH=1280\nORIGINAL_HEIGHT=800\nVNC_CHANGED=1\n'
            tail += 'exit 0\n'
            gate.write_text(prefix + tail)
            env = {**os.environ, "TEST_ROOT": str(root), "CALLS": str(root / "calls"),
                   "RUNNING": str(int(running)), "BOOT_FAILS": str(int(boot_fails)), "SMP": "4",
                   "OWNED_ON_FAILURE": str(int(owned_on_failure)), "QUIT_FAILS": str(int(quit_fails)),
                   "REMAINS_ALIVE": str(int(remains_alive)), "EXISTING_LIVE": str(int(live)),
                   "APPEARS_AFTER_PROBE": str(int(appears_after_probe)), "IDENTITY_ERROR": str(int(identity_error))}
            result = subprocess.run(["bash", str(gate)] + (["--extended"] if phase == "extended" else []),
                                    env=env, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()] if (root / "calls").exists() else []
            for call in calls:
                if call[0] == "boot" and call[-1]:
                    directory = Path(call[-1].removeprefix("unix:")).parent
                    self.assertEqual(directory.exists(), remains_alive, "private endpoint must remain only while its owned guest survives")
                    if directory.exists():
                        import shutil
                        shutil.rmtree(directory)  # Fixture-only teardown; no real guest was started.
            return result, calls

    def test_foreign_core_guest_is_refused_without_resize_or_shutdown(self):
        result, calls = self.exercise(running=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([call[0] for call in calls], ["sh"])

    def test_live_busy_core_guest_is_refused_before_shell_or_boot(self):
        result, calls = self.exercise(live=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("existing live lab guest was left unchanged", result.stderr)
        self.assertEqual(calls, [])

    def test_live_busy_extended_guest_is_not_replaced_after_failed_shell(self):
        result, calls = self.exercise(phase="extended", live=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([call[0] for call in calls], ["sh"])

    def test_core_rechecks_live_identity_after_failed_shell(self):
        result, calls = self.exercise(appears_after_probe=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([call[0] for call in calls], ["sh"])

    def test_unreadable_identity_cannot_authorize_boot(self):
        result, calls = self.exercise(identity_error=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to boot", result.stderr)
        self.assertEqual(calls, [])

    def test_owned_core_uses_private_vnc_and_only_its_shutdown(self):
        result, calls = self.exercise()
        self.assertEqual(result.returncode, 0, result.stderr)
        boot = next(call for call in calls if call[0] == "boot")
        self.assertEqual(boot[1:3], ["--vnc", "32"])
        self.assertTrue(boot[-1].startswith("unix:/tmp/liber-3d-vnc."))
        self.assertEqual(sum(call[0] == "quit" for call in calls), 1)

    def test_failed_owned_boot_still_cleans_up(self):
        result, calls = self.exercise(boot_fails=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sum(call[0] == "quit" for call in calls), 1)

    def test_failed_boot_without_ownership_never_quits(self):
        result, calls = self.exercise(boot_fails=True, owned_on_failure=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[0] in ("quit", "resize") for call in calls))

    def test_failed_quit_turns_successful_body_into_failure(self):
        result, _ = self.exercise(quit_fails=True)
        self.assertNotEqual(result.returncode, 0)

    def test_surviving_owned_guest_fails_and_keeps_private_endpoint(self):
        result, _ = self.exercise(remains_alive=True)
        self.assertNotEqual(result.returncode, 0)

    def test_changed_output_is_restored_before_owned_shutdown(self):
        result, calls = self.exercise(changed=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        resize_index = next(i for i, call in enumerate(calls) if call[0] == "resize")
        quit_index = next(i for i, call in enumerate(calls) if call[0] == "quit")
        self.assertLess(resize_index, quit_index)
        self.assertEqual(calls[resize_index][-6:], ["--width", "1280", "--height", "800", "--timeout", "10"])

    def test_extended_can_reuse_checked_guest_without_owning_it(self):
        result, calls = self.exercise(running=True, phase="extended", live=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call[0] in ("boot", "resize", "quit") for call in calls))


gadget_cleanup_spec = importlib.util.spec_from_file_location("gadget_cleanup_tests", HERE / "test-usb-gadget-cleanup.py")
gadget_cleanup_module = importlib.util.module_from_spec(gadget_cleanup_spec)
gadget_cleanup_spec.loader.exec_module(gadget_cleanup_module)
UsbGadgetCleanupTests = gadget_cleanup_module.UsbGadgetCleanupTests


if __name__ == "__main__":
    unittest.main(verbosity=2)
