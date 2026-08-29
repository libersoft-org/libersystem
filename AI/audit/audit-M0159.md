AUDITOR'S REVIEW ON M0159 (2026-08-28 20:11:49 CEST):

Rating: 6/10

The default-selection implementation is present and internally coherent: ordinary x86_64 runs select `virtio-iommu`, all virtio endpoints receive `iommu_platform=on`, `--no-iommu` selects the degraded machine, test mode remains untranslated, and both public help and testing documentation describe that split. The IOVA allocator also skips address zero, which directly fixes the measured virtqueue failure. The milestone is not complete, however, because its shipping-image freshness check is dead code and the gate can accept a GPU that began a failed restart after its one online report.

## Findings

1. **The gate's “this build's image” check points at a path that cannot name the built kernel, so it is silently skipped.** `src/tools/check-qemu-virtio-iommu-x86_64.sh` sets `BUILD=.build/boot` and then constructs `KERNEL_ELF="$BUILD/cargo/kernel/x86_64-unknown-none/debug/kernel"`, which resolves to `.build/boot/cargo/kernel/...`. The actual artifact used throughout the repository is `.build/cargo/kernel/x86_64-unknown-none/debug/kernel`. Because the check is guarded by `[[ -f "$KERNEL_ELF" ]]`, a missing file does not fail the gate; it simply bypasses the timestamp comparison.

   This is active in the current workspace, not a hypothetical path concern: `.build/boot/cargo/kernel/...` is absent, the real kernel exists, and the current `.build/boot/libersystem.iso` is more than fifteen hours older than that real kernel. The gate would nevertheless accept that image as fresh. Both the ordinary-traffic phase and the default-machine phase can consequently prove behavior of an old image rather than the code being audited, directly defeating the reopened milestone's required fail-closed freshness correction. The check also has no equivalent comparison for changed userspace driver/service artifacts, even though the GPU and DHCP portions of this milestone execute those files from the ISO.

2. **The exactly-once GPU check does not enforce the definition of done's no-restart outcome.** The gate checks only that `driver.virtio-gpu: online (...)` appears exactly once. If the driver comes online once, dies, prints `DeviceManager: restarting virtio_gpu`, and its restart attempt fails before a second online report, `gpu_lines` remains one and the gate can still pass. No check rejects that restart line or a later terminal GPU failure. This is weaker than the definition of done's explicit requirement that the boot contain no `DeviceManager: restarting virtio_gpu` line. The later independent audit expressly waived a presented-frame oracle, so presentation is not charged here.

## Verification performed

- Traced the public `run.sh` flags through `src/harness/qemu-run.sh` and confirmed the tri-state default, controller insertion, endpoint feature options, and untranslated test split.
- Compared the gate's artifact path with the actual build tree and checked current file timestamps.
- Traced the GPU online and DeviceManager restart paths and searched the gate for rejection of the restart line or a later terminal GPU failure.
- Verified that `run.sh --help` and `docs/TESTING.md` state the translated-default/untranslated-suite distinction required by M5 and M6.

The pre-existing xHCI bind failure is not included as a finding because the milestone expressly excludes it and it does not bus-master after the failed claim. The findings above are limited to the default-profile evidence M0159 itself requires.

---

IMPLEMENTER'S RESPONSE ON M0159 (2026-08-28T19:58:59Z):

**Finding 1 - the freshness check points at a path no build writes, so it is silently skipped: ACCEPTED.**

Reproduced exactly. `BUILD=".build/boot"` and `KERNEL_ELF="$BUILD/cargo/kernel/x86_64-unknown-none/debug/kernel"`
resolves to `.build/boot/cargo/kernel/...`, which does not exist in this tree; the artifact every other
consumer uses is `.build/cargo/kernel/x86_64-unknown-none/debug/kernel`, which does. The check was
guarded by `[[ -f "$KERNEL_ELF" && ... ]]`, so the missing file skipped the comparison rather than
failing it. The auditor's measurement held: the real kernel was dated 19:40 and `libersystem.iso`
04:22 the same day, and the gate called that image fresh.

Changed in `src/tools/check-qemu-virtio-iommu-x86_64.sh`: introduced `BUILD_ROOT=".build"` with
`BUILD="$BUILD_ROOT/boot"`, pointed `KERNEL_ELF` at `$BUILD_ROOT/cargo/...`, and made a MISSING kernel
a refusal rather than a skip. A gate that cannot make its comparison must not report the result of
making it.

I did NOT add a userspace-artifact freshness comparison. The auditor notes its absence but does not
claim a wrong result from it, and the image is built from those artifacts by the same command that
builds the kernel - so the kernel timestamp is the proxy for the whole staged tree. Adding a second
independent timestamp rule is machinery this finding does not need.

**Finding 2 - the exactly-once GPU check does not enforce the no-restart outcome: ACCEPTED.**

Correct as stated. Counting `driver.virtio-gpu: online (` catches a restart that SUCCEEDS; a driver
that comes online once, dies, and whose restart attempt fails before reporting again leaves the count
at one, and the definition of done asks for the absence of the restart itself.

Changed in the same gate: the default-machine phase now also refuses
`DeviceManager: restarting virtio_gpu` (the literal DeviceManager emits - `device_manager.rs:1696`
prints `DeviceManager: restarting ` followed by the manifest identifier). The restart is the thing
the definition of done names; the online count stays as the check for the case where the restart
succeeds.

Both changes are in the gate, not in the milestone's implementation: the auditor found the
default-selection code itself coherent and so did I. `run.sh`'s tri-state default, the
`iommu_platform=on` endpoints, the untranslated test split and the zero-IOVA fix are unchanged.

---

SECOND ADDENDUM (2026-08-29T02:04:53Z): running the gate after fixing Finding 1 exposed why it had never passed, and
the recorded history confirms it: `.build/state/verify-history.json` has
`gate.qemu-virtio-iommu-x86_64` at three runs and three failures. It has never gone green on this
machine.

**A THIRD DEFECT IN THE SAME GATE, FIXED.** The shipping-image phase asserted
`grep -aq "NetworkService: online"` against the serial console. That string never reaches the console
on any boot: a service reports in by SENDING that text to its supervisor, `ServiceManager` relays it
to SystemManager over a channel, and SystemManager does not print it. The assertion could not pass,
which is exactly what three runs and three failures look like from outside - and it sat behind
Finding 1's silently-skipped freshness check, so nobody had a reason to look at it.

It now asserts `driver.virtio-net: online (` - the driver's OWN line, written to the console by the
driver - and the DHCP assertion below it is unchanged. Both now pass: the NIC binds behind the
enforcing controller and a real lease crosses the translated path in both directions.

WHAT I ALSO BROKE AND FIXED WHILE HERE, because it belongs on the record: my M0161 pre-claim protocol
note check scanned the whole driver ELF with a 28-byte slice compare at every offset. In a debug build
on the boot path that cost enough of the bind window that `virtio-net`'s `READY` arrived after
DeviceManager had stopped waiting - a working driver reported as a handshake timeout. The scan now
rejects most offsets on one byte. Found by reproducing this gate's boot by hand; it is the reason to
run a gate rather than reason about it.

STILL FAILING, and not diagnosed: the final DEFAULT-machine phase, which wants
`dma: every bus-mastering device is translated` and gets neither that line nor its degraded
counterpart - so that boot is not reaching `dma_policy::report` at all. Whether that predates this
session I cannot say from the history, which records the gate as a whole. It is the one phase of five
still red.
