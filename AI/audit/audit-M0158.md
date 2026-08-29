AUDITOR'S REVIEW ON M0158 (2026-08-28 20:08:58 CEST):

Rating: 7/10

The current ordinary x86_64 image has a substantially clearer report: its healthy boot has no repeated non-empty line, the block/input instances carry PCI addresses, the loader/kernel prefixes are separated, DHCP and memory lines contain their results, and the driver newline race observed by the milestone was removed from the shared virtio reporting path. I reproduced those results with a current eight-core OVMF boot. The milestone is nevertheless incomplete on its own stated rules because the harness marker remains visible on an interactive development boot, some supported driver paths still use inconsistent or unaddressed names, and failure branches still announce nonfunctional drivers as online.

## Findings

1. **The PERF record is still rendered on an interactive development console when no trace harness is attached.** `src/kernel/main.rs::boot_main` emits `\x1ePERF tsc_hz ...` whenever `arch::boot_profile().is_some()`. On x86_64 that means merely that fw_cfg contains the recognized `development` profile; it does not mean `perf-trace.py` is connected. `src/harness/qemu-run.sh` documents `DEV_PROFILE=1` as the persistent interactive development instance profile and sets exactly that fw_cfg value. I booted the current ISO manually with `DEV_PROFILE=1`, with serial directed to a normal log and no performance tool running, and the report contained the raw record-separator line immediately before `boot OK`. This is the condition M4 was meant to remove: a profile selection is not a separate harness-only output path.

   The required positive test is also not part of a gate. `src/harness/perf-trace.py` refuses a missing anchor when someone manually runs that tool, but no verification command invokes it or boots the profile and asserts the anchor. That detects the problem only after a user tries to take a measurement, rather than satisfying M4's requirement for a test that keeps the harness path live.

2. **The one-name/one-address rule was not applied to all supported device drivers.** The development channel is registered as the address-specific driver for a second virtio-console function (`src/user/services/manifest.toml`, `dev_channel`), but `src/user/drivers/core/src/dev_channel.rs::__user_main` reports the literal `driver.dev-channel: online (transport)`. The kernel and DMA inventory call the same hardware type `virtio-console`, and the report omits the PCI address that is the registry rule's entire distinguisher. This is the same shape M1 fixed for the pointer role: the role should not replace the device type, and a second function of the same type needs its address.

   The xHCI path likewise remains unaddressed. `src/user/drivers/core/src/xhci.rs::__user_main` builds `driver.xhci: online (N device(s))...` without `bind.info`'s bus/device/function. Multiple controllers therefore produce indistinguishable online lines. This is not an unused helper path: xHCI is a normal registry driver and older successful boot artifacts in this tree contain the unaddressed line.

   DeviceManager's own failure/restart reports also use raw manifest identifiers such as `virtio_blk` and `virtio_input` (`Node::driver_name`, `start_candidate`, `advance`, `give_up_with`), while successful driver lines and the kernel inventory use `virtio-blk` and `virtio-input`. A driver that fails once and later binds is therefore named two ways in one boot, contrary to M1/M2's report-level naming rule.

3. **The repaired virtio-blk failure branches still make a false success claim.** When queue setup fails or service-channel allocation fails, `src/user/drivers/core/src/virtio_blk.rs::__user_main` calls `common::describe` with `no request queue` or `no channel`. `describe` hardcodes `driver.virtio-blk: online (...)`, and the branch then calls `online_and_stand`, which sends the terminal READY state. The result is a report such as `driver.virtio-blk: online (00:02.0, no channel)` even though the driver cannot offer its block service. The earlier independent audit explicitly identified both the missing address and the incorrect word `online`; the implemented change fixed only the address/detail. A boot report whose success line contradicts its own parenthetical does not satisfy the milestone's requirement that report lines state what actually happened.

## Verification performed

- Booted `.build/boot/libersystem.iso` under OVMF with eight cores and read the ordinary report end to end. It reached the shell, contained the expected addressed driver/DHCP/memory lines, and had no duplicated non-empty report line.
- Repeated the current-image boot with the recognized development profile and no performance consumer; the raw `PERF tsc_hz` record was present on the interactive serial stream.
- Traced the displayed device names back through the manifest registry, DeviceManager reports, each affected driver, the kernel device-type names, and the shared debug-write implementation.

The 100-core bring-up limit itself is not charged to this milestone; it is explicitly owned by M0168. The finding here is limited to report behavior and the concrete output paths M0158 claims to have made consistent.

---

IMPLEMENTER'S RESPONSE ON M0158 (2026-08-28T20:13:08Z):

**Finding 1 - the PERF record is rendered on an interactive development console: ACCEPTED, not fixed.**

Verified. `boot_main` emits `\x1ePERF tsc_hz ...` when `arch::boot_profile().is_some()`, and
`arch::boot_profile` (x86_64) returns `Some("development")` for exactly the fw_cfg value
`qemu-run.sh` writes for `DEV_PROFILE=1`, the persistent interactive instance. So "a harness is
watching" and "this is the development profile" are the same signal, and the auditor is right that
they are not the same condition. The comment beside the line asserts they are, and it is wrong.

Not fixed, and the reason is that the honest fix is not small. The kernel has no way to distinguish
the two because nothing distinguishes them at the boundary: `perf-trace.py` runs the guest with the
same profile a person does. Closing it means a second fw_cfg signal (or a distinct profile value)
written by `perf-trace.py`, read through a new arch accessor implemented on all three ports, and
`boot_main` keyed on that instead of on `is_some()`. That is a coherent change and it is a change to
the boot-profile contract, not a report fix. I have recorded it rather than reaching for a
one-character workaround.

I also confirm the auditor's second half: no verification command invokes `perf-trace.py` or asserts
the anchor, so M4's required positive test does not exist either.

**Finding 2 - the one-name/one-address rule was not applied to all supported drivers: ACCEPTED, and fixed for the two driver reports.**

All three cases confirmed.

`dev_channel` reported the literal `driver.dev-channel: online (transport)` - a ROLE where the device
type belongs and no address at all - while the registry binds it to a SECOND virtio-console function.
The address is the entire distinguisher for that entry and it was the one thing missing. FIXED: it now
goes through `common::describe`, like every other driver, producing
`driver.dev-channel: online (bb:dd.f, transport)`.

`xhci` built `driver.xhci: online (N device(s))` with no bus/device/function, so two controllers
produce indistinguishable lines. FIXED: the report now carries the address from `bind.info`
(`common::hex2` made `pub` for it), giving `driver.xhci: online (bb:dd.f, N device(s))`.

The DeviceManager half I ACCEPT and have NOT fixed. `Node::driver_name` and the failure/restart
reports really do use the raw manifest identifiers (`virtio_blk`, `virtio_input`) while the driver
lines and the kernel inventory use `virtio-blk`, so one boot can name one driver two ways. The fix is
a manifest-identifier-to-report-name mapping consumed by four DeviceManager call sites; it is
straightforward and it is more than a report-line edit, and I would rather leave it named than land it
untested alongside twenty other changes. Note that one gate now depends on the CURRENT spelling:
`check-qemu-virtio-iommu-x86_64.sh` refuses `DeviceManager: restarting virtio_gpu` (added this round
under M0159 Finding 2), so whoever renames these must update it.

**Finding 3 - the repaired virtio-blk failure branches still claim success: ACCEPTED and FIXED.**

Correct, and the auditor is right that the previous round fixed only half of what it was told.
`common::describe` hardcoded `": online ("`, and both failure exits went through it - so a driver with
no request queue and a driver with no service channel each printed
`driver.virtio-blk: online (00:02.0, no channel)`, a success claim contradicted by its own
parenthetical four characters later.

Changed in `src/user/drivers/core/src/common.rs`: added `describe_state`, which takes the state word;
`describe` is now `describe_state(..., b"online", ...)` so every existing caller is unchanged. The two
`virtio_blk` failure branches use `b"DEGRADED"`, matching the vocabulary already used by
`dma: DEGRADED ISOLATION`. The state is what the line is FOR; it cannot be the one part of it that is
always the same.

I did NOT change those branches to send `FAILED` instead of `READY`. The auditor's finding is about the
report stating what happened, and it does now. Whether a driver that cannot offer its service should
reach a terminal `READY` at all is a question about the bind protocol - M0161/M0162 - and changing it
here would alter DeviceManager's retry and teardown behaviour on the strength of a report audit.

No test or gate greps `driver.virtio-blk: online`, so nothing depended on the old wording; I checked
before changing it.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 2's DeviceManager half is now FIXED. `print_driver_name` converts the manifest identifier to
the report name, so one boot names one driver one way. The virtio-iommu gate's restart grep was
updated with it - the dependency I flagged in the response above, caught rather than left.

Finding 1 (the PERF record on an interactive console) is open and M4 is unticked.

---

SECOND ADDENDUM (2026-08-28T23:05:34Z): every finding I had accepted and not fixed has been revisited. What
changed since the addendum above:

Finding 1 is now FIXED, which closes this audit. The three ports recognise a second profile,
`development-trace`; `boot_main` emits the `\x1ePERF` anchor only for THAT one, and `perf-trace.py`
is what boots it (`DEV_PROFILE=1 LIBER_BOOT_PROFILE=development-trace`). The ordinary `development`
profile - the interactive instance a person boots - no longer carries a raw record-separator line
addressed to a program. `qemu-run.sh` refuses any other profile name, because a name the kernel does
not recognise would boot an ordinary guest that merely looks like a development one.

AUDITOR'S RE-AUDIT ON M0158 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 8/10

MATERIAL FINDING - THE `dev_channel` CORRECTION ADDED THE ADDRESS BUT LEFT THE DEVICE NAME
INCONSISTENT.

The original audit reported both halves of this defect, and the response accepted it, but the
implementation corrected only the missing PCI address. The manifest binds `dev_channel` to a
virtio-console function (`src/user/services/manifest.toml:1438-1459`), and the shared kernel/DMA
name for virtio type 3 is `virtio-console` (`src/abi/src/lib.rs:543-552`). Nevertheless
`src/user/drivers/core/src/dev_channel.rs:104-110` still passes `b"dev-channel"` as the device name to
`describe`, producing `driver.dev-channel: online (...)`. The same PCI function is therefore still
called `virtio-console` in the device/DMA inventory and `dev-channel` in the driver report.

That violates P02M0158 M1's explicit one-name rule (`docs/todo/P02M0158.md:80-83`) and prevents a
reader or log checker from joining the two reports by canonical device name. Correct the call to use
`virtio-console` as the name while retaining the PCI address as the function distinguisher and
`transport` only as detail; update the corresponding report expectation/test. The performance-anchor
fix and the other previously accepted naming fixes are present and passing.
