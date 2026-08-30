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
