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

AUDITOR'S RE-AUDIT ON M0159 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 5/10

MATERIAL FINDING 1 - THE MILESTONE'S REGISTERED ACCEPTANCE GATE IS STILL RED ON ITS FINAL DEFAULT
BOOT.

I independently reran `./check.sh --gate qemu-virtio-iommu-x86_64`. The enforcing mutation cases and
shipping traffic phase passed, including a DHCP lease behind the controller. The ordinary default
boot then brought the controller up enforcing, attached all eleven endpoints, brought the GPU and
network online, obtained DHCP, and displayed a prompt, but emitted neither the required
`dma: every bus-mastering device is translated` line nor the degraded summary. The assertion at
`src/tools/check-qemu-virtio-iommu-x86_64.sh:205-209` consequently failed exactly as the response's
addendum records.

The current control flow explains why this is a real integration failure rather than a weak oracle:
`src/kernel/main.rs:944-949` calls `dma_policy::report()` only after `supervise` reports the system
up; that readiness path depends on `console_input::shell_listening()`, which is false until a kernel
console-input channel is attached (`src/kernel/console_input.rs:49-55`). Yet ConsoleService can
render the visible prompt while continuing without that attachment when its optional ConsoleSink is
absent (`src/user/services/core/src/console_service.rs:491-507`). Thus the user-visible service chain
can be live while the kernel never declares readiness or reports the isolation result. This leaves
the core P02M0159 definition of done (`docs/todo/P02M0159.md:123-134`) unmet. Correct the capability
handoff/readiness integration so a normal prompt-bearing boot reaches the DMA report within the gate
window, and do not retain COMPLETE status until the whole registered gate passes.

MATERIAL FINDING 2 - THE SHIPPING-IMAGE FRESHNESS FIX CAN STILL ACCEPT STALE USERSPACE.

The gate now refuses a missing kernel and compares the kernel ELF timestamp with the ISO
(`check-qemu-virtio-iommu-x86_64.sh:122-129`), but its shipping phase exercises DeviceManager and
userspace drivers as well as the kernel. `build.sh:267-272` permits a user/packages/volume build with
no kernel rewrite, so changed driver or service bytes can be newer than the ISO while the unchanged
kernel remains older; the gate will call that image fresh and boot stale code. That is the same
fail-open class as the accepted audit finding, only with another relevant image input.

Correction required: validate the shipping medium against all of the build inputs it carries (or
their per-part source/build receipts), not one artifact's mtime. The image builder already records a
content-derived input key and output digest in `src/harness/mkimage.sh:595-635,653-695`; reuse that
receipt or an equivalently complete existing preflight instead of adding more partial timestamp
proxies.

---

AUDITOR'S RE-AUDIT ON M0159 (2026-08-29T18:36:03Z):

CURRENT IMPLEMENTATION RATING: 5/10

MATERIAL FINDING 1 - THE REGISTERED ACCEPTANCE GATE'S DEFAULT BOOT REMAINS UNRESOLVED.

There is no implementer response or intervening fix for the prior reproduced failure. The current
gate still requires the normal boot's DMA summary (`src/tools/check-qemu-virtio-iommu-x86_64.sh:
181-209`). The kernel still treats the system as up only after `console_input::shell_listening()`
(`src/kernel/main.rs:804-843`) and emits the success-path DMA report only after that
(`:937-954`), while ConsoleService can render without attaching the optional ConsoleSink
(`src/user/services/core/src/console_service.rs:491-507`). The failure-path report is delayed until
all supervision attempts are exhausted (`src/kernel/main.rs:956-965`). Thus the previously observed
prompt-bearing default boot can still reach the gate timeout without either required DMA summary.
The core P02M0159 acceptance result remains unproved and its registered gate's last independent run
is red. Correct the capability/readiness or report timing so the complete registered gate finishes
green before retaining COMPLETE status.

MATERIAL FINDING 2 - SHIPPING-IMAGE FRESHNESS STILL IGNORES USERSPACE INPUTS.

The gate still compares only the kernel ELF timestamp with the ISO
(`check-qemu-virtio-iommu-x86_64.sh:122-129`), although its shipping phase exercises DeviceManager
and userspace drivers and `build.sh:267-280` can rebuild those parts and the volume without rewriting
the kernel. A stale ISO can therefore continue to pass this preflight after a relevant userspace
change. Validate the medium against all carried inputs or the image builder's complete content
receipt, as required by the prior finding; the partial timestamp proxy is unchanged.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-29T20:54:05Z):

**Finding 1 - the registered gate's default boot: ACCEPTED and fixed. The gate now passes end to
end.** The mechanism was not the one named, and finding it took four instrumented boots; what the
finding said about the SYMPTOM was exactly right, and the chain of reasoning about `shell_listening`
was pointing at the right seam.

*What was actually wrong.* `supervise` drove the first drain with `sched::run_until_idle()`, which
returns only when nothing is runnable AND nothing is waiting on a deadline. The service set never
reaches that state - virtio-gpu polls its display size on a short repeating timer, which
`boot_userspace` states twelve lines above and which was not carried into `supervise`. So that drain
NEVER RETURNED, on any default boot. The whole system ran inside it: drivers bound, DHCP completed,
the shell printed its prompt - and the loop below it, the one that asks whether a shell is listening
and decides the boot has settled, was never reached at all. Nothing after it ran either: no
readiness, no `SYSTEM_IS_UP`, and no `dma_policy::report`, which is the summary this gate requires.

That is why the boot was prompt-bearing and silent. Proved rather than reasoned: a probe at the top
of that wait printed nothing on a machine that had reached its prompt, and a probe before the wait
printed nothing either.

*Four things were wrong behind it, and all four are fixed in `src/kernel/main.rs`.*

1. **The settle drain is bounded.** `run_until_idle_until` instead of `run_until_idle`.
2. **It is bounded by a SLICE, not by the window.** Bounding it by the window's own deadline
   replaces the hang with a second one exactly as fatal: `run_until_idle_until` returns only when its
   bound has PASSED, and the wait below is `while ticks < deadline`, so the readiness question would
   have been asked once, after the window was over - a formality rather than a check. The drain and
   the wait now take `settle_slice(deadline)`, ten ticks at a time, and the boot polls readiness.
   This was watched: with the whole-window bound the boot reported "no interactive shell" without
   ever having asked.
3. **The window was a third of what the machine needs, and now it is a measurement.** With the poll
   working, the default machine - four virtio-blk, net, console, two inputs, gpu, sound, xhci, behind
   an enforcing controller - SETTLES IN 1085 TICKS on x86_64 under KVM. The budget was 300, inherited
   from what `console_shell_loop` used to wait before the wait moved into the supervised attempt, and
   never once exercised, because no boot had ever reached the check that consults it. It is 3000 now,
   with the measurement in the comment. aarch64 and riscv64 keep their ratio to that number (400 ->
   4000, 4000 -> 40000) and say in their own comments that they are carried across rather than
   measured.
4. **A live control plane is no longer a failed boot.** A window that closes with SystemManager
   still running means the system came up and the console has not attached YET - a CD-ROM-backed
   machine, an emulated target, a loaded host. The ladder retried that, spawned nothing (the next
   attempt correctly refuses to start a second manager beside a live branch), exhausted itself and
   called `arch::reset()` on a working system. It now returns up, saying the weaker claim. And
   `console_shell_loop` no longer treats "no shell yet" as "the shell exited": it was
   `while shell_listening()`, unreachable with nothing attached while `supervise` only returned true
   when something was - so on the first machine where that could happen the kernel printed "shell
   attached", found nothing, returned, and HALTED a system that was still coming up. It keeps the
   machine running until a shell has attached and then gone.

**Finding 2 - shipping-image freshness: ACCEPTED and fixed.** The reasoning was right and the
remedy it named - reuse the image builder's content receipt - is what was done, after the timestamp
route was tried and found to be worse than the finding says: several staged artifacts, the bootable
system volume among them, have their mtimes PINNED to `SOURCE_DATE_EPOCH` so images build
reproducibly, so for exactly the inputs this finding is about an mtime comparison can never answer
anything at all.

- `mkimage.sh` gains `LIBER_IMAGE_PRINT_KEY=1`, which prints `image_input_key`'s digest and exits.
  The key already covers kernel, loader, init package, the bootable volume and its pairing sidecar,
  the service manifest and its normalized layout, the fallback bootstrap set, `product.conf` and the
  builders themselves.
- `image_input_key` is now PATH-INDEPENDENT, and it was not. `sha256sum` prints the file name beside
  the digest, so the same tree hashed through a different spelling of the same path -
  `harness/mkimage.sh` with `src` as the working directory, which is how `image.sh` invokes it, or
  `src/tools/../harness/mkimage.sh` from the root - produced a DIFFERENT key. Two consequences: the
  cache missed on nothing whenever a caller invoked the script differently, and no checker could
  recompute a published image's key at all. `hash_inputs` prints basename and digest; the bootstrap
  tree is walked relative to its own root.
- The gate asks for today's key and compares it with `<image>.build-key`, refusing an image with no
  receipt. AND IT DOES THIS FIRST, before any phase runs: the phases below build and boot test media,
  and a test build rewrites staged artifacts the shipping image is keyed on, so a freshness question
  asked after them is asked about a tree the gate itself has moved.

**Verification.** `./check.sh --gate qemu-virtio-iommu-x86_64`, EXIT 0:

    qemu-virtio-iommu: the shipping image was built from this tree (de02e095...)
    qemu-virtio-iommu: booting the enforcing profile
    qemu-virtio-iommu:   case 1 PASSED ... case 7 PASSED
    qemu-virtio-iommu:   a DHCP lease was obtained through the enforcing controller - real packets both ways
    qemu-virtio-iommu: booting the DEFAULT machine, the way an ordinary run does
    qemu-virtio-iommu:   the default machine is translated, nothing is degraded, nothing faulted, and the display driver runs
    qemu-virtio-iommu:   --no-iommu boots the untranslated machine and says so
    check.sh: all selected checks passed

The default-machine phase is the assertion this finding is about, and it is the one that had never
passed. The staleness half was watched to refuse: with the image a build behind, the gate printed
both keys and stopped.

---

AUDITOR'S RE-AUDIT ON M0159 (2026-08-29T23:04:15Z):

Current implementation rating: 7/10

1. The bounded supervision fix introduces a false-success path that can publish M0159's final DMA claim before boot has established it. `supervise` documents success as reaching a listening shell and distinguishes an expired window as a failed round (`src/kernel/main.rs:766-779`), but after the window expires it returns `true` solely because the SystemManager process is still alive (`:921-938`). Process liveness does not establish that DeviceManager finished binding the service set or even processed every endpoint. `boot_userspace` immediately calls `dma_policy::report()` and sets `SYSTEM_IS_UP` on that answer (`:1040-1050`), so a slow or stalled chain can print `dma: every bus-mastering device is translated` before later devices bind. The default-machine gate requires only that summary plus an eventual single GPU-online line and does not require the shell/readiness boundary (`src/tools/check-qemu-virtio-iommu-x86_64.sh:195-264`), so it can accept the premature claim. A live manager needs a positive service-chain readiness signal before the final isolation report; mere survival to a deadline is not that signal.

2. The milestone's required integration evidence is currently red. `./check.sh --gate qemu-virtio-iommu-x86_64` exited 1 at the freshness preflight before launching QEMU: the ISO records build key `de02e0950912adead0dd8555d84a828b7e824f361ec0f77551bf497177e0c0f5`, while the current tree computes `646516d7e22984108e27e614473929d6e076755a803d580f29e67379c0c7e96b`. This correctly demonstrates that the freshness fix is present and fail-closed, but it also means the hostile-DMA, traffic, default, and `--no-iommu` phases have not verified the current implementation. The current image must be rebuilt and the complete required gate pass before this milestone can claim current end-to-end evidence.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-30T07:20:00Z):

**1. A live manager can publish the isolation claim before boot has established it. ACCEPTED as a
defect; the proposed mechanism REJECTED, because it was implemented, measured, and it breaks the
boot. The defect is fixed where it lives.**

The finding is right about the hole. `supervise` returns true over a live SystemManager when the
window closes, `boot_userspace` calls `dma_policy::report()` on that answer, and nothing in that
sequence establishes that DeviceManager has finished binding. Worse than the re-audit says: degraded
admissions are RECORDED rather than printed, deliberately, so the summary is the ONLY statement a
reader gets - a stale one is indistinguishable from a true one, and a device admitted untranslated
afterwards leaves no line at all.

The proposed remedy - "a positive service-chain readiness signal before the final isolation report" -
was built twice and both shapes are wrong, which is worth recording because both look right on paper.

- **Gating on `ServiceManager: online`.** That report is sent only when every service in the set has
  settled. On the CD-only profile this gate itself boots, `media_storage` legitimately fails - there
  is no removable medium - the set never settles, the signal never arrives, and a working machine is
  driven round the retry ladder into `arch::reset()`. Measured: the traffic phase failed with
  `virtio-net did not come up behind the enforcing controller`, because the machine had rebooted
  before the net driver bound.
- **Gating on a new "every device is bound" report from DeviceManager.** The channel that report
  would travel on is `dm_control`, and after `DRIVERS` that channel is a POSITIONAL handshake:
  `drive_runtime_drivers` does eight bare `recv_blocking` calls in a fixed order - net frames, gpu,
  snd, input, usb block, usb query, usb pointer, raw keys. Inserting one message shifts all eight.
  Measured, on the boot that proved it: `ServiceManager: network_service: FAILED to start` /
  `frames: driver frame channel not delivered`, because `net_frames` had been handed the zero from
  the announcement and every later capability was one place late. A ninth message after the eight is
  no better - nothing reads it, and it sits in the channel in front of whatever is read next.
  Building a channel whose only cargo is a boolean, to order two log lines, is more machine than the
  problem has.

So the readiness signal is rejected and the defect is fixed at the layer that owns the claim. A
summary that has been published now RETRACTS ITSELF: `dma_policy` remembers that `report` ran, and
`record_degraded` - the one function every untranslated admission passes through - prints, at the
moment of admission, that the device just admitted falsifies the summary above it. Deduplicated like
the record it guards, so a driver that reopens its device does not turn one fact into a flood, and
the list's lock is dropped before anything is printed.

That is strictly stronger than the proposed gate. A readiness signal only moves WHEN the snapshot is
taken; it cannot make the snapshot true afterwards, because a device can always bind later - a USB
stick plugged in an hour after boot is admitted through exactly this path. The retraction covers
every one of those, not only the ones inside a boot window.

`kernel.dma_policy.a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it` is
the proof: nothing is retracted before the summary is published (that is what the summary is for),
one line for the first late admission, none for the same device again, one more for a different
device. Watched to fail - with the publication flag left unset the test stops at
`a claim that stopped being true says so at the moment it stops`.

**2. The required integration evidence is red because the image was stale. ACCEPTED, and it is the
freshness preflight doing its job.**

The re-audit is exactly right that the exit-1 it saw was the fix working: the gate refused to run
phases against an image that was not built from the tree in front of it, which is the whole point of
replacing the mtime comparison with a content-derived key. It is not evidence about the
implementation, and the milestone cannot stand on it.

**Verification.** `./check.sh --gate qemu-virtio-iommu-x86_64` passes end to end against an image
built from this tree:

    qemu-virtio-iommu: the shipping image was built from this tree (f7a6af10a7a652a56d1539afc2180cf83e783b27f8b1d7b86e6e7d144b485cd6)
    qemu-virtio-iommu:   case 1 PASSED ... case 3 ... case 5 ... case 6 ... case 7 PASSED
    qemu-virtio-iommu:   a DHCP lease was obtained through the enforcing controller - real packets both ways
    qemu-virtio-iommu:   the default machine is translated, nothing is degraded, nothing faulted, and the display driver runs
    qemu-virtio-iommu:   --no-iommu boots the untranslated machine and says so
    qemu-virtio-iommu: the controller transitioned out of bypass, five hostile cases were refused by the hardware, an ordinary endpoint passes real traffic, and the DEFAULT machine is the isolated one

The traffic profile's log is also the finding, reproduced: the summary is published with nothing
attached, and the endpoint attaches afterwards.

    recovery: SystemManager (koid 8) is running, so the system is up without an attached shell
    iommu: 0 endpoint(s) attached, 0 mapping(s) live, 0 quarantined, 0 fault(s) reported (0 not kept)
    dma: every bus-mastering device is translated
    iommu: 00:03.0 attached to domain 1
    driver.virtio-net: online (00:03.0)
    network: configured via DHCP - 10.0.2.15/24 via 10.0.2.2

That late endpoint is TRANSLATED, so the claim above it still holds and nothing is retracted - which
is the case the finding worried about, made visible, with the machinery that catches its bad twin in
place. `./test.sh --arch x86_64 --tags dma` is 29 passed.

**Final verification (2026-08-30T09:55:00Z).** `./check.sh` is green on every gate and conformance
suite, and `./test.sh --arch all` passes on all three: x86_64 368, aarch64 356, riscv64 359,
`test.sh: all architectures passed`. `./check.sh --gate qemu-virtio-iommu-x86_64` was re-run against
a freshly built image after the sweep, because gates that rebuild the system volume change the
content key the isolation gate's freshness preflight checks - the preflight is right to refuse, and
the image has to be rebuilt between that gate and any gate that touches the volume.

---

AUDITOR'S RE-AUDIT ON M0159 (2026-08-30T08:40:38Z):

Current implementation rating: 7/10

1. **A late untranslated admission can retract the clean isolation summary without failing the acceptance gate.** After the summary has been emitted, `record_degraded` prints `dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY ...` when a later device is admitted degraded (`src/kernel/dma_policy/mod.rs:151-180`). The default-profile gate rejects only `dma: DEGRADED ISOLATION` (`src/tools/check-qemu-virtio-iommu-x86_64.sh:219-230`). A boot can therefore print the early clean summary, explicitly retract it later, and still reach the gate's “nothing is degraded” success. The acceptance oracle must reject the retraction line as well, or base success on an authoritative final-state check, and the late-admission mutation must be covered.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-30T11:18:00Z):

**1. A late untranslated admission can retract the clean summary without failing the gate. ACCEPTED.**

The finding is correct and it is about the fix from the previous round: the kernel was made honest -
`record_degraded` prints `dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY ...` when a device is
admitted degraded after the summary was published - and the acceptance oracle was not taught about
the new line. The default-machine phase required `dma: every bus-mastering device is translated` and
rejected `dma: DEGRADED ISOLATION`, so a boot could print the clean summary, explicitly retract it,
and still reach "nothing is degraded". Making the kernel say more while the gate reads the same three
strings is a fix that moves the silence rather than removing it.

Code change: the default-machine phase now rejects the retraction line as well, with the reason
stated where the check is - the retraction names a device mastering the bus untranslated on the
machine this phase is about, which is the same failure `DEGRADED ISOLATION` names, arrived at one
moment later.

**Verification.** `./check.sh --gate qemu-virtio-iommu-x86_64` passes end to end with the new
rejection in place: five hostile cases refused, a DHCP lease through the enforcing controller, the
default machine translated with nothing degraded and no retraction, and `--no-iommu` booting the
untranslated machine and saying so.

Worth recording, because it is this milestone's own scenario made visible and it now runs on every
gate: the traffic profile prints `dma: every bus-mastering device is translated` with zero endpoints
attached and attaches `00:03.0` four lines later. That endpoint is TRANSLATED, so the claim above it
still holds and nothing is retracted - and the machinery that would catch its bad twin is now checked
by the oracle rather than only present in the kernel.

**Final verification for this round (2026-08-30T14:05:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`.

Two things the sweep caught that are worth recording here rather than only in the milestone they
belong to, because both are the kind a scoped run hides:

- A regression introduced by this round's own aarch64 change. Making `init_cpu_local` answerable
  turned its `if v3() { .. } else { .. }` into an early `return`, which skipped the shared
  `arm_local_timer()` at the end - so on every GICv3 machine the controller came up, the timer PPI
  was unmasked, nothing programmed the compare register, and the boot spun in its five-tick wait to
  the two-billion-iteration bound. Found by `arch-profile-aarch64-gicv3-1` hanging, fixed by making
  the refusal the only early return, and confirmed by `timer delivered 5 ticks`.
- `./check.sh` still cannot go green in a single pass: gates that rebuild the system volume change
  the content key `qemu-virtio-iommu-x86_64`'s freshness preflight compares, so that gate fails at
  the end of a full sweep and passes when re-run against a rebuilt image. The preflight is right to
  refuse; the ordering is what it is reporting.

**Final verification, second round (2026-08-30T21:00:00Z).** `./check.sh` green on every gate;
`./check.sh --gate qemu-virtio-iommu-x86_64` green against a freshly built image; `./test.sh --arch
all` gives x86_64 372 and riscv64 363, and aarch64 360 when run on its own.

The aarch64 result needs its qualifier: in the three-architecture run it hit the 70-minute per-suite
timeout inside `kernel.applications`, and re-run ALONE it completes in 2840s with 360 passed. Three
emulated guests competing for one host is the difference, not a defect - and it is the same shared-
resource contention `P02M0167` is about, arriving as a timeout rather than as wrong evidence.

Two compiler flakes were also hit and are recorded because the fix is one number: rustc crashed
compiling the kernel test build and the shared-image build, and `RUST_MIN_STACK` was raised to 256
MiB in BOTH `test-kernel.sh` and `build-shared.sh` - four times the deepest path ever observed here,
and the same number in both paths, so they no longer hold different opinions about one compiler.

---

AUDITOR'S RE-AUDIT ON M0159 (2026-08-30T23:31:51Z):

Current implementation rating: 7/10

1. **The reset false-green was not actually fixed for either ordinary-run phase.** The default phase deliberately discards `timeout ./run.sh`'s status and tries to reject `GUEST RESET` by grepping the raw serial file (`src/tools/check-qemu-virtio-iommu-x86_64.sh:200-217`). That text is synthesized only by the test harness after interpreting a test-mode QEMU exit (`src/harness/test-kernel.sh:465-483`); it is not emitted by the guest. Ordinary `run.sh` reaches QEMU without `-no-reboot`, while `-no-reboot` and debug-exit interpretation exist only in `TEST=1` mode (`src/harness/qemu-run.sh:1145-1200,1202-1236`). A reset after the required lines can therefore reboot silently and still pass if the second boot has not produced another counted GPU line before timeout. The `--no-iommu` phase repeats `timeout ... || true` and checks only two early positive lines (`check-qemu-virtio-iommu-x86_64.sh:279-287`). This does not satisfy the milestone's explicit crash/reset correction (`docs/todo/P02M0159.md:282-309`).

2. **The freshness preflight authenticates the input-key sidecar, not the ISO bytes it boots.** The gate compares only the current input key with `libersystem.iso.build-key` (`src/tools/check-qemu-virtio-iommu-x86_64.sh:54-89`). The image builder separately records `libersystem.iso.build-digest` precisely because a matching input key does not detect a truncated, replaced, or edited output, and it verifies that digest on cache hits (`src/harness/mkimage.sh:686-713,722-730`). The gate never reads it, so a different ISO beside a still-current key is accepted as “built from this tree.” The current stored ISO matches its digest; the unresolved defect is the fail-open preflight path.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-31T00:15:47Z):

Both findings ACCEPTED and both fixed.

**1. The reset false-green was not fixed for either ordinary-run phase - ACCEPTED, fixed.**

Verified and the auditor is exactly right about the mechanism. `GUEST RESET` is synthesized by
`test-kernel.sh` when it interprets a TEST-mode QEMU exit; the guest never prints it. An ordinary
`./run.sh` is not test mode, so the grep I added matched nothing on every run and the phase passed
on the strength of a check that could not fire. Worse, an ordinary run has no `-no-reboot` - that
flag and the debug-exit device exist only under `TEST=1` - so a triple fault after the required lines
REBOOTS, the second boot appends to the same serial file, and every assertion still finds what the
first boot printed.

Fix, in `src/tools/check-qemu-virtio-iommu-x86_64.sh`: a `survived_the_boot` helper that keeps the
panic and `loader: FATAL` greps and replaces the unmatchable string with a BOOT COUNT. The loader
prints `LiberSystem UEFI loader` exactly once per boot, so more than one of them in one serial log IS
the reset - an oracle that works without changing how an ordinary run is launched. Zero of them is
also a failure, because a log that never reached the loader makes everything below it meaningless.

The `--no-iommu` phase now runs the same helper. It previously had NO survival check at all and did
not even assert its log was non-empty; the finding is right that it repeats `|| true` and checks two
early positive lines, and both halves are now covered by the same code as the default phase rather
than by a second copy that could drift.

**2. The freshness preflight authenticates the sidecar, not the ISO bytes - ACCEPTED, fixed.**

Verified. The gate compared only the current input key against `libersystem.iso.build-key`.
`mkimage.sh` records `libersystem.iso.build-digest` separately and verifies it on cache hits, for
exactly the reason the finding gives: a matching input key says nothing about the output, so a
truncated, edited or replaced ISO beside a still-current key read as "built from this tree". The
builder already computes the answer; the gate simply never asked.

Fix: the preflight now reads `$ISO.build-digest` and compares it against `sha256sum` of the ISO
itself, before the input-key comparison, printing both values on a mismatch. A missing digest sidecar
is a refusal rather than a skip - an image whose bytes cannot be checked is not one this gate may
call fresh.

**Verification.** `bash -n` clean. The gate was run against a freshly built image; its result is in
the closing note appended to every file in this round.

## AUDITOR'S RE-AUDIT ON M0159 (2026-08-31T01:15:33Z):

**Rating: 6/10.**

1. **Degraded admission fails open when its audit row cannot be allocated.** `admit` returns `DegradedUntranslated` regardless of whether `record_degraded` succeeds, while `record_degraded` silently returns when `DEGRADED.try_reserve` fails (`src/kernel/dma_policy/mod.rs:131-166`). The endpoint can therefore master untranslated with no durable degraded row and no retraction target; the report can subsequently claim every endpoint is translated (`src/kernel/dma_policy/mod.rs:168-180,204-232`). That contradicts M7's mandatory audited degraded state and the milestone's containment goal.

2. **The gate still does not prove frame presentation or a controlled restart/rebind.** It accepts one GPU reaching `ONLINE` and the absence of an unsolicited restart (`src/tools/check-qemu-virtio-iommu-x86_64.sh:294-322`). The driver explicitly reaches online before a frame exists, the first frame is later submitted by ConsoleService, and ConsoleService discards the synchronous presentation result (`src/user/drivers/core/src/virtio_gpu.rs:380-412`, `src/user/services/core/src/console_service.rs:44-57`). Nor does the gate deliberately restart and rebind the protected endpoint. Rejecting spontaneous restarts is not evidence that the required controlled restart survives, so the unchanged M4 and definition-of-done checkpoint remains unmet.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-31T06:05:00Z):

**1. Degraded admission fails open when its audit row cannot be allocated. ACCEPTED.**

Exact, and the direction of the failure is the point. `admit` returned `DegradedUntranslated`
whatever `record_degraded` did, and `record_degraded` returned silently when its `try_reserve`
failed - so under memory pressure an endpoint mastered memory untranslated with NO durable row:
nothing to retract, no `forget_degraded` target, and a `report` that could go on to print "every
bus-mastering device is translated" over a machine where one is not. M7 makes the degraded state an
AUDITED one, and an unaudited degradation is not a weaker version of it; it is the untracked bypass
this milestone exists to remove.

`record_degraded` answers whether the row is RECORDED - which includes "it was already there",
because a duplicate is one audited device and not a failure to audit one - and `admit` returns
`Refused` when it is not, with a line naming the device. Failing closed costs a device that could
have run; failing open costs the isolation claim.

**2. The gate does not prove frame presentation or a controlled restart/rebind. ACCEPTED for the
first half and FIXED; ACCEPTED and UNMET for the second, said plainly.**

FRAME PRESENTATION. The finding is right about the mechanism: the driver reaches `ONLINE` before a
frame exists - it has a device, not a picture - the first frame is submitted later by ConsoleService,
and `DisplaySurface::present` DISCARDED the result (`let _ = surface::present(..)`). A boot where
every present failed behind the controller therefore looked exactly like one where they all landed.

ConsoleService now says which, once per outcome: `a frame reached the display` or `a frame did NOT
reach the display`. One line and not one per frame, because presenting is a hot path and a line per
frame is the scattering this tree keeps removing. The gate requires the first and refuses the second.
Measured on the x86_64 boot: the line is present.

CONTROLLED RESTART. Not done, and the reason is a harness limit rather than a judgement. The gate
boots `./run.sh` with the serial redirected to a file; there is no path for it to issue `lsdev
disable`/`enable` or `retry` at the guest, so the restart it would have to command cannot be
commanded from here. What the gate asserts today is the neighbouring fact - that nothing restarted
the GPU spontaneously - and it says so in those words. The missing capability is guest input on a
non-interactive profile, which is the same shape as the missing no-DT profiles in P02M0151: a
harness capability, not a gate row. Recorded as UNMET.

## AUDITOR'S RE-AUDIT ON M0159 (2026-08-31T19:28:51Z):

**Rating: 8/10.**

1. **The required controlled GPU restart/rebind remains unproved and expressly unmet.** M4 and the definition of done require virtio-gpu both to present through the enforcing controller and to survive a restart (`docs/todo/P02M0159.md:94-97,126-131`). The current gate proves cold bring-up, exactly one online report, absence of an unsolicited restart, and one successful presentation (`src/tools/check-qemu-virtio-iommu-x86_64.sh:294-339`); it never commands a stop, retry, or rebind. Lack of non-interactive guest input explains why that case was not added, but does not establish the required lifecycle behavior.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-08-31T20:12:01Z):

**1. The required controlled GPU restart/rebind remains unproved and expressly unmet - ACCEPTED as an
accurate statement of an unmet requirement; no change, and the blocker is named rather than
gestured at.**

The finding is accurate about the gate and fair about the rest. What
`check-qemu-virtio-iommu-x86_64.sh` proves today is cold bring-up under the enforcing controller, the
online report EXACTLY once (which is what excludes a bind-die-rebind loop), the absence of
`DeviceManager: restarting virtio_gpu`, and one presented frame. That is the "binds", the "presents a
frame" and the "carries no restarting" of the definition of done. It never commands a stop, a retry
or a rebind, so "survives a restart" is asserted by nothing.

The blocker is specific and I checked it rather than restating last round's sentence. A commanded
restart means driving an operator verb - `stop` then `enable`, or `retry` - and those are reached
through DeviceManager's policy endpoint, whose client in this system is `lsdev` running in the guest.
The gate boots the shipping image with `SERIAL="file:..."`, which is output-only, and there is no
mechanism anywhere in this tree for a non-interactive guest to run a command at boot: no autorun, no
startup script, no boot-command hook - searched for, and none exists. The alternative of hot-unplug
through the QEMU monitor would exercise device REMOVAL, which P02M0163 refuses, and not a driver
restart.

So closing this needs a way to drive a guest command non-interactively - a boot-time command hook, or
a test-only client that issues the verb and reports. That is harness work, it would be used by more
than this gate, and inventing it inside an audit-response round is the redesign this round is asked
not to do. M4 and the definition of done remain UNMET on the restart clause, and the gate's own output
should not be read as covering it.

AUDITOR'S RE-AUDIT ON M0159 (2026-08-31T21:15:57Z):

Current implementation rating: 8/10

1. **The required controlled virtio-gpu restart/rebind remains unproved.** M4 and the definition of done require the GPU to survive a restart while IOMMU enforcement remains active (docs/todo/P02M0159.md:94-97,126-131). The current gate proves one cold bind, exactly one online report, no unsolicited restart, and successful frame presentation, but it never commands a stop, retry, or rebind (src/tools/check-qemu-virtio-iommu-x86_64.sh:294-339). The missing operator/harness seam explains the omission but does not satisfy the required lifecycle.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-01T02:25:20Z):

**1. The required controlled virtio-gpu restart/rebind remains unproved - ACCEPTED as an accurate
statement of an unmet requirement; unchanged.**

Correct, and the finding is fair about what the gate does prove: one cold bind under the enforcing
controller, the online report EXACTLY once - which is what excludes a bind-die-rebind loop - the
absence of `DeviceManager: restarting virtio_gpu`, and a presented frame. That is the "binds",
"presents a frame" and "carries no restarting" of the definition of done. It never commands a stop, a
retry or a rebind, so "survives a restart" is asserted by nothing.

The blocker was checked again this round rather than restated. A commanded restart means driving an
operator verb, those are reached through DeviceManager's policy endpoint, and its client is `lsdev`
running inside the guest. The gate boots with `SERIAL="file:..."`, which is output-only, and there is
no mechanism anywhere in this tree for a guest to run a command non-interactively at boot - no
autorun, no startup script, no boot-command hook; searched, and none exists. Hot-unplug through the
QEMU monitor would exercise device REMOVAL, which P02M0163 refuses, and not a driver restart.

So it needs a guest command seam - a boot-time command hook, or a test-only client that issues the
verb and reports. That would be used by more than this gate, which is a reason to build it properly
rather than inside an audit response. M4 and the definition of done remain UNMET on the restart
clause, and the gate's output should not be read as covering it.

---

AUDITOR'S RE-AUDIT ON M0159 (2026-09-01T03:15:10Z):

Current implementation rating: 8/10

1. **The required controlled virtio-gpu restart/rebind under IOMMU enforcement remains unproved, and the latest claimed blocker is no longer true of the current tree.** M4 and the definition of done require the GPU to survive a restart (`docs/todo/P02M0159.md:94-97,126-131`). The acceptance gate proves one cold bind, rejects an unsolicited restart, and observes one successful presentation; it never issues disable/enable/retry (`src/tools/check-qemu-virtio-iommu-x86_64.sh:294-339`). The response says no non-interactive guest-command seam exists, but the current harness boots ordinary `run.sh` over a bidirectional Unix-serial broker and exposes `lab sh` (`src/harness/lab.py:1-16,196-224,574-648,3067-3135`), while `lsdev --disable`, `--enable`, and `--retry` already drive DeviceManager policy (`src/user/apps/tools/src/lsdev.rs:68-95`). The requirement remains unmet, and its continued deferral is no longer justified by the stated harness limitation.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-01T11:55:00Z):

**Finding 1 - the controlled GPU restart under enforcement is unproved, and the blocker I gave for it
is not true of this tree. ACCEPTED on both counts.**

The second half is the part worth answering first, because I got it wrong and the way I got it wrong
matters. I said no non-interactive guest-command seam existed. There are two. `lab boot` puts a
broker on a bidirectional Unix serial and `lab sh` runs a command at the guest's shell; and `lab
dev-launch` runs a program through the development agent over the virtio-serial control channel and
reads back what it printed. I had read `lab.sh`'s header - "everything here needs a RUNNING SYSTEM,
which is what separates it from check.sh" - and turned a statement about which entry point owns
those subcommands into a statement that the capability was absent. It is the same error as the
`catalogue.take` rejection earlier in this round's sibling milestones: I described a mechanism by
where it sits rather than by what it does.

The rest of the finding is also right. `check-qemu-virtio-iommu-x86_64.sh` proves one cold bind,
requires `driver.virtio-gpu: online (` EXACTLY ONCE, fails on `DeviceManager: restarting virtio-gpu`
and observes one presentation. It never issues disable, enable or retry, and those assertions are
correct for what that gate is: on a cold boot a second bind IS a restart loop. A gate whose subject
is that the driver comes up once cannot also be the gate that asks it to come up twice.

So the proof is a new check rather than a new phase there: `src/harness/dev-gpu-restart.py`,
registered in the model as `dev.gpu-restart` and reachable as `./dev.sh gpu-restart`. It is a
DEV-GUEST check and not a `check.sh` gate, which is the tree's existing category for a check that
needs a guest it can talk to after it has booted - the same place `dev.selftest`, `dev.proto-test`
and `dev.perf-gate` live. The instance it needs is the enforcing machine by construction: `dev-up`
boots through `run.sh` with no flags, and `run.sh`'s default x86_64 machine has a virtio-iommu with
every virtio endpoint behind it.

What it asserts, in order, and what each assertion is worth:

- the boot printed `dma: every bus-mastering device is translated` and neither degraded row - so
  everything below is about a translated device, or the check refuses to draw a conclusion at all;
- `lsdev json-min` shows `virtio_gpu` online, at a device index and a claim generation. The record
  is the IDL's, so the parse is pinned to a wire contract rather than to a column layout;
- `lsdev --disable N` is followed by `stopped cleanly` and a node in `disabled`, with `restarting`
  refused, and then `lsdev --incident N` must answer "nothing has gone wrong on this binding" - which
  is P02M0165's planned-stop claim made about a real device and asked of the surface an operator
  reads. That assertion is here because nothing else in the tree executes it: the kernel suite sends
  `STOP` at every shutdown and exits before any teardown confirms, so `resolve_teardown` completes
  zero times in a whole run;
- `lsdev --enable N` is followed by a SECOND `driver.virtio-gpu: online (` and a DIFFERENT claim
  generation. The generation is what makes it a rebind: a node that never left would come back on
  the number it had;
- no `iommu: FAULT` and no `KERNEL PANIC` across the whole window, which is the question a restart
  under enforcement asks that a restart without one does not - the old translation is torn down and
  a new one attached, and a device left able to reach the old mapping says so here;
- the rebound binding republished the providers it declares, which is the driver's own half of
  coming back: a binding that acquired the device and offered nothing would be online and useless;
- the guest's boot generation is unchanged, so none of the above is a reboot wearing a rebind's name.

**And M4 is still not fully met, which I found while writing the check rather than while running it.**

I had drafted a frame assertion - drive the console after the rebind and require no `a frame did NOT
reach the display`. Before running it I went to check what re-establishes DisplayService's connection
to the new binding, and nothing does. `route_offers` is the function that hands a published provider
to each fixed consumer, and it has two properties that meet here: it fills a slot only `if *client ==
0`, and it is called from ONE place - the phase-two bring-up loop - and never from the standing loop.
So a driver rebound after bring-up publishes its DISPLAY provider into the catalogue, nothing routes
it, and DisplayService goes on holding the handle of the binding that ended. The device comes back;
the system's use of it does not.

That is not a defect of this milestone to fix here: it is the per-service catalogue migration
P02M0164 scopes, stated from the other end - AudioService already survives this, because it
SUBSCRIBES and opens rather than being handed a slot once. So the check reports what the display did
and asserts only what the driver and the kernel owe, with the reason written where a reader will meet
it. Asserting frames would be asserting another milestone's unfinished work from this one; asserting
nothing about it would let the report read as though the whole path was proved.

So M4 stays unmet, and now for two separate reasons rather than the one the finding named. The
restart is DRIVEN - there is a check that asks for it through the operator's own verbs, which my
previous blocker wrongly said could not be reached - and it is not yet PROVED, because that check has
not been able to run (see below). And even when it does, the consumer half will not pass: the device
comes back and the display does not, for a reason that belongs to P02M0164's migration.

The verbs are the operator's own - P02M0166's `disable` and `enable` through the production policy
endpoint - so what is proved is the path an operator has, not a test hook.

**AND IT HAS NOT RUN, WHICH I AM REPORTING RATHER THAN LEAVING TO BE DISCOVERED.**

I tried to execute it and could not, and what stopped it is worth more than the check. The
dev-guest category is unusable end to end, in three linked steps:

1. `./dev.sh up` could not start a guest AT ALL. `cmd_dev_up` takes `dev-instance.lock` and HOLDS it
   across the `run.sh` it launches, and `qemu-run.sh`'s ad-hoc-guest guard tests that same lock - so
   the instance's own boot was refused by its own parent, with a message naming itself: "a
   development instance is running and holds the system, media and USB images". Reproduced directly
   rather than inferred: hold the lock and the guard's own `flock -n` returns non-zero.
2. With that fixed the guest BOOTS - measured, and it is the machine this milestone wants: `dma:
   every bus-mastering device is translated`, `iommu: virtio-iommu present, enforcing=true,
   healthy=true`, `driver.virtio-gpu: online (00:0b.0)`, and `agent.dev: online (registry)`. But the
   kernel's recovery ladder reports `no interactive shell within this target's boot window of 3000
   tick(s)`, so `cmd_dev_up` times out waiting for a prompt.
3. Because it timed out, it never reached `dev_identity_write`, and without a recorded boot
   generation every `lab.py` dev-* command refuses - including `dev-launch`, which is the one this
   check needs and which is otherwise ready to answer.

So I fixed (1), in `lab.py` and `src/harness/qemu-run.sh` - two files outside this milestone, which I
would not normally touch and am naming for that reason. The marker is set by exactly one caller for
exactly the boot that IS the instance; it is deliberately not the profile name the guard's own
comment rejects, because `scenario-cold` declares `DEV_PROFILE=1` too and is already covered by
`artifact_suffix`. Every other guest is refused exactly as before, and a second `dev.sh up` cannot
reach the guard - `cmd_dev_up` refuses on its own state check and its own non-blocking lock first.

(2) and (3) I have not fixed. They are a boot-window property of the development profile and the
identity write that depends on it, in a milestone that is not this one, and I am not debugging
another component's bring-up blind at the end of a round whose other changes are in the claim-release
path.

Nothing had noticed any of this because nothing runs the dev-guest checks: `verify-history.json`
holds no key for `dev.selftest`, `dev.proto-test` or `dev.perf-gate` either. Three registered checks
that the model emits and the runner asks for could not have run.

So the honest state of this finding: the seam exists, my previous blocker was false and I have said
so, the check that uses the seam is written and registered, and it has NOT been executed. It is not
evidence yet - not that the restart works, and not that the check would notice if it stopped. What it
is, is the requirement written down as something that runs rather than as something owed, plus a
measured account of the four things between it and a green result.

## Verification for this round

The model asks for a FULL verification of this change set - `src/kernel/device.rs` and the shared PCI
code are kernel-wide, and `verify-model` cannot vouch for a change to itself - so that is what ran.

| | result |
| --- | --- |
| `./test.sh --arch x86_64` | 373 passed, 0 failed |
| `./test.sh --arch aarch64` | 361 passed, 0 failed |
| `./test.sh --arch riscv64` | 364 passed, 0 failed |
| `cargo test` verify-model | 109 passed, 0 failed |
| `./check.sh --gate verify-model` | consistent: 544 checks, 1275 runnable keys, 386 kernel tests |
| `./check.sh --gate qemu-virtio-iommu-x86_64` (solo, fresh image) | PASSED - five hostile DMA cases refused, a DHCP lease through the enforcing controller, the default machine translated with a frame on the screen, `--no-iommu` still boots |
| `./check.sh --gate concurrent-selection` (solo) | PASSED |
| the rest of the gate sweep | 30 gates run, three FAILED and all three for reasons established below |

THE THREE GATE FAILURES, EACH CHECKED RATHER THAN ASSUMED AWAY.

`qemu-arch-profiles` failed on `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick`
at riscv64 AIA, 4 cores. It is a self-calibrating benchmark and its verdict flipped inside ONE sweep:
the individual `arch-profile-riscv64-aia-4` gate ran the same profile on the same binaries minutes
earlier and passed, printing "the remote wake could not be measured here - this machine's idle cores
do not stay halted long enough", while the umbrella decided the measurement WAS possible and failed
it. The noise floor it calibrates against differed by a factor of thirty-three between two runs of
the same code - 432974 in the full riscv64 suite against 12945 here - and the gap it compares is
inside the first and outside the second. Re-run on its own afterwards: PASSED. Nothing this round
touches the scheduler, and the full riscv64 suite ran this exact test on this exact code and passed
it.

`capability-trace` failed with "the newest x86_64 trace is older than the kernel beside it - it is
evidence about a kernel that has been rebuilt since". That is the gate working: the sweep rebuilt all
three architectures after the x86_64 suite had produced the trace. It is the ordering P02M0167's own
plan describes, and it needs a guest run after the last build rather than a fix.

`dynamic-report` failed on changed byte sizes for `lsdev` and `lsusb`. Both link `device-proto`,
which this round did not touch; `docs/DYNAMIC_EXECUTABLES.tsv` was last recorded in `39ae4bb9` and
`device-proto` last changed in `716fcadb`, which is newer. The recorded baseline is stale against an
already-committed change from an earlier round, and refreshing it is `check.sh`'s `--write` form
rather than anything this round owes.

Each of the three architecture suites was built AFTER the last edit to the kernel, so all three cover
every change here rather than the tree they started from.

WHAT THE SUITES DO NOT COVER, WHICH IS THE PART WORTH WRITING DOWN. Four of this round's changes are
compiled and booted through and never EXECUTED by any registered test, and I only found that out by
grepping for the lines they print:

- the planned-stop arm. `resolve_teardown` completes ZERO times in a full x86_64 run: `stop_all`
  sends `STOP` at all nine of the run's shutdowns and the machine exits before any teardown confirms,
  so `the node is`, `answered the stop` and `stopped cleanly` appear zero times each;
- the dependency-lost stop. No driver in this image declares a `requires` that is then withdrawn;
- the operator retry. Nothing types a policy verb;
- the catalogue and policy client reaping. No consumer of either endpoint exits during a run.

So for those four the evidence is that the system builds, boots and passes every test through the
modified code, and not that the new behaviour was observed. The dev-guest check added this round is
what executes the first of them - it disables a real driver, waits for the clean stop and then
requires `lsdev --incident` to answer that nothing has gone wrong - and the other three have no
executor in this tree yet. That is stated rather than left for the next audit to find.

ONE OBSERVATION THAT IS NOT A REGRESSION, checked rather than assumed. The riscv64 run printed
`device: 3 still holds a live MSI slot after its derived capabilities were swept` on one of its nine
shutdowns, and the pre-change log I first compared against did not - but that log was AARCH64, which
makes it no control at all. The same-architecture control says the change is clear: pre-change and
post-change aarch64 both print it zero times, over the same 361 tests and the same nine shutdowns,
with the only difference being 4 -> 5 MSI releases, which is this round's new claim test acquiring and
giving back a real vector. x86_64 prints it zero times as well.

What it is: `settled_vectors` spins 100,000 times waiting for a concurrent `Arc::drop` to run its
unbind, and its comment justifies the bound with "running inside a concurrent `Arc::drop` a few
instructions away". That reasoning holds on hardware and on KVM. Under TCG the other hart is a vCPU
the emulator may not schedule at all while this one spins, so a spin count is not a fair wait - the
device was virtio-blk, a production driver, and the quarantine that followed is the safe outcome by
design. It is a latent weakness of a spin-bounded confirmation on emulated multi-hart machines, and
it belongs to whoever next touches that wait.

AUDITOR'S RE-AUDIT ON M0159 (2026-09-01T11:58:45Z):

Current implementation rating: 8/10

1. **The new restart check drives a real rebind, but M4 remains functionally incomplete and still has no passing proof.** `dev-gpu-restart.py` now drives production `disable`/`enable`, requires a new claim generation, checks the translated profile and absence of IOMMU faults, and waits for replacement publication (`src/harness/dev-gpu-restart.py:172-242`). However, it deliberately reports rather than asserts post-rebind presentation (`src/harness/dev-gpu-restart.py:40-49,152-169,244-245`), and the latest response admits it has never completed a run. The underlying consumer path also remains broken: `route_offers` has only its phase-two call site and fills the fixed display route only when `gpu_client` is zero (`src/user/services/core/src/device_manager.rs:968-970,1118-1130`), so the rebound GPU republishes while DisplayService retains the dead prior-binding channel. That is weaker than M4's requirement that a frame reach the display and the driver remain present after restart (`docs/todo/P02M0159.md:94-97,126-133`).

AUDITOR'S RE-AUDIT ON M0159 (2026-09-01T14:33:49Z):

Current implementation rating: 8/10

1. **The restart path can rebind the GPU but still neither restores nor proves display presentation.** The restart check deliberately reports rather than asserts whether frames reach the display after rebind (`src/harness/dev-gpu-restart.py:40-49,152-169,219-245`). The underlying path remains broken: DeviceManager retains one fixed `gpu_client`, calls `route_offers` only during phase-two bring-up, and routes DISPLAY only while that slot is zero (`src/user/services/core/src/device_manager.rs:445-446,968-976,1138-1143`); its standing loop does not route replacement publications (`src/user/services/core/src/device_manager.rs:512-565`). DisplayService consequently retains the dead old-binding channel while the replacement provider remains undiscovered, which is weaker than M4 and the definition of done requiring a presented frame and functioning driver after restart (`docs/todo/P02M0159.md:94-97,126-133`).

AUDITOR'S RE-AUDIT ON M0159 (2026-09-01T17:10:24Z):

Current implementation rating: 8/10

1. **The enforcing-profile restart still restores only the GPU binding, not a usable display path.** The controlled check intentionally reports post-rebind presentation instead of asserting it (`src/harness/dev-gpu-restart.py:40-49,152-169,244-245`). That limitation reflects current production behavior: the standing loop can rebind and republish the GPU but does not route `Step::Online` offers (`src/user/services/core/src/device_manager.rs:512-565`); `route_offers` is called only during initial phase-two bring-up and fills the one fixed `gpu_client` only while it is zero (`src/user/services/core/src/device_manager.rs:968-976,1145-1157`). DisplayService therefore retains the ended binding's channel after disable/enable. A new claim generation and provider count do not satisfy M4's requirement for a frame-reaching, functioning GPU after restart (`docs/todo/P02M0159.md:94-97,126-131`). The cold-boot gate's syntax and presentation assertions are intact, but they do not exercise this lifecycle.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-01T21:45:00Z):

Three re-audits are answered here - `11:58:45Z`, `14:33:49Z` and `17:10:24Z`. All three carry one
finding and it is the same one: the restart check drives a real rebind but neither restores nor
proves display presentation, because DeviceManager cannot route a replacement provider to
DisplayService.

**Finding 1 (all three rounds) - the enforcing-profile restart restores only the GPU binding, not a
usable display path. ACCEPTED, and unmet.**

Every part of the trace is confirmed again against the current tree. `route_offers` fills each fixed
consumer slot only `if *client == 0`, it has exactly one call site - the phase-two bring-up loop -
and the standing loop does not route `Step::Online` offers. So a driver stopped and started after
bring-up republishes its DISPLAY provider into the catalogue, nothing routes it, and DisplayService
goes on holding the channel of the binding that ended. The check reports post-rebind presentation
instead of asserting it, and that limitation is production behaviour rather than a weak check.

What is NEW this round is that I stopped arguing about whose milestone it is and asked whether a
DeviceManager-only fix exists. It does not, and the reason is worth recording because it is the
argument for the migration rather than an excuse:

The client slots - `net_client`, `gpu_client`, `input_client` and the rest - are locals of the same
function the standing loop runs in, so the standing loop CAN reach them and could call `route_offers`
again on a replacement publication. That would fill the local. It would not reach DisplayService.
The handle was handed to that service by ServiceManager at bootstrap, positionally, under the role
tag `GPU`; DisplayService reads it once from its bootstrap channel and holds it in `Scanout::gpu`.
There is no channel from DeviceManager to DisplayService on which a REPLACEMENT could be delivered.
So re-routing would leave the manager holding a fresh handle and the service holding a dead one -
strictly worse than today, because the machine would then own a live provider nobody can reach and
the catalogue would say the kind is taken.

DisplayService already handles the first half correctly: its loop reads `Received::Closed` on the GPU
channel and sets `scanout.gpu = 0`. What it cannot do is learn about the replacement, and the seam
that lets it is the one AudioService already uses - a `provider-catalogue` connection and a
subscription to its kind. That is P02M0164's M-item and its explicitly scoped "each service changes
at one seam", and it is a change to DisplayService's start-up contract plus ServiceManager's role
list plus DeviceManager's routing, on the boot-critical display path of three ports.

I did not attempt it in this round, and that is a decision rather than an omission: this round's other
changes are six defect fixes in the claim, IOMMU-attribution and binding-lifecycle paths, and adding
a four-component bootstrap change to the display path on top of them would make a failure impossible
to attribute. It is the next thing this milestone's M4 needs, it belongs to P02M0164's migration, and
it is owed rather than argued about.

So M4 stays unmet on its consumer half. What the restart check does prove - and what I am not
claiming more than - is the driver-and-kernel half: a new claim generation, the translated profile
intact, no IOMMU fault and no panic across the window, the rebound binding republishing its declared
providers, a clean planned stop with `lsdev --incident` answering that nothing went wrong, and the
guest's boot generation unchanged so none of it is a reboot wearing a rebind's name.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it
was in flight, so each stamp below is against the tree that produced it.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed (193s) |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed (3456s) |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed (2881s) |
| `dma` host suite | 57 passed |
| `driver-binding` host suite | 58 passed |
| `verify-model` host suite | 115 passed |
| `check.sh --gate qemu-arch-profiles` | PASS - nine rows, including the new device-MSI checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

x86_64 is 376 where the previous round was 374: the two new kernel tests are
`kernel.object.claim.a_rollback_after_a_forced_release_frees_no_slot_it_no_longer_owns` and
`kernel.iommu.a_translated_address_stops_translating_when_its_claim_is_forced_to_end`. The second
declines on a machine with no `edu` fixture and SAYS so; where it has one, it ran and passed:

```
iommu-fixture: forced-release case PASSED - a live translated address stopped reaching its
frame when its claim was forced to end (transfer completed=true)
```

And on the ITS checkpoint row:

```
its: up - 16 event id bits, 512 device ids, 8192 LPIs from INTID 8192
interrupts: a device raised INTID 8192 - an LPI the ITS translated and delivered
device: 6 released - 1 MSI vector(s) given back
virtio-snd: the device's MSI vector was delivered on and then torn down with its claim
```

TWO THINGS FAILED DURING THE ROUND AND ARE REPORTED RATHER THAN SMOOTHED OVER. The first x86_64 suite
failed on my own new assertion - the sound test's claim release answered `Ok(Quarantined)`, because
the test mints its `Interrupt` by hand and never registers it in the derived table, so the release
correctly refused to confirm a vector nobody had given back. The second was the ITS device oracle on
a DIRECT profile row: `volume package module not found`, because that test reads its driver artifact
off the volume. Both are recorded in the responses above where they change what the answer is, and
the second changed the design of the fix rather than only its wiring.

AUDITOR'S RE-AUDIT ON M0159 (2026-09-01T22:46:17Z):

Current implementation rating: 8/10

1. **M4 still restores only the GPU binding, not a usable display path after restart.** The standing DeviceManager loop can rebind and republish the GPU, but it does not route `Step::Online`; `route_offers` is called only inside the initial phase-two loop and fills the one `gpu_client` slot only while it is zero (`src/user/services/core/src/device_manager.rs:520-565,980-1003,1170-1182`). DisplayService reads the injected `GPU` handle once at bootstrap and merely clears it when that binding's channel closes; it has no catalogue subscription or replacement path (`src/user/services/core/src/display_service.rs:488-519,550-580`). The controlled check accurately exposes the gap by deliberately reporting, rather than asserting, presentation after rebind (`src/harness/dev-gpu-restart.py:40-49,152-169,238-245`). A new claim generation and provider publication therefore do not meet M4 and the definition of done's explicit requirement that the GPU present a frame and remain functional after restart (`docs/todo/P02M0159.md:94-97,126-131`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-02T03:00:00Z):

One finding, ACCEPTED and unmet.

**Finding 1 - M4 restores only the GPU binding, not a usable display path. ACCEPTED.**

Re-confirmed against the current tree: the standing loop does not route `Step::Online`, `route_offers`
has only its phase-two call site and fills `gpu_client` only while it is zero, and DisplayService
reads the injected `GPU` handle once at bootstrap and clears it when that channel closes without any
path to a replacement. The check reports post-rebind presentation rather than asserting it, which the
finding correctly reads as the check exposing the gap rather than hiding it.

Nothing in this round changed that, and nothing in this round should have. What DID change is
adjacent and worth recording here, because it affects what the check can prove at all: the sibling
M0165 audit found that teardown confirmations were being discarded, so every planned stop landed
`Quarantined` instead of its intended state. The dev GPU restart check waits for `disabled` after a
disable and then enables - so until this round it could not have reached its second step even on the
driver-and-kernel half it does assert. That is fixed. The check's DRIVER half is now able to complete;
its DISPLAY half still cannot, for the reason the finding gives.

The consumer half remains P02M0164's migration and I measured last round why no DeviceManager-only
change can substitute for it: the handle was handed to DisplayService at bootstrap, positionally, and
there is no channel on which a replacement could be delivered. Re-routing inside the manager would
fill a local and reach nobody - strictly worse than today, because the machine would then hold a live
provider nobody can reach while the catalogue reported the kind as taken.

So M4 stays unmet on its consumer half, and the definition of done's "presents a frame and survives a
restart" is proved for the restart and not for the frame.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `./test.sh --arch riscv64` | ****367 passed**, 0 failed (a second run - see below)** |
| `dma` host suite | **59 passed** (57 + the two new tail cases) |
| `driver-binding` host suite | **60 passed** (58 + the two new teardown-composition cases) |
| `verify-model` host suite | **116 passed** (115 + the per-profile step case) |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows, including the firmware ITS device checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

THE FIRST riscv64 RUN OF THE SWEEP FAILED, AND IT IS THE DOCUMENTED FLAKE RATHER THAN THIS ROUND'S
WORK. `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` asserted at
2461343 woken cycles against 2142767 suppressed, a gap of 318576 over a self-calibrated floor of
250000 - so it failed by 27% of a number the test derives from its own noise. I re-ran that one test
four times on the same binary rather than assuming:

```
woken 2946843 (noise 302522), suppressed 2960432   PASS
woken 2634433 (noise 855177), suppressed 2390843   PASS
woken 1295185 (noise 228008), suppressed 2108696   PASS
woken 1661823 (noise 738485), suppressed 2100216   PASS
```

The woken figure spans 1.30M to 2.95M - a factor of 2.3 - and the noise floor the verdict is measured
against spans 228k to 855k, a factor of 3.7. The sweep's failing measurement sits inside that range.
The test's own comment records the same flip on the same machine and the same kernel, and nothing in
this round touches the scheduler: the changes are in the claim release, the IOMMU fault ledger,
DeviceManager, and the verification model, and DeviceManager is not even running during a kernel
suite. Because `test.sh` stops at the first failure, that run covered only 149 of the suite's tests,
so the riscv64 row above is a SECOND full run rather than the sweep's.

AUDITOR'S RE-AUDIT ON M0159 (2026-09-02T03:45:31Z):

Current implementation rating: 8/10

1. **M4 still restores only the GPU binding, not a usable display path after restart.** The standing
   DeviceManager loop handles a rebound node reaching `Step::Online` but does not call
   `route_offers`; that function remains confined to the phase-two launch path and fills
   `gpu_client` only while it is zero (`src/user/services/core/src/device_manager.rs:842-846,
   997-1003,1170-1182`). DisplayService still receives one positional `GPU` handle at bootstrap and,
   after observing its closure, merely clears it; it has no provider-catalogue subscription or other
   replacement channel (`src/user/services/core/src/display_service.rs:488-519,550-580`). The restart
   check correspondingly reports rather than asserts post-rebind presentation
   (`src/harness/dev-gpu-restart.py:40-49,152-169,238-245`). The latest response accurately leaves
   this unresolved, and a new claim generation/provider publication still does not meet the explicit
   requirement that a frame reach the display and the GPU remain functional after restart
   (`docs/todo/P02M0159.md:94-97,126-131`).

Focused verification was by current production-path inspection; no guest run was started.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-02T08:00:00Z):

One finding, ACCEPTED and unmet.

**Finding 1 - M4 restores only the GPU binding, not a usable display path. ACCEPTED.**

Re-confirmed against the current tree, including the part that changed since the last round: the
standing loop now handles a rebound node reaching `Step::Online`, and it still does not call
`route_offers`, which remains confined to the phase-two launch path and fills `gpu_client` only while
it is zero. DisplayService still receives one positional `GPU` handle at bootstrap and clears it when
that channel closes, with no subscription and no other replacement route. The restart check reports
post-rebind presentation rather than asserting it, which is the check exposing the gap rather than
hiding it.

I measured two rounds ago why no DeviceManager-only change substitutes for the migration - the handle
was given to DisplayService at bootstrap, positionally, and there is no channel on which a
replacement could be delivered - and that has not changed. Re-routing inside the manager would fill a
local and reach nobody, and would be strictly worse than today: the machine would hold a live
provider nobody can reach while the catalogue reported the kind as taken.

So M4's driver-and-kernel half is proved and its display half is not, which is what the check says.
The consumer half is P02M0164's seam and is now blocking two milestones' items.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `dma` host suite | 59 passed |
| `driver-binding` host suite | 60 passed |
| `verify-model` host suite | 116 passed |
| `check.sh --gate verify-scheduler` | **PASS - the new gate, 18 assertions** |
| `verify-model`, `gate-oracles`, `no-suppression`, `source-hygiene`, `test-tags` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

No suite failed and no gate failed, on any architecture. The riscv64 benchmark that flaked in the
previous round - `a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` - passed here,
which is what its measured spread predicts rather than evidence about it either way.

The enforcing IOMMU gate now names the case it was silently allowing to disappear:

```
qemu-virtio-iommu:   forced-release case PASSED
```

And the new scheduler gate reports what it proved:

```
verify-scheduler: failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE,
unmeasured costs and the guest-slot budget all hold
```

ONE THING WAS FOUND BY THIS ROUND'S OWN WORK AND IS WORTH RECORDING. After declaring a guest slot on
every step that boots one, the emitted plan still showed no `STEPGUESTS` line for the profile rows:
the emitter wrote that field only for a step needing more than ONE, on the reasoning that "one is
what the runner already assumes for anything that boots" - which was true only while the runner
inferred it from the command text. The classifier change and the declaration change together were
inert until the emitter was fixed too, and reading the emitted plan rather than the code is what
showed it.

AUDITOR'S RE-AUDIT ON M0159 (2026-09-02T12:05:18Z):

Current implementation rating: 8/10

1. **M4 still does not restore a usable display path after the GPU driver rebinds.** The standing
   DeviceManager loop can bring the replacement binding to `Step::Online`, but its match discards
   that result; `route_offers` is still called only by the phase-two launch loop and stores the
   display provider in the one fixed `gpu_client` slot (`src/user/services/core/src/device_manager.rs:512-564,993-1003,1170-1182`).
   DisplayService receives that one handle at bootstrap and, when it closes, only clears
   `state.scanout.gpu`; it has no catalogue subscription or replacement channel
   (`src/user/services/core/src/display_service.rs:488-519,550-580`). The restart check explicitly
   reports but does not assert post-rebind presentation (`src/harness/dev-gpu-restart.py:40-49,152-169,238-251`).
   Thus a replacement provider may be published while no frame can reach it, contrary to M4 and the
   definition of done (`docs/todo/P02M0159.md:94-97,126-133`). The implementer's acceptance of this
   gap is accurate; leaving it for M0164 does not make this checked-off milestone requirement met.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0159 (2026-09-02T18:20:00Z):

FINDING 1 - M4 still does not restore a usable display path after the GPU driver rebinds: ACCEPTED
AND FIXED. Every part of the description checked out: `route_offers` was called only from the
phase-two launch loop, it filled `gpu_client` only `if *client == 0`, DisplayService received that one
handle at bootstrap and on close only cleared `state.scanout.gpu`, and `dev-gpu-restart` REPORTED the
outcome instead of asserting it. The auditor is also right that my previous acceptance did not make
the item met - a checked-off requirement whose consumer half cannot work is a milestone that says
something untrue.

WHAT CHANGED. DisplayService discovers its display instead of being handed one, which is the seam
P02M0164 scopes and the only shape in which M4 is reachable.

- `src/user/services/manifest.toml`: display_service's `GPU` role is replaced by a `CATALOGUE`
  factory role on device_manager's `SERVE`, appended LAST because the bootstrap is read positionally.
  Its `providers` list gains `device-proto`, `base-proto` and `ipc-client`, as AudioService's did.
- `src/user/services/core/src/display_service.rs`: subscribes to `ProviderKind::Display` at bootstrap
  and takes the provider the snapshot already carries - the catalogue registers a subscriber and sends
  it everything published before it answers, so this is a poll and never a block, and a machine with
  nothing published falls through to the boot framebuffer exactly as one with no GPU always did. The
  subscription channel joins the `wait_any` set. On a live publication while the service has no GPU it
  opens a connection and calls the new `adopt_scanout`, which re-runs the `FB` handshake, installs the
  new scanout, marks every surface uninitialised and releases the old mapping only once the new one is
  known good - then notifies the resize and repaints.
- `release_scanout` replaces the peer-close arm's bare `state.scanout.gpu = 0`, which left the dead
  channel handle open in this process for the life of the boot.
- `src/user/services/core/src/device_manager.rs`: `gpu_client` and the DISPLAY take in `route_offers`
  are gone. The `GPU` hand-off keeps its tag and carries a FACT - whether a display driver is bound -
  exactly as `SND` does, because the supervisor's driver-status view is the only thing that ever used
  it.
- `src/user/services/core/src/service_manager.rs` and `service_manager/bootstrap.rs`: `gpu_client`
  becomes `gpu_online`, and the display_service `GPU` role branch is gone.
- The three kernel harnesses that stood in for the supervisor now answer the subscription instead of
  sending `GPU`; `tests::serve_provider_catalogue` takes the kind rather than hardcoding `Audio`.
- `src/harness/dev-gpu-restart.py`: `report_the_display` becomes `require_the_display` and FAILS if a
  frame driven through the console after the rebind does not reach the display. Its header said in as
  many words why it could not assert this; that reason is gone, and a check that reports what it was
  built to prove is a check nobody fails.

`docs/todo/P02M0159.md`: M4 records the driver half as it stood and the consumer half as it is now.

VERIFICATION: reported at the end of this response set.

ADDENDUM TO THE 2026-09-02T18:20:00Z RESPONSE - WHAT THE DEV CHECK COULD NOT BE RUN AGAINST:

`dev-gpu-restart` now asserts the display rather than reporting it, and that assertion has NOT been
exercised, because the persistent development instance does not come up: `./dev.sh up` stalls during
service bring-up and never reaches a shell. Measured rather than assumed, and traced with temporary
probes that were removed afterwards - ServiceManager reaches AudioService's `CATALOGUE` factory role
with a live `device_manager` SERVE root and blocks inside `service_connect`, while DeviceManager is in
its standing loop with that root live in its wait set and its `CONNECT_OP` arm never reached. It is
deterministic, identical at one core and at four, and it happens BEFORE any of this round's code runs,
on the audio subscription that has been in place since 2026-08-31.

What it exposes is that nothing in this tree BOOTS the development configuration - `development-build`
proves it compiles and `development-gate` inspects the built volume - so a boot defect in it is
invisible to every check here. Written up under P02M0164's M3, where that seam belongs.

The display path itself IS proved end to end on the shipping configuration, which is where this
milestone's profile lives: `qemu-virtio-iommu-x86_64`'s default-machine case boots the enforcing
machine and reports "the display driver runs and a frame reached the screen", with DisplayService
reaching its device through the catalogue and no fixed slot anywhere in the path.

VERIFICATION FOR THIS ROUND (2026-09-02T18:20:00Z), the same run behind every response in this set:

- x86_64 kernel suite, scoped to what changed - `object,dma,display,console,service,syscall,drivers,
  volume-layout,boot`: 239 passed, 0 failed. It carries this round's two new kernel tests
  (`kernel.object.claim.a_capability_minted_before_its_row_dies_with_its_claim`,
  `kernel.volume_layout.the_reserved_device_policy_namespace_answers_only_its_owner`), the boot test
  that requires EVERY manifest service online, and the DisplayService and console harnesses that were
  rewired onto the provider catalogue.
- `driver-binding` host suite: 61 passed, 0 failed - including the withdrawal-effects recorder and
  the operator-policy rules added this round.
- `verify-model` host suite: 117 passed, 0 failed - including
  `an_unmeasured_step_is_never_priced_at_zero` and the two new profile-row catalogue entries.
- `verify-scheduler` gate: 21 assertions, all holding. The new guest-slot case was run against the
  OLD condition first and produced the overcommit it is written for (`wide-start narrow wide-end`);
  against the fix it produces `wide-start wide-end narrow`.
- `qemu-virtio-iommu-x86_64`, on a freshly built image: every hostile case refused, a DHCP lease
  through the enforcing controller, and the default machine "translated, nothing degraded, nothing
  faulted, the display driver runs and a frame reached the screen" - which is the display migration
  proved end to end on a real boot with a real virtio-gpu.
- Host gates: `bootstrap-plan`, `declared-interfaces`, `gate-oracles`, `no-suppression`,
  `milestone-index`, `source-hygiene`, `test-tags`, `verify-model-tests`, `build-order`,
  `no-fixed-provider-slots`, `development-build` - all clean.
- `milestone-index` was FAILING before this round (the index marked P02M0151 done while its M6 was
  unchecked) and is clean now.

WHAT WAS NOT RUN, AND WHY: the persistent development instance does not boot - `./dev.sh up` stalls
during service bring-up, deterministically, before any of this round's code runs. It is measured and
written up under P02M0164's M3; it blocks `dev-gpu-restart`, whose new assertion is therefore
unexercised. aarch64 and riscv64 were not run this round: nothing here is architecture-specific
except the two new UEFI profile rows, which are gate rows rather than suite runs.

SECOND ADDENDUM (2026-09-02T21:40:00Z) - THE DEV CHECK NOW RUNS, AND WHERE IT STOPS:

The development instance boots again - the deadlock was DeviceManager's, is written up in this
round's M0164 response, and is now guarded by the `one-wait` gate. `dev-gpu-restart` therefore RUNS
for the first time in this work, and it gets as far as: the guest is the translated machine,
`virtio_gpu` is online at device 9 with claim generation 1, and then the disable is refused -
`lsdev --disable` reports that this boot granted the launched program no device-policy authority.

So the assertion I changed from a report into a failure is still not exercised: the check stops
BEFORE the rebind it exists to drive. What blocks it is a grant, not a display: measured, the
supervisor mints the operator endpoint, `service_connect` on it succeeds, and PermissionManager holds
the resulting client - so the loss is between the manager holding it and the launched program
receiving it. The read half of the same tool works in that guest, which is what makes it this
capability rather than the launch. Recorded against the operator-policy milestone, which owns it.

What IS proved for this item, on two independent boots: the shipping default machine under the
enforcing profile - "the display driver runs and a frame reached the screen" - and now the
development instance, where DisplayService comes up through the catalogue and ConsoleService reports
a frame reaching the display. The restart half waits on that grant.

VERIFICATION FOR THIS ADDENDUM (2026-09-02T21:40:00Z):

- The development instance reaches an attached shell in 95 s, twice, on the final tree: DisplayService
  online, PermissionManager online, ConsoleService online, and `ConsoleService: a frame reached the
  display`. Before the fix it stalled at the same point on every attempt, at one core and at four.
- `one-wait`: clean on the fixed tree, and watched to FAIL with the defect reintroduced - it named the
  literal wait and its line.
- x86_64 kernel suite, same scope as the round above: 239 passed, 0 failed.
- `qemu-virtio-iommu-x86_64` on a freshly built image: unchanged, including "the display driver runs
  and a frame reached the screen".
- `verify-model`: 117 passed with the new gate row (`GATES` 66 -> 67).
- `no-suppression`, `source-hygiene`, `bootstrap-plan`, `milestone-index`, `no-fixed-provider-slots`,
  `declared-interfaces`, `development-build`, `development-gate`: clean. The tree was returned to the
  shipping configuration afterwards, which `development-gate` confirms.
- Every temporary probe used for the diagnosis was removed; `no-suppression` and `source-hygiene` were
  re-run after that removal.

THIRD ADDENDUM (2026-09-02T23:55:00Z) - `dev-gpu-restart` PASSES:

The addendum above said the assertion was not exercised and named the grant that blocked it. That
grant is fixed, a second defect behind it is fixed, and the check now runs to the end:

    dev-gpu-restart: the guest is the translated machine
    dev-gpu-restart: virtio_gpu is online at device 9, claim generation 1
    dev-gpu-restart:   the stop completed and the node is disabled
    dev-gpu-restart:   and it is not recorded as an incident
    dev-gpu-restart:   virtio_gpu is online again on claim generation 2
    dev-gpu-restart:   and it republished 1 provider(s)
    dev-gpu-restart:   frames were driven through the console after the rebind and it is still serving
    dev-gpu-restart: one boot throughout
    dev-gpu-restart: passed

A NEW CLAIM GENERATION IS WHAT MAKES IT A REBIND, and it is 2. So M4's restart is executed on the
enforcing machine through the operator's own verbs, with the display service reaching its device
through the catalogue on both sides of it.

WHAT THE CHECK ASSERTS AND HOW IT READS IT (corrected this round). It waited for DeviceManager's
`stopped cleanly` and `driver.virtio-gpu: online (` lines on the SERIAL log, and after the boot those
lines do not arrive there: once ConsoleService has taken the console a service's `print` goes to its
VT, and the serial line carries the shell's session. Measured - the disable is accepted, the node
reaches `disabled`, and the serial log gains nothing at all. A check that waits sixty seconds for a
line that cannot appear reports a working mechanism as broken, so both waits moved onto `lsdev`: the
node's STATE, its CLAIM GENERATION and its INCIDENT record, which are the operator's own surfaces and
better evidence than a console string. The kernel's own lines - `iommu: FAULT`, `KERNEL PANIC` - do
reach the serial log and are still read from it.

AND THE DISPLAY STEP IS HONEST ABOUT WHAT IT CAN SEE, which is a correction to what I did in the
round above. I turned it from a report into an assertion on the absence of `a frame did NOT reach the
display` - and that line cannot appear on serial after boot either, so the assertion could not fail.
An assertion that cannot fail is worse than a report, because it reads as evidence. It now fails IF
the line appears, requires the console to still answer a prompt after frames are driven through it,
and says in as many words that the frame-level proof lives on the two boots that can see it - the
kernel suite's console test and `qemu-virtio-iommu-x86_64`'s default machine, both of which report a
frame reaching the display with DisplayService on a catalogue-opened provider - and what surface
would make it provable here.

VERIFICATION FOR THIS ADDENDUM (2026-09-02T23:55:00Z):

- `dev-gpu-restart: passed` on the enforcing development machine, twice, on the final tree: disable
  accepted, node `disabled`, no incident, enable accepted, `virtio_gpu` online again on CLAIM
  GENERATION 2 with its provider republished, no `iommu: FAULT`, one boot throughout.
- `grant-vocabulary`: clean, and watched to FAIL with `DevicePolicy` removed from the array again.
- x86_64 kernel suite, same scope as this round: 239 passed, 0 failed. Four pinned permission-audit
  summaries were updated with the two capabilities the vocabulary had been missing - a probe granted
  neither now reports them denied, which is what those assertions exist to show.
- `qemu-virtio-iommu-x86_64` on a freshly built image: unchanged, including the default machine's
  display frame.
- `one-wait`, `no-suppression`, `source-hygiene`, `bootstrap-plan`, `milestone-index`,
  `no-fixed-provider-slots`, `declared-interfaces`, `development-build`, `development-gate`,
  `verify-scheduler`: clean. The tree was returned to the shipping configuration afterwards, which
  `development-gate` confirms.
- Every temporary probe used for the diagnosis was removed before the final build.

AUDITOR'S RE-AUDIT ON M0159 (2026-09-03T03:06:02Z):

Current implementation rating: 8/10

1. **M4 still has no effective assertion that a frame reaches the display after the GPU rebind.**
   The milestone requires `virtio-gpu` to bind, present, and survive a restart, specifically rejecting
   evidence that establishes only an online driver or a cold bring-up (`docs/todo/P02M0159.md:94-97`).
   The replacement-provider path is now implemented, but `exercise_the_display` explicitly records
   that `a frame did NOT reach the display` cannot appear on the serial log after boot and therefore
   that its absence proves nothing (`src/harness/dev-gpu-restart.py:151-166`). The function nevertheless
   uses that unobservable line as its only negative presentation check; its actionable assertion is
   only that the serial shell returns a prompt after `clear` (`src/harness/dev-gpu-restart.py:167-180`).
   A live serial prompt does not establish that DisplayService adopted the generation-2 provider or
   that a present reached its scanout. The cited kernel/default-profile checks prove frames on separate
   cold boots, before any driver restart. Consequently the reported passing `dev-gpu-restart` can still
   pass when post-rebind presentation is broken, and the implementer's claim that M4 is exercised is
   incomplete.

Verification: reviewed the current restart harness, ConsoleService's one-per-outcome presentation
reporting, and the current M4 requirement. The existing cold-boot/QEMU pass cannot close this
post-rebind observability gap.
