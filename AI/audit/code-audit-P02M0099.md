IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-09T20:30:00Z):

Scope: `docs/todo/P02M0099.md` is a NON-COMPLETABLE INDEX by its own status line and by the
roadmap's `[i]` row - never ticked, closed item by item. What this job owns of it is what its own
rules make startable: the items whose prerequisites exist and whose item plan is written. Read
before touching anything:

- Section 0, the Phase-2 maintenance subsection, gates every item on THE HOST-TEST SEAM (its
  first item) and on P02M0172's declared DMA policy. `virtio-blk maintenance` is the one item with
  its plan written ("THE ITEM PLAN IS WRITTEN (2026-08-31)"); the other six maintenance items and
  every group-1/2/3 item are subject to "NO ITEM IS STARTED WITHOUT ITS OWN WRITTEN PLAN", which
  this implementer does not write for them (that is planning, and the index says why it is
  declined in advance).
- "THE INDEX GETS A STATE OF ITS OWN": `check-milestone-index.sh` already knows the `[i]` marker
  (title-checked, never counted open, an unknown marker refused in its self-test) and
  `docs/todo/TODO.md` carries both this index and P02M0103 as `[i]`. The item is landed; the tree
  did not say so in the item's own checkbox.
- P02M0172 declared every driver row's DMA policy in `src/user/services/manifest.toml` and made the
  kernel admit from the generated registry. That is the "declare its DMA policy" clause of six
  maintenance items; the rest of each of those items stays open and is not started here.
- The host-test strategy did not compile: `cargo test --manifest-path src/user/drivers/core/Cargo.toml`
  failed with E0152 (rt's `panic_impl` against std's), which the index records as a prerequisite of
  the whole section.

## What was implemented

**The host-test seam (cfg/feature design).** `src/user/runtime/rt` gained the feature
`host-tests`: it drops the `_start` stubs and `__rt_start`, the allocation-error hook, the
`#[panic_handler]` and the `#[global_allocator]` registration, and replaces the architecture
`syscall` entries with one that answers `ERR_UNSUPPORTED` - there is no kernel on the host, and a
wrapper reached from a pure-layer test must fail the way a missing facility fails rather than
execute the host kernel's syscall of the same number (`SYS_YIELD` is 21, which Linux reads as
`access`). `src/user/drivers/core/Cargo.toml` enables it through `[dev-dependencies] rt = {
features = ["host-tests"] }` - cargo unifies a dev-dependency's features into `cargo test` and into
no other build, so shipped programs never see it - and marks every `[[bin]]` `test = false`: the
programs are `no_std` and carry no harness. `lib.rs` is `#![cfg_attr(not(test), no_std)]`. The
gate the plan states for this design, `cargo test --manifest-path src/user/drivers/core/Cargo.toml`
from the repository root (the host target), runs the library's suite: 8 tests.

**DRV-001, central virtqueue validation** (`src/user/drivers/core/src/virtio.rs`): `UsedFault`,
`check_used_element` (the id is one the ring has and, on the synchronous path, the posted head; the
length is at most the chain's device-writable bytes) and `check_used_advance` (the used index never
moves past the buffers outstanding, wrapping as a free-running `u16`); `ring_layout` shared by
`setup_queue` and the fake controller; `Queue` tracks `posted` (up on `post_recv` and
`submit_async`, down on `take_used`) and `fault`; `take_used` checks the index before it believes it
and the element before it returns it; `submit` is `submit_checked(...).ok()`, and `submit_checked`
refuses an empty or over-long chain, a request the device never completes, more than one completion,
a completion that is not descriptor 0, and a length past the chain's writable bytes. The seam:
`Queue::over(virt, size, notify)` under `cfg(test)`, and `src/virtio/tests.rs` drives it over a
`Vec<u64>`-backed ring with the device's half written by hand (the synchronous completion from a
thread, since `submit` samples the index before it publishes). `post_recv`/`submit_async` take
`&mut self` now; every caller already held a mutable queue.

**DRV-002 and DRV-003 / WIRE-002** (`src/user/drivers/core/src/blk.rs`, `virtio_blk.rs`): the pure
decisions `request_range` (count zero or above the request limit refused, `lba + count` past the
capacity or overflowing refused - no clamp) and `write_source` (a memory object, readable through the
handle, at least `bytes` long - the three parts of one `object_info` answer), with `src/blk/tests.rs`.
The driver replies `STATUS_INVALID` (2, new, typed apart from `STATUS_ERR`) for either refusal, closes
a refused write's handle, and never asks the device; `serve_write` calls `object_info` before it
maps. StorageService treats any non-zero status as a failed request (`service.rs`, the
`status != 0` check), so the new code is a refusal to it today and a distinguishable one for a
consumer that wants to distinguish.

**The oracle** (`src/tools/check-qemu-virtio-iommu-x86_64.sh`, `src/user/services/storage/src/
service.rs`, `image.sh`): the traffic phase attaches a `virtio-blk-pci` endpoint with
`iommu_platform=on` carrying a run-private copy of the bootable system volume the medium's signed
manifest names, and asserts the driver's `driver.virtio-blk: online (bb:dd.f)` line, the kernel's
`iommu: bb:dd.f attached to domain` line for that address, and StorageService's new line
`storage: vol://system mounted through its block provider ...`, printed only after the partition
table and the superblocks were read over the provider's request channel and the mount succeeded.
The volume comes through `evidence_image` - the run's stored artifact inside a release run (image.sh
stores it beside the enforcing ISO), the tree's otherwise.

**The index state**: ticked in the index with the facts above; `docs/todo/TODO.md` unchanged
(`[i]`).

## Verification (so far)

- `cargo test --manifest-path src/user/drivers/core/Cargo.toml` (repository root): 8 passed.
- WATCHED TO FAIL, each debt under a compiling mutation of its check, then restored (sources
  compared byte-for-byte against the copies taken before): DRV-001 length bound disabled -> 4
  failed (every ring test); DRV-001 index bound disabled -> 4 failed; DRV-002 range check disabled
  -> `a_request_past_the_last_sector_is_refused_by_range` failed; DRV-002 count bound disabled ->
  `a_zero_count_and_a_count_above_the_request_limit_are_refused_not_clamped` failed; DRV-003 rights
  check weakened -> `a_write_source_must_be_...` failed; DRV-003 size check disabled -> the same test
  failed; restored -> 8 passed.
- `cargo build` of the driver crate and of the storage service for the x86_64 target: clean.
- `cargo check` of `rt` with `--features host-tests` on the host target and without on the shipping
  target: clean.
- `verify-model release-required --write`: 200 keys (`host.drivers / host / host / default` joined
  the frozen set, a reviewed act); `verify-model check`: consistent; 157 tests.
- `./check.sh --gate qemu-virtio-iommu-x86_64` with the block endpoint: RUNNING at the time of this
  entry (build, image, gate); the result is appended below.
- `./check.sh --gate qemu-virtio-iommu-x86_64` (after `./build.sh --arch x86_64` and
  `./image.sh --format iso`): PASSED end to end - the hostile phase under `gate` with
  `enforcing-required`, five refused cases plus the forced release, the traffic phase with BOTH
  endpoints behind the controller: a DHCP lease through virtio-net, and "the system volume was read
  through virtio-blk at 00:04.0, attached to a domain behind the controller" - the driver's online
  line, the kernel's attach line for that address and StorageService's read line all present; the
  default machine translated with nothing degraded; `--no-iommu` on the degraded image refusing
  virtio_net by name. One correction on the way: the gate's phase-2b assertion (written with
  P02M0172 and never exercised until now) grepped the suite's own output for the runner's row
  announcement, which lives in the run log the suite names; it reads the concatenated result logs
  now.
- NOT RUN YET: the whole kernel suite on aarch64 and riscv64 ("the suite green on all three
  targets" in the item's definition of done) - the emulated suites are part of the long run at the
  end of the whole job, and the virtio-blk item is left unticked until they are.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-12T21:49:45Z):

SIX PHASE-2 DRIVER DEBTS ARE CLOSED IN CODE, each with host tests over the crate's seam and each test
watched to fail under a mutation of the check it holds. None of the seven maintenance items is ticked:
every one of them owes a three-target suite run, and the run is what turns implemented into done.

WHAT WAS DELIVERED, BY DEBT

DRV-009 (virtio sound). The playback path was four pieces of trust and none of them was checked: any
interrupt was a completion - the MSI-X vector is shared with queues this driver sets up and does not
drive - the status word the device writes was never read, a failed period was played again out of a
reused page, and the answer to AudioService was always "OK", which is how the other three stayed
invisible. `drivers::snd` is the completion contract: a bounded repeated wait, a used element that
must be THIS submission's, a status structure that must have been written before its status is
believed, and every code that is not `S_OK` a failure. The refusal reaches AudioService as an EMPTY
reply - the convention this driver already used for capture - and the service stops marking the stream
running and treats eight refusals in a row as a lost device.

DRV-010 (virtio GPU). The device's reported display size was believed with only a zero refused, and
what an overflow does here is not a wrong picture: a wrapped `width * 4` describes a framebuffer whose
rows are shorter than the pixels in them, and that description is handed to ConsoleService, which maps
the buffer and draws into it. `drivers::gpu` bounds the geometry by the image model's own maximum
extent with the pitch and the backing size both checked, saturates the rectangle union - a wrapped one
has its corner before its origin - and CLIPS rather than clamps: a rectangle entirely off the display
presents nothing instead of being moved to the edge. The transfer offset is checked before the device
is told where to read.

DRV-002's USB half, DRV-003 / WIRE-002, DRV-005, DRV-006 and DRV-012 (xHCI). The block path clamped a
count and never checked the LBA, and `read10_cb` truncated the address to the thirty-two bits a
ten-byte SCSI command carries - so a request past two terabytes named a block near the start of the
medium and reported success. Both refuse now, through the same `blk::request_range` the virtio half
uses plus `blk::command_lba32`. The write's transferred handle is checked for type, rights and size
before anything is mapped, from one `object_info`, exactly as virtio-blk does. The status wrapper has
five fields and three were read: the residue is now checked before it is subtracted and a short
transfer is a refusal for every caller but the sense read. The flush turned every repeated failure into
success without reading the sense data; only ILLEGAL REQUEST / INVALID COMMAND OPERATION CODE means
"this unit has no volatile cache", and everything else is a failed barrier. And the configuration walk
trusted that the transfer arrived, that a record is as long as the field being read, and that the
answer was the descriptor asked for - `drivers::descriptor` checks all three, in one walker both class
drivers use, with `control_in` now reporting how many bytes actually arrived.

DRV-007 and DRV-008 (xHCI resources and events). Enumeration allocated a slot and three DMA pages and
dropped their handles: a partial failure left the slot enabled and the pages pinned for ever, and a
detach disabled the slot without closing anything. Every allocation is owned by the device now,
`UsbDevice::release` is the one place that undoes what enumeration did, and it is called from the
partial-failure path and from the detach path; a device the driver leaves addressed is held by the
inventory rather than dropped. And the two synchronous waits took every event off the ring and dropped
the ones they were not waiting for, so a device plugged in during a block read was invisible until
something unrelated woke the loop. The change is recorded where every event passes through, as ONE
BIT - a storm cannot grow it - and the reconcile reads each port's own register, which is what makes a
connect and a disconnect in one window exactly one attach and one detach.

AND TWO PARSER LAYERS THE ITEMS ASKED FOR. `drivers::net` bounds the link MTU the device reports -
it sizes every buffer this driver allocates - and answers where a received frame is from an index and
a length, including the case the shared ring check cannot: a length past the slot it claims to be in,
which reads into the next slot of the pool. `drivers::input` decides whether an `ABS_INFO` block is
a range at all and folds an event into the pointer - four rules that had no test because they were four
arms of a match inside an `unsafe` function over a raw address.

WHAT WAS VERIFIED, AND HOW

`cargo test` for `drivers`, run the way the gate runs it: 37 passed, 0 failed. Every new module's
tests were watched to FAIL under a mutation of the check they hold: removing the sound status
comparison fails two, believing a completion that is not there fails three, believing the GPU's
reported extent fails one, wrapping the rectangle union fails one, ignoring the CSW residue fails two,
treating every flush failure as "no cache" fails one, truncating the block address fails one,
believing a short descriptor transfer fails one, and losing the pending port change fails two.
Every driver binary builds for the user target. `./format.sh` was run.

NOT PERFORMED, AND THIS IS WHY NOTHING IS TICKED: the three-target guest suites. Each item's class
definition of done requires the driver to bind and serve through the governed path unchanged with the
suite green on x86_64, aarch64 and riscv64, and for virtio-blk the isolation gate's traffic phase as
well. A full sweep is running as this is written; until it is green these are implementations awaiting
their evidence.

WHAT IS STILL OPEN IN THE PHASE-2 SUBSET

`virtio-console maintenance` - the `console-bytes` provider's migration onto the catalogue. Its
consumer is still chosen by the DRIVER'S NAME rather than by the provider kind, and every other
console-bytes channel is closed; the manifest declares the kind for `virtio_console` as well, and that
driver publishes no provider at all. The migration owes provider identity, subscribe, attach, detach,
failover and reconnect, plus a versioned device-side byte-stream contract. It is the one item of the
seven whose work is not yet implemented.

IMPLEMENTER'S VERIFICATION NOTE ON P02M0099 (2026-09-13T00:22:55Z):

THE THREE-TARGET EVIDENCE IS PARTIAL, AND HERE IS EXACTLY WHAT WAS RUN.

PERFORMED AND GREEN. All three architectures build (`./build.sh --arch all`, and again with
`LIBER_DEVELOPMENT=1`). `check-host-tests.sh`: 94 suites, all green, with `drivers` among them -
37 tests including every new module. Twenty gates ran and passed before the first failure:
`numa-profile-riscv64`, `arch-profile-aarch64-gicv3-1`, `arch-profile-aarch64-gicv3-4`,
`arch-profile-aarch64-gicv3-its-1`, `arch-profile-aarch64-gicv3-its-4`, `arch-profile-aarch64-no-dt-1`,
`arch-profile-riscv64-aia-1`, `arch-profile-riscv64-aia-4`, `concurrent-selection`, `duplicate-edge`,
`rollback-floor-x86_64`, `model-mutations`, `perf-anchor`, `iommu-riscv64-uefi-aia`,
`iommu-riscv64-uefi-aia-ordinary`, `iommu-aarch64-direct-gicv2-ordinary`, `iommu-aarch64-uefi-gicv2`,
`grant-vocabulary`, `dependency-policy` and `iommu-aarch64-direct-gicv3-its` (its transition half).
Those boot the built system on aarch64 and riscv64 with the drivers this work changed.

NOT PERFORMED, AND WHY. The sweep was stopped part way through the remaining gates: a single aarch64
IOMMU gate takes over an hour under TCG on this machine, and the rest of the list is dominated by
guest boots of the same kind. What is therefore still owed for the seven maintenance items is the
suite on all three targets and, for virtio-blk, the isolation gate's traffic phase - which is why
none of them is ticked.

THREE GATES ARE RED AND NONE OF THEM IS THIS WORK'S.
`foreign-facilities-guest` refuses because `abiprobe` is not staged: it is `producer = "audit"`, so
the foreign static substrate has to be built first, and that substrate is P02M0135's - its files were
still uncommitted when this session began and were committed as `8e33259d` while it ran.
`boot-harness` fails on two assertions in `src/harness/harness-test.py` about `lab.image_command()`
carrying `--dma-mode harness`: the helper passes the flag and the test does not expect it, and both
files are untouched by this work.
`qemu-arch-profiles` fails its `no-dt-absent` case - the loader loads a kernel after refusing the
absent DMA-mode record - in loader and image code this work does not touch, and consistent with the
same unfinished DMA-mode change the `boot-harness` failure names.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-13T02:20:00Z):

THE LAST CONSUMER CHOSEN BY A DRIVER'S NAME IS GONE.

`virtio-console maintenance` owed the `console-bytes` provider's migration onto the catalogue, and
that item is now implemented. What was there, read out of the code rather than out of the intent:

  - `route_offers` called `Catalogue::take_from`, which LIFTED the publication out of the catalogue,
    compared the binding's artifact name against the literal `dev_channel`, started the development
    agent on the channel it had taken, and CLOSED the publication of any other driver that offered
    the same kind. So the one provider this machine publishes could be shown to nobody, a second
    publisher was silently discarded, and a provider bound after that pass had no path to a consumer.
  - The wire was untyped messages in both directions. An EMPTY message meant the port would not take
    a write; a handle under a `BYTES` tag on the driver's own bootstrap meant a replacement consumer;
    the version was not on the wire at all. Three conventions, each remembered separately by both
    ends.
  - `virtio_console` declared `provides = [{ kind = "console-bytes", most = 1 }]` and publishes no
    provider at all.

WHAT IS THERE NOW:

  - `liber:device@1` carries `console-stream`: `attach(version) -> console-attachment` settles a
    version and a frame bound before a byte moves, `write(bytes) -> u32` answers with what the port
    took (`again` is a host that stopped reading), and `receive() -> stream<console-chunk>` grants
    the inbound endpoint. Regenerated with ./gen.sh; `./gen.sh --check` reports no drift.
  - `dev_channel` serves that contract over `common::Serving`, so a replacement consumer arrives as
    an ordinary `CONNECT` and a departed one is reported with `DISCONNECT` - which is what frees the
    single-consumer slot the next `open` is checked against. The `BYTES` tag and the `adopt` loop
    that waited for it are deleted.
  - `dev_agent` holds a `provider-catalogue` connection, subscribes to `console-bytes`, opens what it
    finds, settles the version, asks for the stream, and holds the provider's identity - slot,
    provider generation, binding generation. A withdrawal naming that identity detaches and ends the
    session it carried; a later publication attaches. A replacement agent does all of this from
    scratch, so DeviceManager no longer re-wires one.
  - DeviceManager starts the agent ONCE, after phase two, because the image is a development image -
    not because a driver with a particular name bound. `Catalogue::take_from` has no caller left in
    any image and is removed.
  - The manifest no longer claims a provider `virtio_console` does not publish.

WHAT IS DELIBERATELY NOT DONE, AND IT IS RECORDED IN THE PLAN AND THE MANIFEST: a SECOND
`console-bytes` publisher would be indistinguishable from the development channel to a consumer that
selects by kind, because both would speak the same version. Telling two publishers of one kind apart
needs a ROLE the registry declares and the catalogue carries, and nothing in this tree has one. The
first item that adds a second console byte stream owes that role before it can close its gate.

THE DECISIONS ARE SHARED AND HOST-TESTED, AND EACH WAS WATCHED TO FAIL. `driver_protocol::console`
holds version agreement, publication identity, the cutting of a long frame and the meaning of a
refused write, in the crate both ends already link. Seven tests, and the mutations:

  - identity compared by slot alone -> `a_withdrawal_that_names_another_provider_does_not_detach_this_one` FAILS
  - a partial write counted as a whole one -> `a_partial_write_loses_the_provider_and_a_backpressure_refusal_does_not` FAILS
  - a version mismatch downgraded instead of refused -> `a_version_that_was_not_asked_for_is_refused_rather_than_downgraded` FAILS
  - a span that stops one byte short -> `a_frame_longer_than_the_bound_is_cut_into_spans_that_cover_it_exactly` FAILS

  Reverted, 34 of 34 pass.

VERIFIED: `cargo test` over driver-protocol (34 tests, green); `./build.sh --part user` for x86_64 in
BOTH profiles - ordinary and LIBER_DEVELOPMENT=1 - green; `./gen.sh --check` green; `format.sh` run.
NOT PERFORMED: the three-target suite run, and the guest boot that would exercise the new wire end to
end. The item is therefore NOT ticked. Nothing about the migration has been observed in a running
guest yet; what is proven is that it compiles for the shipped image and that its decisions refuse
what they say they refuse.

AND A MARKER WAS REMOVED FROM THE INDEX. `docs/todo/TODO.md` carried this document as `[i]`, a state
`check-milestone-index.sh` counts as neither open nor done and refuses to let anything tick. The
project owner has said plainly that a marker meaning "skip" was never agreed to. The row is `[ ]`
now, the status line no longer calls the document non-completable, and the gate passes. It closes
when its items do.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-13T03:20:00Z):

VIRTIO-RNG, AND THE ONE CONCLUSION THE MACHINE WILL NOT DRAW FROM IT.

`SYS_RANDOM_GET` had exactly one answer on a machine with no hardware random instruction:
`ERR_UNSUPPORTED`. That is two of this system's three architectures, so anything wanting key material
had nowhere to go and the only call that always answered was the one named `insecure`. This item
closes that, and the whole of the care is in not closing it too far.

WHAT IS THERE:

  - `entropy::Pool`, a crate outside the kernel for the reason `driver-binding` is one: the kernel is
    not host-testable and these are decisions. A paravirtual submission is credited at a QUARTER of
    its length, a hardware one at a half, no single submission is worth more than 512 bits however
    large, the pool holds at most 4096 bits, and below 256 credited bits `draw` answers NOTHING.
  - `SYS_ENTROPY_ADD`, whose authority is the device capability checked against the CURRENT claim on
    a function whose device type is the entropy device's. The submitter hands over BYTES and the
    KERNEL decides the credit - a driver that could name its own credit could seed a machine to
    "fully seeded" with a constant.
  - `SYS_ENTROPY_HEALTH`, which answers credit and provenance and never a verdict.
  - `driver.virtio-rng`, which publishes NO provider: what it produces goes to the pool, and a
    channel handing out raw host bytes is the one thing it exists not to be.
  - `random_into` draws from the pool when there is no hardware instruction and the pool is seeded.

EVIDENCE, AND WHAT IT IS WORTH:

  - Eight host tests in `entropy`, green. The deterministic-injection case pins a draw computed by an
    INDEPENDENT SHA-256 - the construction written out against Python's `hashlib` - so it asserts the
    chain this pool documents rather than self-agreement.
  - Two kernel tagged tests, compiled into the test binary and confirmed present by `nm`: a
    capability that names no binding is refused, the submission bounds are refused before the
    capability is looked at, and a machine with no instruction refuses the secure draw until the pool
    is seeded and answers afterwards.
  - `./build.sh --part kernel` and `--part user` for x86_64, green.

NOT PERFORMED: the guest run. The harness now attaches `virtio-rng-pci` on all three targets in test
mode, and no guest has yet booted with it. The two kernel tests have not been RUN, only built. The
item is therefore not ticked.

A JUDGEMENT THAT SHOULD BE READ BY SOMEBODY ELSE: the credit rates - a quarter and a half - and the
256-bit threshold are this implementation's choices. They are conservative relative to what Linux
credits a virtio-rng source, and the plan asked for conservative. They are not derived from a
measurement, because nothing inside a guest can measure them.
