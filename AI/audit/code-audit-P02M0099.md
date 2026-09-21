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

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-13 19:05):

THE FADT AND GAS PARSER, as `src/acpi`. A shared-library item, closed the way the file's completion
section says one closes: on host tests and hostile fixtures, with no bind, no device and no claim.

WHY IT IS A CRATE AND NOT A KERNEL MODULE. Three fixed-hardware items follow this one - event
delivery, the power button, and the battery/AC/thermal classes - and each would otherwise read the
FADT with its own offsets and its own idea of which fields its revision may be believed about. It is
`&[u8]`-shaped rather than `phys_to_virt`-shaped like `fdt`, because everything that reads a FADT
reads it through a mapping that already exists, and because that is what makes a hostile fixture a
byte array in a test rather than a mapping a test has to fake.

WHAT THE 22 TESTS HOLD. One is the well-formed table, and it is the least interesting. The rest are
fixtures that are wrong on purpose: a length below the header, a length past the buffer, a length no
real table has, a checksum that does not sum, another table's signature, a revision-1 table asked for
revision-5 fields, a revision-6 table truncated before its sleep registers, an access size of five, an
address with no width, a width with no address, an address at the top of the space with a width that
runs past it, a machine that declares a reset register and no support for it, a machine with no CMOS
clock and a century index anyway, a zero SMI command port, and a hardware-reduced machine whose fixed
hardware fields are filled in and must be ignored.

THE ORDER OF THE REFUSALS IS PART OF THE CONTRACT and one test says so: an absurd declared length is
refused BEFORE the checksum walks it, because walking four billion bytes is the work the bound exists
to prevent.

THREE PIECES OF DRIFT WERE FOUND AND FIXED WHILE REGISTERING IT, all of them mine from earlier in this
job and all of them reported by `verify-model check` rather than noticed:
  - `check.sh` ran the `run-verdict` gate and the catalog did not know it, so nothing would ever have
    selected it. It is registered now, with its subject.
  - `host.graphics-proto` was derived as release-required and was not in the frozen set.
  - `host.acpi` needed the same entry, which is what a new crate with tests always needs.
`verify-model check` is clean, and its own 157 tests are green.

NOT PERFORMED: nothing binds this parser yet, by design. The first consumer is the ACPI fixed-hardware
event item, which is an architectural-prerequisite item and is not this one.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-13 19:40):

HID OVER I2C, THE PROTOCOL HALF, as `src/user/libs/driver/hid-i2c`. The second shared-library item
closed today, and closed the same way: on host tests over a fake device, with no bind.

THE BUS CONTRACT IS THE PART WORTH REVIEWING. This item is the first implemented consumer of an I2C
bus in this tree, so by the milestone's own shared-contract rule it states the contract - and the
temptation was to state a controller. It states three operations instead: `write`, `read`, and
`write_read`. The third is there because a register read IS a write-then-read with no stop between
the two halves, and an implementation that offered only the first two would force every caller to
split the transaction where another master can act.

WHAT THE 13 TESTS HOLD, and the shape is the same as the ACPI parser's: one test is the well-formed
case and the other twelve are devices behaving the way real ones do when something is wrong. A device
still in reset answering with thirty zero bytes. A maximum input length of zero, of one, and of one
past what the bus carries. A report descriptor of zero length. A report whose declared length is
longer than what the bus returned. A bus that stops four bytes short. A report id of 15 and of 200,
which take the escape form. A power transition, asserted on the BYTES rather than on the call
returning `Ok`.

THE ONE DESIGN DECISION THAT COULD HAVE GONE THE OTHER WAY: `Device` does not own the bus. A touchpad
shares its bus with every other device on it, so a type that owned one could not be built twice; the
bus is passed to each operation instead. That is also what let the fake device be the bus in the
tests.

NOT PERFORMED: the binding half. It needs an I2C controller driver with a real fixture and the
firmware-node device identity, and neither has an owner - which is precisely why the milestone splits
the two and why this half could close today.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0099 (2026-09-13 21:00):

TWO MORE ITEMS, AND ONE OF THEM IS A DECISION THE FILE HAD BEEN CARRYING SINCE AUGUST.

THE EDID PARSER AND THE MONITOR VOCABULARY, split from its transport by this file's own rule. DDC is
I2C on the display connector; there is no I2C controller in this tree and no display driver that can
reach one, so the FETCH has no event that closes it and the PARSE does. `src/user/libs/display/edid`,
13 host tests, and the transport half is now a named blocked item rather than half of a closed one.
The parse is the ordinary half; the vocabulary is the half worth reviewing. `PhysicalSize` has three
answers because a compositor that computes a DPI from an undefined size divides by zero. `VideoInput`
answers `None` for a bit depth on EDID 1.3, where the byte carried none, rather than reading the
analogue flags as a colour depth. `Manufactured` separates a week from a bare year from a MODEL year,
because week 255 is not a week. And a timing is refused before it is arithmetic: no active pixels, no
blanking (the refresh rate divides by active plus blanking), or a sync pulse that runs past the
blanking interval it lives in.

THE USB EXECUTION MODEL, DECIDED AND IMPLEMENTED. The first answer: a class driver is a module inside
the controller's process and Domain, holding no claim and no Domain of its own. The second answer -
one binding unit per USB interface - is a sub-function identity, the same cross-cutting case as the
firmware node, which P02M0163 already refused and which would have put every USB class item behind an
unowned prerequisite.

AND THE MECHANISM IS WHERE THE REAL FINDING WAS. Writing the isolation clause down meant looking at
what the two class modules actually hold, and both were unbounded:
  - `Hids::entries` is a `Vec` with no ceiling. A tier of hubs full of keyboards configures one HID
    module entry each, and each entry is an endpoint and a DMA page of the CONTROLLER'S, held for as
    long as the device stays plugged in.
  - `Storage::fit_data` grows the data buffer to the largest request ever made and never shrinks.
    The bound on a DMA span inside the controller's Domain was therefore whatever the largest transfer
    anybody had asked for.
`drivers::usb_class` is the budget: eight HID devices with their endpoints, rings and in-flight
reports; one mass-storage device with two endpoints, two rings and a data buffer bounded at a
megabyte. Charged when a module takes a device, given back when the port goes away, in the same two
places that already allocate and free the pages - so the count cannot drift from the memory.

THREE DETAILS THAT WOULD HAVE BEEN WRONG THE OBVIOUS WAY. The charge is taken when the module TAKES
the device and not when the admission is asked for, because `configure_hid` answers `None` for
anything that is not a HID and charging that would spend the keyboard budget on every device on the
bus. A refused device is left ADDRESSED and in the inventory rather than released, so a later detach
still gives its pages back. And the refusal is only PRINTED for a device whose class byte could
plausibly belong to the module, because "the keyboard budget is full" about a printer is a lie.

EVIDENCE: 8 new host tests (drivers 45 total, green) at each ceiling and one past it, including a
hundred plug/unplug cycles ending where they started and sixteen refusals costing nothing; the guest
`usb` and `storage` tags pass 59 tests over the real emulated controller.

NOT PERFORMED: the per-class detach-under-load case and the budget-exhaustion refusal AS A GUEST
observation. Both need a QEMU fixture that adds and removes USB devices while traffic is in flight,
which the harness has no mechanism for today; the budget's own behaviour is held by the host tests
above, and the gate contract that names those two cases belongs to the first in-controller class
module item rather than to this decision.

IMPLEMENTER'S EVIDENCE RUN ON P02M0099 (2026-09-14 04:20):

THE THREE-TARGET SUITE HAS RUN, AND FIVE MAINTENANCE ITEMS WERE WAITING ON EXACTLY THAT.

    x86_64    394 tests   225 s   KVM
    aarch64   382 tests  3477 s   TCG
    riscv64   385 tests  4102 s   TCG

plus the 48 host tests in `src/user/drivers/core`, whose per-module counts are the ones the gate
clauses name: `net` 3, `input` 4, `snd` 5, `gpu` 8, `blk` 4 with `virtio` 5, and the xHCI four -
`usb` 4, `usb_class` 8, `descriptor` 4, `port` 3. Ticked: virtio-net, virtio-input, virtio-console,
virtio-snd and xHCI.

AND THE RUN FOUND A REAL DEFECT, WHICH IS WHAT A THREE-TARGET RUN IS FOR.
`kernel.kernel.a_secure_random_syscall_refuses_rather_than_answering_from_a_formula` failed on
aarch64 and on nothing else. The assertion said "a machine with no hardware source refuses rather
than substituting", and that WAS the contract until the seeded entropy pool arrived as the second
answer - the reason most of this system's machines can answer `SYS_RANDOM_GET` at all, since two of
its three architectures have no instruction. The syscall answered from the pool on aarch64 while the
test still demanded `ERR_UNSUPPORTED`, and the disagreement was invisible on x86_64 because `RDRAND`
takes the first branch there. The test now states the contract in three cases - hardware, no hardware
with a seeded pool, neither - and what is refused is still the thing that matters: no hardware AND no
pool means a refusal and an untouched buffer rather than a formula under a name that promises
otherwise.

TWO MAINTENANCE ITEMS ARE NOT TICKED AND SAY WHY.
  virtio-blk  needs `check.sh --gate qemu-virtio-iommu-x86_64`, whose preconditions did not come
                together: the gate requires an ISO keyed to this tree AND, inside itself, a
                `./test.sh` that accepts the build. The shipping ISO is signed `enforcing-required`
                and is keyed over the bootable volume, so the volume has to be signed that way for
                the key to match - and with that volume in place `test.sh` refuses the build as not
                matching the sources. Each half is reachable and not both at once. It is a procedural
                knot in the gate's own preconditions rather than anything about the driver, it
                predates this session's work, and it is left for whoever owns that gate.
  virtio-gpu  needs the display endpoint under translation on P02M0173's profiles, which is the
                per-profile IOMMU gate set rather than the suite.

IMPLEMENTER'S DEBT CLOSURES ON P02M0099 (2026-09-14 05:10):

SIX MAINTENANCE ITEMS AND EIGHT DEBT ROWS CLOSED, against evidence that has actually run.

  virtio-blk      `check.sh --gate qemu-virtio-iommu-x86_64` green - the controller out of bypass,
                    five hostile cases refused by the hardware, a DHCP lease through the enforcing
                    controller, and THE SYSTEM VOLUME READ THROUGH virtio-blk AT 00:04.0 behind the
                    controller, which is this item's own oracle
  virtio-net, virtio-input, virtio-console, virtio-snd, xHCI, virtio-rng
                  their host tests plus the three-target suite

  debt rows       DRV-001, DRV-002, DRV-003/WIRE-002, DRV-005, DRV-006, DRV-007/-008, DRV-009 and
                    DRV-012, each ticked against the item whose gate names it

THE GATE'S PRECONDITIONS ARE AN ORDER AND NOTHING SAID SO. `qemu-virtio-iommu-x86_64` needs a
shipping ISO keyed to this tree AND, inside itself, a `./test.sh` that accepts the build. The volume
has TWO shapes with TWO receipts - `built-x86_64-volume-test` for the shape the suite boots and
`built-x86_64-volume` for the bootable one the ISO is keyed over - so the order that satisfies both
is `./build.sh --arch x86_64`, then `./image.sh --format iso --dma-mode enforcing-required`, then the
gate. The other order leaves the test receipt stale and the gate fails INSIDE its own `./test.sh`
with "the build does not match the sources", which reads like a build problem and is an ordering one.
It cost an afternoon; it is written into the virtio-blk item so it costs nobody else one.

WHAT IS LEFT OF THE MAINTENANCE SET IS ONE ITEM, AND IT IS NOT WAITING ON A RUN. virtio-gpu's gate
asks for the display endpoint under translation on P02M0173's profiles, and those profiles DROP the
display device by design: `check-qemu-iommu-ports.sh` boots a reduced machine because the full
interactive one does not finish attach-and-map inside DeviceManager's boot window on an emulated
port. The proof cannot be obtained by running those gates as they stand; it needs a display phase
added to them, with that boot-window cost measured rather than assumed. Recorded in the item.

## 2026-09-14 - HID defects, and why they survived (implementer note)

DRV-011, DRV-013 and DRV-014 are closed, each with a fixture that FAILS against the code as it was.
The overflow one is worth recording in full because the fixture proves it: with the old
`*cursor + bits <= MAX_REPORT_BYTES * 8` the test panics with `attempt to add with overflow` at
`hid.rs:275`, and with `cursor.saturating_add(bits)` it passes. In a release build the same
expression wraps to a small number and ADMITS the segment the check exists to refuse.

**Why all three survived: the parser lived inside a binary.** `hid.rs` was `mod hid;` in `xhci.rs`,
so nothing could reach it from a host test - there was no seam at all, not a missing test. It is now
`drivers::hid`, which is also where it belongs on its own terms: a report descriptor is not
transport-specific, and the I2C and Bluetooth HID bindings this roadmap lists will want the same
parser. `drivers` gained `extern crate alloc` for it.

Two smaller things went in with them:

- **`hid::remember`** is now the single place the previous-report state is written, so the
  zero-the-tail rule has one implementation and one test rather than living inline in the xHCI
  driver's event path where nothing could see it.
- **`Mods::release_all`** exists for the other half of DRV-014: a device that goes away with a
  modifier down otherwise leaves it down for ever, and the next thing typed arrives shifted. Nothing
  calls it yet - the disconnect path is the xHCI driver's and is not in this change - and it is
  written so that when that path lands there is a correct thing to call.

**DRV-015 closed on its own stated trigger**, which is a threat-model statement rather than a driver
change, and `docs/THREAT_MODEL.md` already carries it in three places: section 2.2 on what an
untranslated DMA capability actually hands a driver, the paragraph on the boot mode being stated
rather than inferred (so the limit is never silent), and section 5's non-goal naming the
architectures and backends that are not claimed.

**What is left of DRV-010 is a GATE PHASE, not a driver change.** The display half is implemented and
tested; the item waits on a display phase in `check-qemu-iommu-ports.sh`, which drops the display
device by design because the full interactive machine does not finish attach-and-map inside
DeviceManager's boot window on an emulated port. That is gate work with its own boot-window cost to
measure, and it is the one thing standing between this row and closed.

## Implementer note - 2026-09-14, DRV-004, DRV-010 and the display phase

DRV-004 closed on its stated trigger - the first item that changes a service protocol's handle
handling, which was the display service's move to client-supplied images and attenuated replies. The
survey behind it is worth keeping: every one of the fifteen hand-written multi-capability receives in
the tree already closes what it did not adopt, and `Reader::finish` refuses a request whose signature
does not account for the handles it carried, because it checks `taken == count` as well as the byte
position. What was missing was a FIXTURE, and there are now two: a host one in `display-proto` that
sends the same well-formed request bare and with a capability attached, and a guest one that requires
the smuggled channel's peer to be CLOSED - the half a host fixture cannot show.

DRV-010's finding is recorded closed where the finding lives; what stays open is `virtio-gpu
maintenance`'s gate clause, which asks for the display endpoint under translation on the port
profiles. That phase now EXISTS: `check-qemu-iommu-ports.sh` has a `display` phase on the two UEFI
rows, `DMA_DISPLAY=1` is the reduced machine plus exactly one endpoint, every phase reports its boot
window so the added endpoint's cost is measured rather than assumed, and the oracle is a frame
reaching the display rather than a driver reporting online. It is an emulated port boot and belongs
with the other slow gates at the end of a batch.

THREE GATES WERE RED BEFORE THIS WORK AND ARE NOT NOW, all of the same shape - a fixture that had not
followed the script it tests:

- the guest-case inventory named none of the twelve virtio-iommu port rows, neither DMA-mode row,
  neither no-DT-absent row, the evidence gate or twenty-one cases. It said "all nine" and "all
  sixteen" in prose, and the count agreed with nothing, so nothing disagreed with it. The numbers are
  gone and the rows are written out.
- `check-perf-anchor.sh`'s sandbox lacked `evidence.sh`, the services manifest, `stage-kernel.sh` and
  the volume's `dma-mode` sidecar, and its negative control patched a line `mkimage.sh` no longer
  contains. Each failure read as the gate's subject rather than as the copy list.
- `test-kernel.sh` installed its cleanup trap AFTER sourcing the evidence machinery, so anything that
  failed in between leaked that run's staged kernel into `.build/state`. The trap is now armed at
  staging and the publishing is added to it, and publishing cannot abort the cleanup.

And two staged components had neither an oracle nor a stated reason: `font_catalogue`, which the text
guest gate really does exercise and now names, and `virtio_rng`, which nothing starts - recorded as
the gap it is.

## The written plan for virtio-serial multiport (2026-09-15)

The milestone's own rule - NO ITEM IS STARTED WITHOUT ITS OWN WRITTEN PLAN - is why this exists
before any code. Three facts in this tree decide the shape and none of them is in the item's
sentence:

1. THE HARNESS ALREADY PAYS FOR THE MISSING FEATURE, and says so. `qemu-run.sh` attaches the
   development channel as a `virtconsole` rather than a `virtserialport` because without MULTIPORT
   there is no control queue to open a generic port with - measured, per its own comment - and the
   price is a UEFI firmware preamble on the channel that the framing above it skips. The feature is
   not new capability for its own sake: it retires a workaround that is already documented.
2. THE DESTINATION IS NOT A NEW SERVICE. `ProviderKind::ConsoleBytes` is published by DeviceManager
   and subscribed by the development agent. What is missing is SELECTION: `ProviderInfo` has no name
   field, so two open ports are two indistinguishable providers - and that field, or the ninth
   provider kind the milestone's head allows, is what the implementer owns.
3. THE QUEUE INDEX RULE IS THE SPECIFICATION'S. Port 0 keeps 0 and 1, control is 2 and 3, port n is
   `2n + 2` and `2n + 3`. Numbering them otherwise produces a device that answers nothing, which
   presents as a dead port rather than as a wrong index.

AND THE GATE IS NAMED CONCRETELY, which is what the milestone demands of a gate: one
`virtio-serial-pci` carrying BOTH a `virtconsole` and a `virtserialport,name=org.libersystem.dev`,
each on its own chardev socket, with the observable effect being bytes written to the NAMED port
arriving on that port's socket and on no other. A single-port driver cannot produce that effect at
all, which is what makes it an oracle rather than a liveness check.

## virtio-serial multiport: the control half has landed (2026-09-15)

`VIRTIO_CONSOLE_F_MULTIPORT` is negotiated, the control queue pair is set up, `DEVICE_READY` goes
out and every `PORT_ADD` is answered with `PORT_READY` - which is what an unanswered announcement
costs: a port the device will never open. The test machine now carries a `virtserialport` named
`org.libersystem.diag` beside the `virtconsole` on one `virtio-serial-pci`, and the driver's own line
reads `multiport 2/2` against `multiport 1/1` on the machine's other console function.

THE COUNT IS THE ORACLE AND NOT A LIVENESS LINE. A single-port driver cannot see the second port at
all - there is no control queue to learn about it through - so "two ports found, two open" is an
effect that only the feature produces. That is the shape this milestone's gate rule asks for.

THE DECISIONS ARE IN A MODULE WITH FIXTURES, because every input is bytes the device chose:
`drivers::console` holds the queue-index rule, the message, the port state machine and the refusals.
Its seven tests are the hostile half - four thousand announced ports, a message shorter than its
header, an undefined event, a name longer than the buffer, a repeated open, and the invariant that no
port ever claims the control queue's indices.

AND THE HANDSHAKE IS BOUNDED IN TWO DIRECTIONS: a round count, and a quiet count that ends it when
the device stops answering. A driver that waited for a device that never speaks again would never
report at all, which is a worse failure than a port nobody can use - and the console it already has
keeps working either way.

WHAT IS LEFT: the per-port byte pump, one `ConsoleBytes` provider per open port carrying the port's
NAME (which `ProviderInfo` has no field for - that is the IDL change the plan names), and the gate
that reads bytes back off the named port's own capture file.

## The name on the wire, and the gate that reads a port's own chardev (2026-09-15)

TWO PROVIDERS OF ONE KIND ON ONE DEVICE IS WHY `ProviderInfo` GAINED A NAME. A virtio-serial device
with a console port and a named diagnostic port publishes two `console-bytes` providers whose every
other field is identical - same address, same binding, same kind - so a consumer asking for the kind
gets whichever the catalogue hands it. The name travels from the driver's own offer
(`encode_offer_named`, bounded at forty-eight bytes) through DeviceManager's offer table and the
catalogue entry into every snapshot and subscription frame.

AND THE DECODE HAD TO CHANGE SHAPE, not just gain a field: `decode_offer` refused any payload that
was not exactly four bytes, so a named publication from a newer driver would have read as a CORRUPT
frame to an older manager rather than as a publication whose name it does not understand. It now
takes anything from the header up to the header plus the bound, and refuses past it.

THE GATE IS `./check.sh --gate virtio-multiport`, and what makes it an oracle rather than a liveness
check is the chardev it reads: the test machine's `virtio-serial-pci` carries a `virtconsole` and a
`virtserialport,name=org.libersystem.diag`, each with its own capture file. The driver writes one
line to the GENERIC port, and the gate requires it on that port's capture and requires its ABSENCE
from the console's. A single-port driver cannot see the port, open it or write to it - there is no
control queue to learn about it through - so neither half of that is producible without the feature.
The console's own banner is asserted in the same run, because a feature that cost the port that ships
today would not be worth having.

WHAT IS LEFT for the item: the per-port byte PUMP - a receive pool, a transmit path and one provider
per open port served to a consumer the way the development channel's single port is served today. The
write direction and the name are done; the duplex stream is not.

## The per-port byte pump, and the port module both drivers now share (2026-09-15)

THE PORT IS A LIBRARY NOW, AND IT WAS MOVED RATHER THAN COPIED. `drivers::serial_port` holds what
one virtio-serial port served as a byte stream IS: the receive pool's slot count and size, the
bounded transmit wait and why its deadline is the session's rather than a shorter guess, the typed
refusals (`again` is a host that stopped reading), the `console_stream::Service` implementation, the
receive drain and the backpressure that answers the manager's pings while a consumer is behind. All
of it came out of `dev_channel.rs`, which is where it was written, and none of it was duplicated
into the console driver. That was the one duplication worth refusing outright: two copies of a
byte-stream contract diverge invisibly, because mismatched bytes are refused nowhere - they arrive
somewhere else in the reader. The dev-channel driver had said as much in its own header, that a later
multiport driver should carry the same protocol on a named port without anything above it changing.

`Stream` IS THE UNIT, AND THAT IS WHAT MADE THE SHARING POSSIBLE. One served port is both queues, the
receive pool behind one and the transmit buffer behind the other, plus one consumer's session. A
driver with a single port could keep those as six locals and did; a driver with N of them cannot, and
the pieces are not independently useful - a receive pool with no attachment to hand bytes to is bytes
discarded. Bundling them is also what makes "one port" the thing a loop iterates over.

`Port` OWNS ITS TRANSMIT QUEUE NOW instead of borrowing one. A multiport device has one queue per
port, and a set of borrows of one device's queues is a set no single scope can hand out. The queue is
plain data - a ring's addresses and two indices - so moving it into the only thing allowed to drive
it costs nothing.

THE STAMP GOES OUT ON THE ASYNCHRONOUS PATH, and this was a real trap rather than a tidiness. The
synchronous `Queue::submit` busy-polls the used ring without touching the indices `take_used`
accounts against, so one synchronous write followed by an interrupt-driven pump leaves the reaper
looking at a completion it was never told to expect - `check_used_advance` would refuse it. The stamp
now uses `Port::send_now`, the same submit the first real write uses, and the pump's first `reclaim`
takes its completion like any other.

THE CONSOLE PORT IS NOT PUBLISHED, deliberately. Port 0 carries the guest's console and the banner;
a second writer would interleave with both and a consumer reading it would be reading the boot log.
What is published is the GENERIC ports the control queue opened, capped at the bring-up protocol's
four publications per handshake - and the report says how many of the open ports that was
(`multiport 2/2/1` is announced over open over served), so a port that is open with nothing able to
reach it is visible rather than inferred. That distinction matters here because it is exactly what
this driver was when only the control half had landed.

THE CONSOLE DRIVER TAKES AN MSI-X VECTOR NOW, which is a DeviceManager change and not a driver one:
the interrupt-driven list was a name list in `bind`, and `virtio_console` joined it. A byte stream
must block on arriving bytes; polling one would spin for the guest's whole life, and under a
cooperative scheduler a runnable spinner starves every thread that still has boot work. A binding
that granted no vector leaves the driver publishing nothing and the console working exactly as it
did, which is the honest degradation rather than a wait on a handle that is not one.

THE REGISTRY DECLARATION CAME BACK, and the comment that removed it said what it was waiting for:
"telling two publishers of one kind apart needs a ROLE the registry declares and the catalogue
carries, and nothing in this tree has one yet". The name is that role. `most = 4` is the protocol's
bound rather than a count of any machine's ports, and `consumers = 1` because a byte stream divided
between two readers is bytes arriving at whichever drained first.

AND THE CONSUMER SELECTS BY NAME. `driver_protocol::console::selects` is the decision, host-tested:
exact bytes, an empty selector matching anything, an empty name matching only that. A prefix rule
would attach the development agent to `org.libersystem.dev2` and a case-insensitive one to a port
some host named in capitals, both silently. The development channel publishes under
`provider::DEV_CHANNEL_NAME`, read by the driver and by the agent from the same constant, because a
name each end spells for itself is two names.

`MAX_PAYLOAD` WAS TWO BYTES TOO SMALL and nothing had found out. It was `BIND`'s payload alone; a
named `OFFER` is four header bytes plus a forty-eight-byte name, which is fifty-two against fifty. A
publication named to the bound would have been built into a frame buffer too short, and the failure
would have been a panic in the sender rather than a refusal anywhere. It is a maximum over the
opcodes now, with a fixture that asserts the property rather than today's answer.

FOUR GATES FOR THIS EXACT CODE COULD NOT RUN AT ALL, and they are repaired in the same change.
`check-provider-catalogue`, `check-driver-connections`, `check-no-fixed-provider-slots` and
`check-audio-provider-recovery` extract production functions by signature and compile them against
host doubles. Three locators carried an `unsafe` the functions no longer have; one mutation locator
had lost a level of indentation when its loop left a nested block, so a defect the gate claims to
reject was not being injected at all; one double was missing a field the extracted code reads; and
one `ProviderInfo` literal predated the name. Each failed as a Python traceback or as a compile error
about somebody else's function, which reads like a broken tool rather than like an unverified claim -
which is why they had stayed broken. Two `impl Provider` blocks in one file were also merged into
one, because the extraction takes the first and the first had become the wrong one.

THE SERIAL CONTRACT IS DEFINED, because this was the first of its three claimants to be implemented
and the milestone's rule says the first one answers for all three. The byte stream is the BASE -
`console-stream`, with no baud, parity, stop bits, DTR, RTS or break in it - and the line coding and
control lines are a SERIAL LAYER above it, a separate interface with its own provider kind owned by
the first of 16550, PL011 and CDC-ACM that actually needs one. The reason is not symmetry: a
virtio-serial port has no line coding at all, so a common contract carrying baud would make every
virtio port answer a question it has no answer to, on every operation, for ever.

VERIFIED: `./check.sh --gate virtio-multiport` green with the served count and the catalogue's
published-provider count added to it; `./check.sh --gate qemu-2d-demo` green; the whole x86_64 guest
suite at 399 passed with the console driver serving consumers where it used to stand.

## Two development gates repaired, and one that is red for a reason outside this work (2026-09-15)

`development-build` DEMANDED PROGRAMS NO `cargo build` CAN PRODUCE. `abiprobe` and `vkprobe` are
`development = true` AND `producer = "audit"`: what is staged for them is linked against a pinned
upstream this tree deliberately does not carry, and `build-shared.sh` deletes them from any image
that is not the development one. The gate builds the services and drivers crates and then asserts
every development-only program appeared - so it failed for a tree behaving exactly as its manifest
describes. Audit-produced programs are now excluded from that inverse check, and only from it.

`development-gate` DECLARED ITSELF BROKEN, WHICH IS THE ONE MESSAGE THAT STOPS WORK, and it had three
causes stacked on each other. It re-execs itself to prove its own refusals, passing an injected
manifest; the manifest travelled as an ENVIRONMENT VARIABLE, and Linux caps a single environment
string at `MAX_ARG_STRLEN` - thirty-two pages, 128 kB, not the total `ARG_MAX` and not raisable -
which the exported registry has outgrown at 153 kB. It also resolved `"$0"` AFTER `cd`-ing away from
where it was invoked, so every re-exec from a relative path named nothing. Both failures surface as
the same self-test message, and neither is about the manifest. The injection travels in a file now
and the script resolves its own path first. Underneath them was a real check defect: the
`required-features` test read four lines after the program's name, and `dev_channel` has two lines of
comment saying WHY it is gated before the attribute - so a comment could turn the gate red. It reads
the whole `[[bin]]` block.

`development-lifecycle` IS RED AND IT IS NOT THIS WORK. The development IMAGE build fails at
`verify_staged_selection_candidates`: `abiprobe` and `vkprobe` record the digest of the `icdprobe`
they were linked against, and the staged `icdprobe.lslib` is a different one. Neither side is wrong -
`icdprobe.c` has not changed, but the runtime it links against has, and the pass-2 audit link under
`.build/foreign/pass2` is frozen at 2026-09-13. Re-running that link is what reconciles them, and it
needs the pinned upstream, so it belongs to the foreign-artifact track rather than here. The
consequence worth knowing: while it is red, no `harness/scenarios/` replay runs, and the development
image cannot be built at all - which is why `dev_channel` and `dev_agent` were verified in this
change by compiling the development configuration directly rather than by booting it.

## The boot surface's element size, and a unit conversion nobody checked (2026-09-15)

TWO DESCRIPTORS FOR ONE SURFACE, IN TWO UNITS. `bootproto::Framebuffer` states the element size in
BITS and the `abi::Framebuffer` the kernel hands userspace states it in BYTES, so there is a division
between them - and it was `fb.bpp / 8`, written twice in `kernel/main.rs`, unchecked at both.

NOTHING TRUNCATES TODAY AND THAT IS THE POINT OF WRITING IT DOWN. Every producer supplies a multiple
of eight: the UEFI path derives the element size from the channel masks and ROUNDS UP to whole bytes,
and the two ramfb paths hard-code thirty-two. The division is exact on every machine this system
boots. What was wrong is where the check was not - `BootInfo` is a wire between two SEPARATELY BUILT
artifacts and the kernel is its READER, not its author. A reader that divides a number it did not
produce without asking whether the division is exact is one loader version away from a stride a whole
byte short of the one firmware described, and that is a diagonal smear rather than a picture. No test
on a matched loader-and-kernel pair would ever show it.

`element_bytes` IS THAT ONE PLACE, and it refuses rather than rounds. A surface whose element size
this system cannot state is not a surface it can draw into, and the honest answer is the one a
machine with no video mode gets: serial only. The fixture asserts the refusals, the live machine's
own answer - so a change that made this refuse a real boot surface fails there rather than on a blank
screen - and that this boot's pitch holds a whole row of the element size it claims, which is the
other half of a descriptor whose two halves could describe different surfaces.

IT IS ONE THIRD OF ONE OF THE THREE THINGS THE GOP ITEM'S PLAN NAMES, and the item stays open. The
descriptor MERGE is not done, the early display is still a syscall rather than a provider, and the
cache policy is still unstated. What the plan is for is that none of those three has to be
rediscovered - and that the loader's mask checking, which IS done, is not redone by whoever takes it.

## The cache policy, and why the early display cannot be a provider yet (2026-09-15)

THE POLICY IS A STATED FIELD NOW. `abi::Framebuffer` carries `memory_type`; the kernel sets
`FRAMEBUFFER_WRITE_BACK` where it describes the boot surface, which is what `PRESENT | WRITABLE |
NO_EXECUTE` with no memory-type bits actually IS on x86_64 and under this kernel's aarch64 MAIR, and
DisplayService states the same for a device scanout, which is a DMA buffer in ordinary RAM. A cache
policy that is not stated cannot be checked, and this one is a real choice: a linear aperture on a
discrete card wants WRITE-COMBINING, where a read costs an uncached round trip.

IT IS NOT A FIELD NOBODY READS. DisplayService COMPOSITES into the scanout, which reads it back, and
that is precisely the access pattern a write-combining aperture makes orders of magnitude slower than
it looks. A consumer that cannot ask has to assume the worse case or be wrong, and the fixture
asserts the value on every machine in the harness - so the day the mapping and the statement disagree
is the day a test fails rather than the day someone measures a mysterious slowdown.

AND THE SECOND PART OF THE ITEM HAS A PREREQUISITE NOBODY OWNS, which looking at it found rather than
assumed. Publishing the boot surface as an ordinary `ProviderKind::Display` needs a publisher that
can HAND DisplayService the surface, and there is nothing to hand:

- `SYS_FRAMEBUFFER_MAP` maps into the CALLER's address space and answers with a virtual address. It
  cannot be transferred, and a second caller is refused by design - `try_claim` is what stops two
  privileged callers both being given the display.
- `MemoryObject::create_in` and `DmaBuffer::create_in` both ALLOCATE fresh physical pages. Neither
  wraps an existing physical range.

So there is no kernel object that carries the firmware's aperture across a channel. THAT IS A
CAPABILITY DECISION AND NOT AN OVERSIGHT: a handle that maps arbitrary physical memory is exactly
what a capability system must not hand out casually, so such an object would have to be minted by the
kernel for one range it already owns and for nothing else - which is an object with its own rules
about who may ask for it and how often. Whoever takes that part owns it first; it is not a refactor
of what is already there, and the plan now says so rather than leaving the next implementer to
discover it after starting.

## And a second prerequisite, which correcting the first one found (2026-09-15)

THE PLAN I WROTE THIS MORNING SAID PART 2 WAS "machinery this tree already has and already tests".
That was an overclaim made from reading the shape of the thing rather than the code under it, and
looking found a second prerequisite beside the first.

EVERY PUBLICATION IN THE CATALOGUE IS KEYED TO A DEVICE CLAIM. `Catalogue::publish_all(binding,
entry, offers)` takes a `BindingId` - a bus, a device, a function and a claim generation - and there
is no other way in: a driver offers over the bootstrap channel of its own BINDING, and the manager
files the offer under that binding. A boot framebuffer is not a device, has no claim and therefore
has no binding, so there is no path by which a program holding it publishes a provider at all.

SO PART 2 NEEDS TWO NEW MECHANISMS AND NOT ONE:

1. A kernel object over an EXISTING physical range that can cross a channel - see the previous note.
2. Either a device-less publication path, or the boot surface as a SYNTHETIC device node.

Both are decisions about how this system's authority is shaped rather than refactors of what is
there. The second is the more interesting one: "a provider is something a bound driver published" is
load-bearing in the catalogue's design - it is what makes a withdrawal describable after the handle
is gone, and what ties a provider's lifetime to a claim - so a device-less publication is not a hole
to be filled but a second kind of thing the catalogue would have to carry.

THE PLAN NOW SAYS SO, AND IT ALSO SAYS THE THREE PARTS ARE NOT THE SAME SIZE. Two of them were a day
between them and are done; the third is two kernel-level mechanisms before a line of it can be
written. An item whose parts look alike in a list and differ by an order of magnitude in the work is
exactly what a written plan is for, and mine did not say it until it was checked.

## The two acceptance conditions that read as a gate on every driver item were stale (2026-09-16)

THE LEDGER AND THE HEAD OF THE SAME FILE COULD NOT BOTH BE TRUE. The head said "THE OTHER 21 ARE
IMPLEMENTATION WORK AND ARE NOT BLOCKED"; the acceptance conditions below it said P02M0172's
registry-policy mechanism "remains planned" and that "no driver item closes on a target until its
window has been measured and recorded" with P02M0162's measurement open. Read together, every
driver item was gated on two milestones. That is why the question "which of the twenty-one do I
start" had no good answer: on the ledger's reading, none of them could finish.

Both were checked against the tree rather than against their status lines:

- P02M0172 is COMPLETE since 2026-09-10, six days after the ledger's 2026-09-08 recheck.
  `abi::DMA_POLICY_NONE`, `DMA_POLICY_IOMMU_REQUIRED` and `DMA_POLICY_TRUSTED_UNTRANSLATED` are
  declared, `kernel/dma_policy/` maps each to a `Policy` and enforces it at admission, and the suite
  holds the mode/policy matrix, refusal by name, admission by declaration rather than rank, the
  manifest migration table and the degraded-isolation record. The `none` policy the condition was
  waiting for is a thing a driver declares.
- P02M0162 is COMPLETE since 2026-09-09 and its M5 is closed. The condition described its
  "300/400/4000 recovery constants"; M5 itself calls those obsolete and names 3,000 / 4,000 / 40,000
  ticks as the current `boot_userspace` arguments, and the tree agrees at `main.rs`,
  `arch/aarch64/boot.rs` and `arch/riscv64/boot.rs`. The measurement requirement survives the
  correction and is restated as what it is: a step each driver item performs on each target, not a
  wait on somebody else.

## And my own blocker table from 2026-09-15 was wrong in four ways

Correcting it is the point of writing it down. It said 43 open when the file holds 47 and has held 47
since before the table was written. It filed the ACPI WDAT watchdog and USB DFU under "an AML
interpreter": both are blocked on an unowned destination service, and WDAT's own text says it must
never execute AML while the specification table calls it a static table with no AML, so that row
named two items neither of which it described. It counted the destination-service cause at 9 when
thirteen items state it. And it omitted two causes entirely - an I2C controller with a real fixture
(IPMI SSIF, HID-over-I2C) and a display controller that can reach a bus (DDC/AUX) - so three blocked
items appeared in no row.

THE DEEPER MISTAKE WAS THE SHAPE RATHER THAN THE ARITHMETIC. Four rows summing to seventeen presented
the causes as a partition, and they are not one: six items are blocked more than once, UCSI and the
ACPI battery classes on three causes each. The corrected table answers "how many items does deciding
THIS reach", which is the question an owner asks, and says in as many words that the column does not
add up.

## The block wire had three hand-written copies and NVMe would have made a fourth

`OP_READ`, `OP_WRITE`, `OP_CAPACITY`, `OP_FLUSH` and the status codes were declared privately in
`virtio_blk.rs`, in `usb_storage.rs` and in `services/storage/src/service.rs`, and all three took the
sixteen request bytes apart index by index at the point of use. The roadmap's rule is that the wire
belongs to the first implemented slice that publishes a block provider; nobody owned it because
nobody extracted it. It is `driver_protocol::block` now, in the crate all three already share, with
fourteen host tests over the LITERAL bytes rather than over a round trip that would pass against a
wire nobody else speaks.

WHAT THE EXTRACTION FOUND, which is the argument for doing it before the fourth copy and not after:
the two servers disagree TODAY about what a refused request answers. `virtio-blk` replies
`STATUS_INVALID`, the constant that exists so a caller can tell a request it got wrong from a device
that failed one it got right; the USB path replies `STATUS_ERR` for the same refusal, so on that
server the distinction does not reach the client. Neither is a typo - each is a faithful
implementation of one end's own copy. It is RECORDED AND NOT REPAIRED in this change, because
changing a reply code on a path this change does not test is the kind of edit that looks free.

The compatibility rule was also a property of one client rather than of the protocol: StorageService
accepted a twelve-byte capacity reply so that a server predating the per-request bound still reports
a size, written as `len >= 12` inside the service. `CAPACITY_SIZE_LEN` and `decode_capacity_bytes`
name it, and two tests hold it.

## NVMe: the driver is up, and what it cost to find out why it was not (2026-09-16)

THE FIRST BRING-UP FAILED AND THE RESTART SUCCEEDED, and the log said "the driver says the device is
not responding" - which is DeviceManager's category, not a diagnosis. The driver answered `None` from
six different places and every one of them arrived at the manager as that one sentence.

So the first fix was to the driver's ability to be read rather than to the driver: `bring_up` now
returns a typed reason and prints it, and every admin command names its opcode and carries the
controller's own status type and code. The next run said "the I/O queue pair was not created" on
attempt 1 and "an admin command did not complete" on attempt 2, succeeding on attempt 3 - which is
already a different kind of problem from the one "not responding" suggested. A driver that cannot say
where it stopped makes its own log useless at the moment the log is the only thing there is.

A HYPOTHESIS WAS CHECKED AND WAS WRONG, and it is recorded because it was nearly written down as the
cause. `iommu/mod.rs` walks every PCI function during the bypass transition, finds the NVMe class
triple and clears `CC.EN` - which would explain a controller being disabled underneath its driver at
whatever command happened to be in flight. The logs carry no `quiesce` or `bypass` line at all on
this profile, so the transition does not run here and that is not what happened.

WHAT WAS GENUINELY WRONG, INDEPENDENT OF THE FLAKE: there was no barrier between the sixty-four bytes
of a submission entry and the doorbell write that says the entry is there. `write_volatile` stops the
COMPILER reordering those and says nothing about the machine. x86's store ordering usually hides it,
which is precisely why it was worth fixing: this tree also builds for aarch64 and riscv64, where a
controller reading a half-written submission entry is not a hypothetical. The completion side had the
mirror of it - the phase bit is what says the other fifteen bytes belong to this pass, and reading
them in an order the controller did not write them in is how a completion is believed while its
status is still the previous command's.

THE FLAKE IS NOT YET CLAIMED TO BE EXPLAINED. The fences are correct on their own terms; whether they
are what the intermittent bring-up needed is a separate question, and a run that happens to pass is
not an answer to it.

## The NVMe defect has a name now, and the first fix for it was wrong (2026-09-16, later)

THE DRIVER SAYS IT: "a completion for a command that is not outstanding", at the first bring-up and
at the stop-time flush. One defect wearing three faces - the retried bring-up, the flush that cannot
certify a clean stop, and the failure of the oracle test. It only has a name because the driver was
taught to report where it stopped and what the controller answered; before that it was "the driver
says the device is not responding", which is DeviceManager's category for six different places.

THE FIX THAT WAS TRIED AND REVERTED, recorded because the theory behind it was reasonable and is now
disproved. NVMe permits completions in any order, so an entry carrying an id this caller is not
waiting for looked like a stale completion to be CONSUMED and stepped over - the queue cannot advance
past an entry nobody consumes, and a driver that stops at the first one stops forever. Implemented,
it made the symptom worse: the driver reported "never completed" instead, and the boot came up with
five of twenty-four services. Draining the entry consumed the only completion that was going to
arrive. So the ids genuinely do not match, rather than a stale entry being in the way of the right
one, and the next attempt starts from that instead of from the theory.

The submitted id and the awaited id were read back out of the generated source rather than assumed:
they are the same `id` binding in all five admin call sites. So the mismatch is between what the
driver writes into dword 0 and what it reads out of dword 3, or between the slot it reads and the
slot the controller wrote - not a bookkeeping slip in the call sites.

THE ORACLE WAS WRITTEN, RUN AND REMOVED. It drives the driver as DeviceManager does, takes the block
provider, writes a non-constant pattern to LBA 7 - non-constant because the medium starts blank, so
a read returning untouched medium would match a zero buffer and one returning a constant fill would
match whatever offset it actually read - reads it back, compares, and checks that a range past the
last block is refused with the typed status rather than clamped. It fails because spawning a second
instance against a controller DeviceManager already owns destabilises the boot. A test that breaks
the machine it observes is not yet a test, so it came out rather than being left red, and
`component-oracle-exceptions.txt` carries a line saying exactly this rather than claiming the driver
has coverage it does not.

WHAT IS GREEN, AND WHAT THAT DOES AND DOES NOT MEAN. 401 tests pass with the controller on the bus
and the driver bound; the driver reports its namespace from IDENTIFY rather than from anything the
harness told it; three consecutive runs carried zero restarts after the memory fences went in. That
is a working driver with a known intermittent defect that DeviceManager's restart hides, and the
milestone says so in those words.

## The NVMe defect, found (2026-09-16, later still)

THE CONTROLLER WAS FETCHING SUBMISSION ENTRIES THIS DRIVER NEVER WROTE. The evidence is one number
and it took three rounds of making the driver able to say things to get it.

Round one: `bring_up` answered `None` from six places and the manager said "the device is not
responding" for all of them. Round two: a typed reason per place, and every command naming its opcode
and carrying the controller's own status - which turned "not responding" into "a completion for a
command that is not outstanding". Round three: the id it actually saw and the raw dword it came out
of, which is the one that settled it.

    driver.nvme: sqe slot 0 dw0 00010006 entries 8
    driver.nvme: command 06 got a completion for command id 0 while waiting for 1, dword 3 00010000

The submission entry was read back out of queue memory before the doorbell: command id 1, opcode 6.
The write had landed. The completion that came back carried command id 0 with a SUCCESS status, and a
ZEROED submission entry is exactly that command - opcode 0x00 is FLUSH and its command id is 0. So
the controller consumed a slot this driver never wrote, on the very first command of a fresh boot.

It does that when its tail doorbell still holds a value from before the reset: on enable it believes
there are entries queued up to that tail, consumes them, and they are the zeroed page. That also
explains why it was INTERMITTENT and why it moved around - the failure lands on whichever command
happens to be in flight when the phantom completions run out, which is why it was seen at IDENTIFY,
at CREATE I/O COMPLETION QUEUE and at the stop-time FLUSH.

THE FIX IS TWO STORES PER QUEUE AND CANNOT BE WRONG. A queue that has just been created is EMPTY, so
zero is what both ends should believe; the driver writes its own view - tail zero, head zero - onto
the admin pair after enable and onto the I/O pair after creation, rather than assuming the reset
cleared the controller's. A driver may not assume that.

WHAT THIS REPLACES: the earlier attempt to drain unexpected completions, which was reverted. That
theory said a stale entry was IN THE WAY of the right one; draining it made things worse because
there was no right one coming - the phantom completion was the controller's answer to a phantom
command. The disproof was worth more than the change.

## The NVMe defect, actually found: a torn completion entry (2026-09-16, final)

THE PHASE BIT ARRIVED BEFORE THE ENTRY IT VALIDATES. Everything else followed from that, and it took
three rounds of making the driver able to say things to see it.

    driver.nvme: sqe slot 0 dw0 00010006 entries 8
    driver.nvme: command 01 got id 0 waiting for 5, sq 0 head 0, dw3 00010000

The submission entry was read back out of queue memory before the doorbell: command id 1, opcode 6,
so the write landed where the controller reads. The completion did not come from the controller
answering anything: dword 2's bottom half is the submission queue head, and `head 0` with five
commands outstanding says it had consumed nothing. Every field of the entry was zero except the
phase bit in dword 3.

A SIXTEEN-BYTE WRITE FROM A DEVICE IS NOT ATOMIC TO ITS READER, and `read_volatile` says nothing
about that: it stops the compiler reordering, not the write from being half-visible. An acquire fence
BEFORE reading the entry orders the reader's own earlier accesses and orders nothing about a write
that is still in flight while it runs. So the driver read sixteen bytes, saw a phase bit that said
"this is yours", and believed a command id of zero.

THE FIX IS TWO RULES, and both are about not believing a half-arrived entry:
- The phase is read ON ITS OWN. Only once it says this entry belongs to this pass is an acquire
  fence taken and the entry read AGAIN. Every real completion is read twice and every empty slot
  once, which is what not believing an unfinished one costs.
- COMMAND ID ZERO IS NEVER ISSUED. It is a rule the driver keeps rather than a fact that happens to
  hold, and it makes "id 0" mean exactly one thing: an entry that has not finished arriving. Without
  it, a torn entry and a completion belonging to somebody else are indistinguishable, and those want
  opposite responses - keep waiting, or stop using the queue.

EIGHT CONSECUTIVE RUNS: no restart, no unexpected completion, no timeout, no failed flush. Three of
the ten runs before it carried a failure.

TWO EARLIER CANDIDATES, KEPT IN THE RECORD BECAUSE EACH DISPROVED SOMETHING. Draining an unexpected
completion and stepping over it was reverted - it made the symptom worse, which disproved "a stale
entry is in the way of the right one" and pointed at there being no right one coming. Zeroing each
queue's doorbells after creation was kept, because a queue that has just been created is empty and
the two stores cannot be wrong, but six runs with it carried three failures, so it is not what fixed
this and is not claimed to be.

THE WIDER LESSON FOR THE NEXT DRIVER IN THIS TREE: the same read is in `virtio.rs` and in `xhci.rs`
wherever a device-written descriptor is validated by one field and read as a whole. Nothing here
asserts they are wrong - the used-ring and event-ring paths were not examined - but the shape is
worth checking before the next one is written.

## The same defect, in a driver that ships (2026-09-16)

THREE PROGRAMS IN THIS TREE CONSUME A RING A DEVICE WRITES INTO and validate each entry by one
field: `virtio.rs` by the used index, `xhci.rs` by the TRB cycle bit, `nvme_driver.rs` by the
completion phase bit. Writing the third one and having it fail roughly one boot in three is what
made the shape visible in the other two.

`virtio.rs` is correct and has been: `take_used` reads `used.idx`, checks that it advanced by no more
than the buffers outstanding, FENCES, and only then reads the element. That is the pattern.

`xhci.rs` was not. `take_event` read the cycle bit out of the TRB's control dword, checked it against
the expected cycle, and then read `param` and `status` with nothing between. The cycle bit says the
TRB belongs to this pass; it does not say the other twelve bytes have arrived. Acted on torn, an
event carries somebody else's slot id, somebody else's completion code, or - for a port-status event
- a port number that is not the port that changed.

ONE FENCE, AND NO CLAIM THAT IT WAS EVER SEEN TO FAIL. It is the same defect in the same shape in a
driver that ships, fixed because it is wrong. 402 tests pass with it in.

WHAT THIS SAYS ABOUT THE OTHER TWO THINGS THE TREE DOES WITH DEVICE-WRITTEN MEMORY: nothing yet. The
virtio-gpu response reads and the virtio-snd period handling were not examined, and this note does
not cover them.

### And the sweep that closes it

Every ring consumer in the tree was then checked rather than assumed, which is the half that turns
one fix into a statement about the tree:

- `virtio.rs::take_used` - CORRECT. Reads `used.idx`, checks it advanced by no more than the buffers
  outstanding, fences, then reads the element.
- `virtio.rs::submit` (the busy-polling path `virtio-gpu` uses, which reaches the used ring without
  `take_used`) - CORRECT. After the poll loop observes the index bump there is an explicit fence,
  with a comment naming the exact reordering it prevents on a weakly ordered core, before the element
  is read.
- every other virtio driver - covered, because `virtio_blk`, `virtio_net`, `virtio_input`,
  `virtio_snd`, `virtio_console` and `serial_port` all reach the ring through one of those two, and
  their direct `read_volatile` calls read the DMA payload AFTER the fence those paths take.
- `xhci.rs::take_event` - WAS WRONG, now fixed.
- `nvme_driver.rs` - was wrong, now fixed, and is where this started.

So the defect class is closed across this tree's ring consumers rather than in the one driver where
it was caught.

## AHCI: the second consumer, which is what tells a generalisation from a rename (2026-09-16)

THE POINT OF TAKING AHCI NEXT WAS NOT THE DRIVER. It was that `RESOURCED` - the plain-PCI resource
profile the NVMe item owns - had exactly one consumer besides the xHCI row it was refactored out of,
and a table with one row is a special case wearing a table's clothes.

IT COST ONE ROW AND ONE FIELD. AHCI's register file is in BAR 5 rather than BAR 0, which xHCI and
NVMe both use, so "resolve BAR 0" and "resolve the register file" had been the same sentence and were
not. The row carries the index; `resolve_endpoint` gained nothing. Before the generalisation the same
change would have been a resolver, three architecture shims and a loop in `device.rs`.

THE MACHINE THEN ANSWERED A QUESTION NOBODY ASKED IT. The q35 chipset carries its own SATA controller
at 00:1f.2 with a CD-ROM on port 2, so every boot binds this driver twice without the harness
arranging it, and the ATAPI refusal path is exercised for free on every run:

    driver.ahci: port 2 carries an ATAPI device, which this driver does not serve
    driver.ahci: bring-up gave up at 00:1f.2 - no implemented port has a SATA disk on an active link

AND THAT FOUND A DEFECT IN THE REPORTING. The failure was answered as retryable, so DeviceManager
restarted a driver that had already given its final answer, and the boot log carried the same refusal
twice for no reason. "Whether a second attempt could differ" is a different question from "what went
wrong", and the manager acts on the first: a controller whose only device is ATAPI is
`UnsupportedDevice`, while a resource shortage, a port that did not settle and a command that did not
come back are states a second attempt can find differently. Worth noting that the NVMe driver has the
same shape and has not been re-examined for it - its give-up points are all resource or timing ones,
but that is a claim about the six arms rather than a check of them.

ONE MISTAKE IN THE ORACLE, WORTH RECORDING BECAUSE IT IS A HARNESS TRAP RATHER THAN A DRIVER ONE. The
test walks both controllers and keeps the one that reports READY, and it first held each bootstrap
channel in a loop-local. Dropping it closes the driver's bootstrap end, the driver reads that as the
manager going away and stops, and the block provider it had just published answered the first request
with `PeerClosed`. The harness stands in for DeviceManager, and DeviceManager does not hang up on a
driver it is still using.

## HDA: not working, and what it cost to find that out precisely (2026-09-16)

THE DRIVER IS STAGED AND FAILS CLEANLY. It resets the controller, resets the command ring's read
pointer, reads the codec bitmap and gets its first verb answered - and then the next one is not
answered. It reports `UnsupportedDevice`, the node fails once, the boot is unaffected and 404 tests
pass with it in.

THREE THINGS WERE WRONG AND ARE FIXED, and only one of them was the reason it fails:

- THE COMMAND RING'S POINTER RESET IS A FOUR-STEP HANDSHAKE. The bit is written as one, and the
  specification then requires software to READ IT BACK AS ONE - that read is the confirmation - and
  only then write zero and read zero. Writing one and waiting for it to clear waits for something the
  controller is specified never to do: the register sat at 0x8000 for ever. Fixed, and the failure
  moved past it.
- THE RING SIZE. Sixteen entries is an optional size; only 256 is required of every controller. A
  controller that declines sixteen keeps what it had, and the driver then wraps at sixteen while the
  controller wraps at 256 - which does not disagree until the rings have been used a little, so the
  first response still arrives. Changed to 256. It changed nothing, so it was not the cause, and it
  was wrong anyway.
- A DIAGNOSTIC THAT LIED. The CORB pointer wait borrowed the reset failure's message and printed
  GCTL's value beside it, which read 1 - a sentence that contradicted itself and sent the reader to
  the wrong register. Every wait has its own arm now.

AND ONE DEFECT THAT IS NOT ABOUT AUDIO, which is the finding worth carrying to every driver here.
The waits were bounded in SPIN COUNTS. A spin is not a time: one iteration is an MMIO read, tens of
nanoseconds on silicon and a trap into the hypervisor under emulation, three orders of magnitude
apart. Ten million of them outlasted the two-second bind window, so the manager killed this driver
for silence before it could say what it was waiting for - and outlasted the BOOT window too: nine of
twenty-four services came up, which looked like a system-wide failure caused by adding one audio
driver. The machine already has the unit. The timer is 100 Hz on every architecture, READY is due 200
ticks after BIND, and the bind windows measured today put every driver on this machine inside 44. So
bring-up takes one deadline in ticks and every wait shares it, and a bring-up in which everything
times out still finishes in time to report.

WHAT IS LEFT is the CORB/RIRB handshake after the first response. Its intermittence is the clue: one
boot gets past the vendor verb to the node count and another does not, which is the same shape as the
torn completion entry the NVMe driver turned out to have, and that one took three rounds of better
diagnostics to see rather than one round of better reasoning.

---

## 2026-09-19 - five drivers stopped serving when a consumer left

FOUND BY READING, not by a failing test, while planning the vsock connection table. `virtio_vsock`'s
serve loop had `let Received::Message { .. } = recv_blocking(endpoint, ..) else { continue };` where
every older driver in the tree has `close_at` + `disconnected`. A scan of `src/user/` found the same
`else { continue }` in exactly five files - `virtio_scsi`, `ahci_driver`, `virtio_vsock`,
`nvme_driver`, `sdhci_driver` - and nowhere else. They are the five newest drivers.

THE MECHANISM, CONFIRMED FROM BOTH ENDS RATHER THAN ASSUMED:
- `rt::poll_ready(handle)` is `wait(handle, clock().max(1)) == 0`, and a kernel channel's readiness
  is `!self.inbox.lock().is_empty() || self.is_peer_closed()` - the peer is held as a `Weak`, so it
  reads closed once the last strong reference goes. A CLOSED ENDPOINT IS A READY ONE, deliberately:
  it is how a reader learns of the closure.
- `serve_any_or_answer_inner` returns the FIRST ready index. So the departed consumer is answered
  ahead of every live one, on every pass. The driver does not merely spin - it never reaches the
  consumers that stayed.
- The control channel is drained FIRST on every pass, so the heartbeat is answered throughout. A
  watchdog cannot tell this driver from a working one.
- `DeviceManager`'s `BindingEvent::Disconnected` is what refunds a place against the registry's
  `consumers` bound, and its own comment says the count "only rose" without it.

THE FIX IS ONE COPY: `common::recv_from_consumer(bootstrap, bind, serving, at, buf)` answers the
request or drops and reports the consumer, and all five call it. Five independent omissions is the
argument against leaving the rule written only inside the drivers that got it right.

THE ORACLE IS IN `kernel.hardware.virtio_vsock_driver_echoes_bytes_off_the_host`: mint a second
consumer with `send_connect` the way DeviceManager does, drop BOTH references to the first (the
handshake keeps one beside the downcast, and the peer is weak), then require the second to be
answered and a `Disconnect` frame naming the publication's token to have arrived.

MUTATION: replacing the helper's body with a bare `None` makes the suite TIME OUT rather than fail an
assertion - the driver never goes idle, so `sched::run_until_idle()` does not return. Recorded in the
test beside the assertion, because a timeout is what somebody later calls flaky.

Guest evidence: `./test.sh --tags drivers,pci,slow --arch x86_64` - 103 passed, with all five drivers
online in the same boot; the same run times out at 300 s with the mutation in.

AND A GATE, because five independent omissions is the evidence. `source-hygiene` now refuses a file
under `src/user/drivers` that calls `serve_any_or_answer` or `wait_providers_or_answer` and never
calls `close_at` or `recv_from_consumer`. PRESENCE AND NOT SHAPE, deliberately: the six drivers that
got it right do not agree on the shape, and a rule written against one of them would flag the others.

THE FIRST VERSION OF THE RULE PASSED ON A COMMENT. Each fixed driver names `recv_from_consumer` in
the comment above the call, so a match that looked anywhere in the file approved a driver whose CALL
had been removed - which is the "passes by not being understood" failure this script warns about
elsewhere. It uses the same leading-content class as the literal-address rule now, and was proven to
refuse before being trusted to approve: with the nvme call renamed the gate exits 1 and names the
file; restored, it is clean.

Guest evidence after the gate: `./test.sh --tags drivers,pci,slow,storage,filesystem,usb` - 152
passed; `./check.sh --gate source-hygiene` clean.

---

## 2026-09-19 - virtio-vsock: a connection table and the event queue

THE TWO THINGS THE ITEM NAMED AS OPEN. `virtio_vsock` served one connection and configured its event
queue without ever posting to it.

**THE TABLE.** `MOST_STREAMS = 4`, each a `Stream { owner, connection, staging }`, and the manifest
declares four `local-stream` publications of one consumer each. `consumers` stays ONE deliberately -
the entry's own reason, that two consumers sharing a stream interleave their bytes - and what four
buys is four independent streams.

TWO LOOKUPS THAT ARE NOT THE SAME LOOKUP, which is where this would have gone wrong:
- BY PORT, for an arriving packet. `vsock::slot_of(dst_port, base, slots)` - a stream's local port is
  `base + slot` by construction. `checked_sub` then a range test, so a port below the base does not
  wrap into the table and one above it is not masked in. Host-tested at both ends of the range, plus
  a port a remainder WOULD have admitted (1032 against four slots).
- BY ENDPOINT VALUE, for a consumer's request. NOT by the `Serving` index: `close_at` fills the hole
  with the last entry, so an index names a different consumer as soon as any other one leaves.

RECONNECT: refused while this consumer's own stream is OPEN (the wire cannot say which of two a
later SEND is for), allowed once it has CLOSED, and the slot is reset so no bytes from the previous
connection are readable through the new one.

A CONSUMER'S DEPARTURE RESETS ITS STREAM AND FREES THE SLOT, or "connection limits" would mean four
connections per boot rather than four at a time.

AN OVERRUN IS PER STREAM. `drain` answers a BITMASK, because one `OP_RECEIVE` routinely drains
packets belonging to other streams - the stream that overran is reset there, and only an overrun on
the caller's own stream becomes its answer.

**THE EVENT QUEUE.** Four slots of one `u32`. A transport reset is a statement about the transport
that no connection's state machine can make for itself, so it is read before a consumer's request is
answered - the only moment its answer differs. An unrecognised event is re-posted, not acted on. A
device with no event queue, or no buffer for one, serves its streams as before and says so.

**THE ORACLE.** Both streams are written before either is read; send-read-send-read would pass a
one-buffer driver. Two different patterns, each read back on its own stream. The host's log carries
connections from ports 1024 and 1025, which is the table handing out distinct ports.

MUTATION: `slot_of(..).map(|_| 0)` - every packet to slot zero - fails at the second consumer's
connect, `left: 1 right: 0`, because that stream's response landed on the first and it timed out in
`Connecting`.

A BUILD NOTE WORTH KEEPING: the kernel's in-guest tests are not `cfg(test)`. Helpers added beside
them need `#[allow(dead_code)]` and FULLY QUALIFIED paths - `driver_protocol::stream::`,
`object::channel::Message` - because the imports the test bodies use are inside those bodies. The
build error is five `cannot find` lines and the run log shows only "5 previous errors"; the detail
comes from `TEST=1 TEST_TAGS=... cargo build --target x86_64-unknown-none --tests` in `src/kernel`.

---

## 2026-09-19 - virtio-scsi reaches a second target, and what was actually in the way

THE DRIVER WAS NOT THE PROBLEM. `find_units` has walked `0..=max_target` since it was written; it
stopped early because `MAX_UNITS` was two and the first target on this machine carries two logical
units, so the table was full before the walk asked target 1.

AND `MAX_UNITS` WAS TWO BECAUSE `MAX_PROVIDER_CLIENTS` WAS EIGHT. Four units at four consumers is
sixteen connections; the manifest check refuses `most * consumers` past the serving set. Eight was
sized when a driver published ONE provider - four consumers for a disk, room for a second - and a
SCSI HBA publishes one per unit across several targets, which is a consumer that number never had.

RAISED TO SIXTEEN in both copies (`drivers::common` and `system-manifest`, which a check holds in
step). Cost: four arrays of that length in `Serving`, thirteen bytes an entry, ~100 bytes per serving
driver. `MAX_INITIAL_OFFERS` is eight and four publications fit it. The shape is unchanged - fixed
set, a `CONNECT` past it still refused by closing the endpoint.

THE FIXTURE IS THE HALF THAT MAKES IT OBSERVABLE. Two units behind ONE target cannot distinguish a
driver that walks targets from one that stops at the first - both find everything there is. The
harness now attaches a third medium at `scsi-id=1,lun=0`, 1 MB against the existing 4 MB and 2 MB,
because capacity is how the oracle tells which medium it was handed. The oracle asserts three units,
the third unit's own capacity, and a write-then-read on it with a pattern that differs from the first
target's.

MUTATION: `let last = ...min(0)` - the walk stops at target 0 - fails with `left: 2 right: 3`.

ALSO FOUND, BY THE THIRD UNIT NOT FITTING: `common::print_line` copies every driver's online report
into a 96-byte buffer while drivers build those reports in `Bounded<160>` and `Bounded<192>`. The
guest printed `... t0l1 4096 x 512 bytes, t1l` and stopped - an operator told about two and a half
disks, with nothing saying the line was cut. The buffer is 256 now. Truncation itself stays: a driver
that panicked writing a report is a device that never came up.

Guest evidence: `./test.sh --tags drivers,pci,slow,storage` - 152 passed, with
`driver.virtio-scsi: online (00:0e.0, 3 units: t0l0 8192 x 512 bytes, t0l1 4096 x 512 bytes, t1l0 2048 x 512 bytes)`.

---

## 2026-09-19 - AHCI: NCQ, and the concurrency without which it is ceremony

NCQ ALONE WOULD HAVE BEEN A MECHANISM NOTHING EXECUTES, which this milestone refuses in as many
words beside NCM and CDC-ACM. A queued command that is the only one in flight costs what an unqueued
one costs. So the question was where a second command comes from, and the answer was already in the
registry: the block contract is one request and one reply per message, so a single consumer never
has two outstanding - but every disk here declares FOUR CONSUMERS, and nothing used that.

THE SHAPE IS A BATCH, chosen over an asynchronous loop deliberately. Holding commands outstanding
across `serve_any_or_answer` means the driver blocks indefinitely with the disk working, so it would
have needed either an interrupt (AHCI's is not granted to this driver) or a poll that runs while
idle - the thing this tree warns about beside two other drivers. A batch never leaves a tag
outstanding across a blocking wait: block for the first request, ask every other consumer whether one
is ALREADY waiting (`poll_ready` is a wait against the current instant), issue each on its own tag,
then complete and answer each as its own command finishes.

PER-TAG RESOURCES, not one shared span: two commands in flight into one buffer is two transfers into
the same memory and the second to finish wins. Four tags (`min(CAP.NCS, 4)`), each with a 256-byte
command table in the second structures page and its own growable data span.

THE THREE DECISIONS ARE IN `drivers::ahci` WITH FIXTURES, because each is a way a first NCQ driver is
wrong:
- `queued_fis`: the COUNT goes to features/features_exp and the sector-count byte carries the TAG
  shifted left by three. Filled the ordinary way it asks for tag 0 of a zero-length transfer, which
  most controllers read as the maximum.
- `queued_outcome`: outstanding is `PxSACT`, not `PxCI`. `PxCI` clears when the command was SENT.
- The error is read BEFORE the active bit, because a failed queued command halts the port and
  abandons every other tag - they are all answered failed, and the port is stopped, cleared, started.

`Capabilities` gained `queued` from CAP.SNCQ (bit 30, ADJACENT to S64A at 31 - a fixture pins that
the two are not confused).

THE ORACLE: mint a second consumer with `send_connect`, write two sectors with different patterns,
then put a read from EACH on the wire before taking either reply, and require each to get its own.

MUTATIONS AND THEIR SIGNATURES:
- every request on tag zero -> `both consumers should be answered: Empty`.
- a queued completion read out of `PxCI` -> the suite HANGS rather than failing an assertion. Worth
  knowing before somebody calls it flaky.

Live evidence: `driver.ahci: online (00:0b.0, 16384 x 512 bytes, ncq 4 tags)` - QEMU advertises
CAP.SNCQ, so the pre-existing write-then-read oracle now runs over `READ/WRITE FPDMA QUEUED` and is
what says the field mapping holds on a controller rather than only against fixtures.

Guest evidence: `./test.sh --tags drivers,pci,slow,storage` - 152 passed. Host: drivers crate 231.

---

## 2026-09-19 - SDHCI ADMA2, and the 32-bit address nobody can ask for

ADMA2 lifts the one-block-per-request bound the PIO path publishes: 256 blocks in one request
instead of one, with the bound published in the capacity reply and a request past it refused rather
than shortened.

THE REGISTRY REFUSED IT FIRST, CORRECTLY. The entry declared `dma = "none"` for the PIO slice, and a
claim under that policy mints no DMA buffer - so the first build reported "no memory for the
descriptor table" rather than quietly mastering the bus. Changed to `trusted-untranslated`, at the
cost the threat model already records.

CORE DECISIONS WITH FIXTURES: the length field is 16 bits and ZERO MEANS 65536 (so the bound is
65024, a whole number of blocks below the range, and every length is literal); Auto CMD12 belongs to
a multi-block transfer and must NOT be set for a single one; the last descriptor is marked.

**THE INTERMITTENT FAILURE IS THE FINDING.** The suite passed on `drivers,pci,slow` and failed on
`drivers,pci,slow,storage`, at the SINGLE-block write - which the descriptor arithmetic cannot
reach, so the first instinct (my mutation broke it) was wrong. Found by making the driver say what it
could not say, in three rounds:
1. a fault line on the ADMA completion - nothing printed, so the failure was earlier;
2. the same on every `command()` give-up point - still nothing, so it was earlier still;
3. the remaining silent `STATUS_ERR` paths - and then
   `command 24 reported an error - int 02008001, adma-err 01`.

Interrupt bit 25 is ADMA error; ADMA error state `01` is FETCH DESCRIPTOR. An ADMA2 descriptor
carries a 32-BIT address, QEMU's controller claims no 64-bit bus, and the allocator had placed the
descriptor table above 4 GiB - so the truncated low half named something else. Which span landed
where depended on what had allocated first in that boot, which is what made it look like flakiness.

Both spans are checked now; a driver that cannot address its own descriptors keeps PIO and says so -
the same refusal `ahci::Capabilities::usable` makes before binding.

**THE GAP, NAMED FOR AN OWNER:** nothing here can ask for memory a 32-bit device can address.
`SYS_DMA_BUFFER_CREATE(size, device)` and `frame::allocate_contiguous(pages)` carry no address
limit. That is a memory-ABI change rather than a driver's, so it is recorded and not invented inside
this item; until then ADMA2 is taken when the allocator obliges and PIO serves when it does not.

**AND THE FIRST VERSION OF THE ORACLE PASSED A BROKEN DRIVER.** Four blocks fit ONE descriptor, so
"every descriptor names the same address" was a no-op. Raised to 128 blocks (two descriptors) - and
it STILL passed, because the driver's bounce span still held the write's bytes, so a read that never
fetched found the right answer sitting there. A second span written in between fixed it: the
mutation now fails on the array.

Guest evidence: `drivers,pci,slow` 103 passed with `adma2 256 blocks`; `drivers,pci,slow,storage`
152 passed with the 32-bit refusal firing and the PIO path serving. Host: drivers crate 234.

---

## 2026-09-19 - HDA capture, and a provider kind whose two servers spoke different protocols

THE ITEM ASKS FOR CAPTURE "THROUGH THE SAME PCM CONTRACT AS virtio-snd", and the contract was three
constants and a comment inside `virtio_snd.rs`. `hda` - the second server of `ProviderKind::Audio` -
had never read it. Two consequences, neither visible from either driver on its own:

1. A ONE-BYTE MESSAGE WAS A COMMAND TO ONE SERVER AND A ONE-BYTE PERIOD TO THE OTHER. A consumer
   asking an HDA machine for a microphone got a click.
2. `hda` NEVER REPLIED TO A PERIOD. `audio_engine` sets `driver_pending = Period` on send and only
   sends the next one from `driver_ready`, so an HDA machine played ONE PERIOD PER BOOT and then went
   quiet - and a driver waiting for a message nobody will send looks exactly like an idle one.

EXTRACTED TO `driver_protocol::audio`, beside `block` and `stream` and for the reason that section
gives, except this one was not copied but ignored. The shapes are an ENUMERATION (`message()`):
`PERIOD_BYTES` is Play, empty is EndPlayback, one byte is Capture/EndCapture, everything else is
Unknown and refused. Both servers and `audio_engine` read it; `audio_engine` keeps a `const _: () =
assert!` tying its frame count to the wire's period.

CAPTURE IS THE OUTPUT ROUTE REVERSED: `Widget::AudioInput`, `pin_can_input` (capability bit 5, beside
output's 4), stream tag 2, pin control 0x20 (IN) where playback writes 0x40 (OUT), input amp payload
0x7000 where output writes 0xB000. Input stream descriptors are the ones BEFORE the output ones, so
input stream zero is descriptor zero. The stream runs between requests and periods come from
alternate halves on `SD_STS` bit 2, sticky and write-one-to-clear.

A codec with no input refuses with the wire's empty reply - which cannot be mistaken for samples,
because a period is never empty.

FIXTURE: `hda-output` -> `hda-duplex`. ORACLE: a period is ANSWERED (the reply that never existed),
a capture returns a whole period rather than the refusal, and the INPUT stream's link position
advances - the device saying it filled the buffer rather than the driver handing back its own zeros.

MUTATIONS: dropping the capture converter fails with `left: 0 right: 2048`. AND ONE THAT DOES NOT
FAIL, recorded in the test: setting the input pin to DRIVE instead of LISTEN passes everything,
because QEMU's codec model runs the stream either way. On real silicon that is a microphone that
records nothing - the fixture's limit, not the assertion's.

Host: driver-protocol 66, drivers 235. Guest: `drivers,pci,slow,storage` 152 passed.

### An unreproduced UB-check panic, recorded so it is not lost (2026-09-19)

One run of `--tags drivers,pci,slow,storage,usb,network,service` failed inside
`kernel.applications.a_command_word_on_its_own_runs_the_command` with

    panicked at core/src/slice/iter.rs:100:78:
    unsafe precondition(s) violated: ptr::add requires that the address calculation does not overflow

The immediately preceding run of the same selection, and the immediately following one, both passed
201 tests - so it is intermittent and it is NOT a regression from the audio work in this session (the
same test ran and progressed further in a run predating those changes).

IT IS WRITTEN DOWN RATHER THAN DISMISSED because a UB precondition check is not a flake: something
built a slice iterator over a pointer whose arithmetic overflows, and it did so once in three runs.
The panic is inside the core library, so the frame that built the slice is not in the message and
there is no backtrace to chase it with from here. What a next attempt needs first is a way to get one
- the check fires in the kernel's own test process, so the fault path already has the registers.

---

## 2026-09-19 - CDC-ECM: the notification endpoint, and what "one adapter by budget" really was

THE ENDPOINT WAS MEASURED BEFORE IT WAS WRITTEN FOR. The parser gained the control interface's
interrupt IN endpoint and a log line saying whether the adapter publishes one; the live device
answers `0x81`. Only then was anything built to consume it - and QEMU turned out to SEND a
`NETWORK_CONNECTION` notification, which the guess "QEMU's ECM model has no source of notifications"
would have got wrong.

CLAIMED AS A THIRD PIPE: endpoint type 7 (interrupt IN) where 6 is bulk IN and 2 is bulk OUT, its own
ring and page, transfer standing beside the receive one. `usb_class::NETWORK_COST` charges three
endpoints, three rings and three pages whether the device has one or not - a budget that charged for
what a device happened to publish would admit an adapter it could not afford.

DECISIONS IN `drivers::cdc` WITH FIXTURES: any non-zero wValue is connected (not `== 1`); an
unrecognised notification is `Other(code)` and not a link change; a speed change whose header claims
eight bytes of rates in a transfer that carried none is `Malformed`, because reading them reads past
what the controller wrote. Repeat suppression lives in the driver: a second "connected" is not a
transition.

LIVE: `driver.xhci: the CDC adapter reports its link is up` on every binding boot; 201 passed across
`drivers,pci,slow,storage,usb,network,service`.

TWO MUTATIONS THAT DO NOT FAIL, recorded because silence about them is worse than the gap:
configuring the notification endpoint as BULK still binds and still delivers, because QEMU's xHCI
model does not enforce the type; and nothing ever reports the link going DOWN, so the transition is
exercised one way only.

**AND THE "ONE ADAPTER, BY BUDGET" NOTE WAS WRONG ABOUT THE CAUSE.** The budget is the symptom.
`network_service.rs` opens the FIRST `net` provider and closes the subscription - "this service takes
one NIC at bootstrap and does not follow a replacement - its whole stack is built on the link it
opened". A second adapter would publish a provider nobody opens. Two links is a NetworkService item.

---

## 2026-09-19 - UAS: task management and tagged concurrency are one piece, and the measurement says why

The item's note had these as two open points. They are one.

WRITTEN AND WIRED FIRST, THEN MEASURED: `uas::task_management_iu` and `uas::task_done` with
fixtures, and an `abort_task` on the command timeout path. The live device answered NOTHING -
`the UAS device did not answer a task-management request` - which a probe at bring-up confirmed was
not about the timeout path being unreachable.

THE REASON IS `const TAG: u16 = STREAM_ID as u16` AND A DOORBELL THAT ALWAYS RINGS `STREAM_ID`. The
status transfer is posted on one stream; a task-management request must carry a tag distinct from
every outstanding task's, and the device answers on the stream matching THAT tag - where nothing is
posted. A second tag is a second stream, and a second stream is tagged concurrency.

SO THE DRIVER WIRING WAS TAKEN BACK OUT rather than left as a path that cannot answer, and the
timeout path now SAYS what it actually does: clearing an endpoint halt tells the controller to forget
a transfer, and the device is still executing the command.

THE DECISIONS STAY, with fixtures, because they are what the next attempt needs first and they are
held rather than assumed: the request's own tag in the header and the managed tag in its own field
(both big-endian), and `task_done` being TWO non-adjacent codes - 0x00 "complete" and 0x08
"succeeded" - so a successful abort is not read as a failure and escalated into a unit reset.

Host: drivers 241. Guest: `drivers,pci,slow,usb` 103 passed.

---

## 2026-09-19 - USB Audio Class: an isochronous transport, and a class on top of it

THE ITEM'S REAL SIZE WAS THE TRANSPORT. This xHCI driver had no isochronous transfer type at all -
measured before the plan was written, not assumed: its TRB constants were Normal, Setup, Data,
Status, Link and the command types, and all four existing classes ride bulk, interrupt or control.

WHAT THE TRANSPORT NEEDED: the Isoch TRB (type 5) and isochronous endpoint types (1 OUT, 5 IN);
`Start Isoch ASAP` rather than a frame id; error count ZERO in the endpoint context, because an
isochronous endpoint that retried delivers last interval's audio in this one; the interval as an
EXPONENT OF MICROFRAMES where the descriptor states FRAMES (one frame is eight, so the descriptor's
number gives a schedule eight times too fast); and one transfer descriptor per service interval -
the wire's 2048-byte period is eleven transfers against this device's 192-byte packet, the last one
short.

`drivers::uac` HOLDS THE DECISIONS WITH FIXTURES: a 24-bit little-endian sample rate (a four-byte
read swallows the next rate's low byte); `bSamFreqType == 0` is a RANGE, not an empty list; the
format must match exactly INCLUDING the subframe size (16 bits in a 4-byte subframe is the same
samples in twice the bytes); and only bits 1:0 of the endpoint attributes are the transfer type -
QEMU's device sets the synchronisation bits, so a whole-byte comparison fails on the real fixture.

FORMAT IS A REFUSAL, NOT A CONVERSION. 48 kHz stereo 16-bit or nothing.

INTEGRATION: `ClassKind::Audio` with its own budget (one isochronous endpoint, two in flight -
a sink with one goes silent between the transfer completing and the next being posted); the
publication is offered only when a sink is bound, because this wire's refusal is an empty reply and
a service could not tell "no device" from "the device said no"; `usb-audio` on the hub at port 1.4,
because the root ports were full.

**AND ONE BUG I INTRODUCED AND THE SUITE CAUGHT IMMEDIATELY**, worth recording because of its shape:
adding the audio release to the detach path broke the BRACE NESTING, so the slot teardown and the
"port detached" print moved inside `if audio.is_some()` and ran for EVERY port. The guest printed
five detaches right after coming online and the CDC round trip failed. A structural edit that
compiles is not a structural edit that is right.

ORACLE: a period is answered `OK` only after the transfer event for its last packet, so a sink that
answered without moving anything could not answer. MUTATION: a Normal TRB in place of the Isoch one
fails it, empty against `OK`.

BIND WINDOW, READ WITH CARE: 97, 164 and 181 ticks across three runs of the SAME build against an
allowance of 200. The number is dominated by host load, not by the seventh device.

Host: drivers 247, driver-protocol 66. Guest: 201 passed across
`drivers,pci,slow,storage,usb,network,service,filesystem`.

---

## 2026-09-19 - the ABI snapshots were stale, and the gate that catches it had not been run

`./check.sh --gate host-tests` failed on two crates. `system-manifest`'s two were mine - the
provider budget moved from 8 to 16 and SDHCI's DMA policy from `none` to `trusted-untranslated`, and
both were spelled out as literals in assertions. `abi`'s two were NOT mine and were in the committed
tree: `SYS_DEVICE_EVENTS = 86` added with `SYSCALLS` still ending at 85, and `DeviceInfo::on_bus`
inserted so `_pad2` moved 52 -> 53 against a layout snapshot asserting 52. Both are the PCIe hot-plug
work of 2026-09-18.

THE GUARDS WORKED AND THE RUN WAS MISSING. The syscall snapshot's own comment records the previous
occurrence (`SYS_DEVICE_QUIESCED`) and the completeness check added because of it - and that check is
exactly what reported this one. A build and a guest suite both pass over a stale snapshot, because
neither compiles that crate's tests; only `host-tests` does.

AND THE MANIFEST TEST WAS REWRITTEN RATHER THAN RENUMBERED. Its subject is two constants in two
crates agreeing, and it had `8` written into four assertions - so the day the bound moved it failed
on its own arithmetic rather than on a disagreement. It derives every case from
`MAX_PROVIDER_CLIENTS` now, including a new one for the PRODUCT (`most * consumers`) that the literal
version could not express once the numbers changed.

107 host suites pass.

### A full-suite-only console failure, reproduced and not attributed (2026-09-19)

`./test.sh --arch x86_64` with NO tags - the full 449-test suite - fails deterministically in
`kernel.services.a_tty_a_job_left_raw_comes_back_cooked`: the program receives `xab\n` where the
test typed `ab\n`, and the console's echo in the log is `xab`. A stray `x` is in the kernel console
input before the test types anything.

WHAT WAS ESTABLISHED:
- It is DETERMINISTIC in the full suite (three runs).
- It PASSES in every scoped selection tried, including `service,console,shell` (99 tests),
  `service,console,shell,imgview` (99), `service,console,shell,input,mouse,text,display,image` (100)
  and `service,console,shell,drivers,pci,slow,storage,usb,network` (201) - the last of which carries
  every driver, device and harness change from this session.
- NOTHING IN THE TEST SUITES FEEDS AN `x` TO THE CONSOLE. `console_input::feed_serial` has exactly
  one caller in the suites, inside this test's own helper; the kernel's other callers feed `\n`.
- The serial receive path is not the source: `read_byte` checks `data_ready()` before reading.
- The code it fails in - `console_service`, `term`, `test_suites/services.rs` - is untouched by this
  session's work.

SO IT IS RECORDED RATHER THAN GUESSED AT. What a next attempt needs is the suite that carries the
contaminating test, and the bisection is by TAG - noting that the tag filter requires EVERY tag a
test declares to be selected, which is why adding `drivers,pci` alone does not run a
`[Drivers, Pci, Slow]` test.

AND ONE SMALL THING FOUND WHILE BISECTING: `./test.sh --list-tags` prints `permission`, and the
kernel's own filter answers `test filter error: unknown tag 'permission'` for it. The list and the
filter disagree about at least that one name.

## The tag filter reads the other way round, and the list is truncated (2026-09-19)

CORRECTION TO THE PARAGRAPH ABOVE. It says "the tag filter requires EVERY tag a test declares to be
selected". It does not. `src/kernel/tests.rs` selects a test when ANY of its tags was requested, or
when the test is a smoke test. What is all-shaped is a VETO over exactly two tags: a test carrying
`slow` or `stress` is excluded unless that tag is requested by name. So the observation that
`drivers,pci` alone does not run a `[Drivers, Pci, Slow]` test is right and the reason given for it
was wrong - the missing tag is `slow`, and adding it is enough. Bisection by tag is available.

AND THE `permission` MISMATCH HAS A CAUSE. `./test.sh --list-tags` greps the tag table out of the
kernel source with a character class that stops at a hyphen, so every hyphenated tag is printed
truncated. It advertises four names that are not tags - `arch`, `capability`, `permission`, `volume`
- and hides eleven that are: `arch-aarch64`, `arch-riscv64`, `arch-x86_64`, `audio-service`,
`capability-tcb`, `dynamic-reject`, `lico-load`, `permission-service`, `process-service`,
`volume-layout`, `volume-scope`. Four of the eleven truncate onto a different REAL tag - `audio`,
`dynamic`, `lico`, `process` - so the output looks complete. `volume-layout` is the selector for the
booted-system test below, and it is one of the eleven.

## The booted-system test's log says the volume is empty, and the volume is not (2026-09-19)

`kernel.boot.init_package_starts_system_manager` (`[Boot, Service, VolumeLayout]`) fails
intermittently on x86_64 and failed both aarch64 runs of 2026-09-19, always with `every manifest
service must report online`, 9 of 24.

WHAT THE LOG CLAIMS: `ProcessService: no artifact at vol://system/libexec/resource_manager.lsexe`
and six more, plus `DeviceManager: <driver> is named by the registry and not on the volume` for
eleven driver kinds.

WHAT IS TRUE: the x86_64 runs at 07:26, 07:31 and 07:38 on 2026-09-19 boot one medium,
`sha256 5010eb2e...`, against one system volume image. Only the middle one reports anything missing;
the other two report `wasi_host.lsexe` alone, which is deliberate. Reading the names out of
`.build/boot/system-volume-x86_64.img` finds every one of the eighteen.

THE MECHANISM: `storage: vol://system mounted through its block provider` precedes the first "no
artifact" by two hundred lines. Between them, all four `virtio-blk` instances and then
`virtio-console`, `virtio-scsi`, `virtio-snd`, `virtio-vsock`, `xhci`, both `nvme`, `ahci`, `sdhci`
and `hda` hit `stopped answering its control path inside the deadline its registry entry declares`
and `went away holding published providers; they are withdrawn`. The MOUNT outlives the provider
under it, and every lookup through the surviving mount answers NOT PRESENT.

SO THERE ARE TWO THINGS HERE AND NEITHER IS A DRIVER:
- StorageService answers `not present` where the honest answer is `the provider under this mount was
  withdrawn`. That is what sent this session looking at the build twice.
- Nothing remounts after the manager restarts the driver (`restarting virtio-blk` is four lines on),
  so a recoverable transient ends the boot.

THE TRIGGER IS CONTENTION AND IT IS MEASURED: bind windows of 126 to 177 ticks in the failing boot
against 7 to 87 in a passing one of the same medium. The cascade predates this session and tracks
the device count - nine drivers on 2026-08-27, thirteen on 2026-09-16, sixteen on 2026-09-19 - over
a span in which the harness gained a third SCSI target, an IDE disk, a duplex HDA codec and a USB
audio function. Widening the deadline would weaken the criterion that caught it.

AND THE GATE THAT EXISTS DOES NOT COVER IT. `check.sh --gate test-tags` proves that every tag a
`tagged_test!` uses is declared in `define_test_tags!`, and it self-tests by refusing three injected
defects before it approves anything. What it does not compare is the list the HARNESS advertises
against the set the kernel parses, which is the only place these two can disagree - and they do.
The fix is two parts: the character class in `test.sh`'s `--list-tags`, and a rule in that gate that
reads both sides and requires them equal. Held until the port runs finish, because `test.sh` and
`src/harness/` are inputs to the build digest and an edit during a run invalidates it.

FIXED (2026-09-19). `test.sh --list-tags` takes the hyphen, and `check-test-tags.sh` compares the
declared wire names against what the harness prints, refusing when they differ and naming both
directions. Proven to refuse on three fixtures before it approves - a truncated name, an advertised
name the kernel does not have, and an agreeing pair in a different order - and then the original
defect was reintroduced and the gate refused it with the exact four-and-eleven split above.

## The foreign-facilities gate moved one step and is still not this work's (2026-09-19)

The note above from 2026-09-13 records `foreign-facilities-guest` as red because `abiprobe` is not
staged, and puts it with the foreign static substrate rather than with this milestone. Running the
whole gate set at the end of this session reproduced it, and the gate names its own precondition:
`LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`.

THAT PRECONDITION IS NOW MET AND THE FAILURE MOVED. `.build/image/x86_64-unknown-none/libexec/
abiprobe` exists after the development build, the gate gets past the staging check, boots the guest,
runs the probe - and reports `abiprobe printed nothing at all`. So the state it was in ("not built
into the image") and the state it is in now ("in the image and silent") are different failures, and
the second is inside the foreign substrate, which this session touched in no file.

RECORDED RATHER THAN CHASED, for the same reason the earlier round gave: nothing under `src/` that
this work changed is in that path, and a gate belonging to another milestone is that milestone's to
close. What changed here is only that its stated precondition no longer hides the real failure.

## UAS tagged concurrency and task management (2026-09-19)

The item's two remaining open points were one piece of work, and its note said what was missing in
one sentence: per-stream rings and a completion routed by stream. Both are in.

THE TRANSPORT: `Streams` holds a ring per driven stream instead of one ring at stream 1 - two
command tags and one task-management tag - with a data page and a control-page region per tag, and
a bind that REFUSES a device advertising fewer entries than this driver drives rather than clamping.

THE ROUTING: an xHCI Transfer Event carries no stream id, so the stream is read out of the TRB
pointer - each ring is its own page, so the page the completed TRB lives in says which tag answered.

THE SERVING: a block request is posted and left outstanding, and the loop that drains events answers
the consumer that asked. Concurrency is ACROSS consumers because `driver_protocol::block` carries no
correlation id, so replies on one channel must stay in order.

TWO THINGS THE MACHINE TAUGHT AND REASONING DID NOT:
- NOTHING IN THIS DRIVER IS WOKEN BY A COMPLETION. It has always polled the event ring. The first
  asynchronous version posted two commands and slept holding both. Completions are reaped by
  polling now, on a synchronous request's budget, servicing HID and network events on the way past.
- FILLING MUST PRECEDE REAPING. With events drained before the next consumer was served, each
  command finished before the next was posted: the suite passed and proved nothing. The serve arm
  now takes every already-ready UAS consumer while a tag is free, then reaps.

EVIDENCE: `2 UAS commands outstanding at once, each on its own tag` and `task management answered`,
both printed by the driver on a live QEMU device, and both absent from the arrangements that do not
do it. Two mutations, each killing the guest at 100 tests: every request on tag zero, and the
management answer posted on the command stream.

STILL OPEN: the three-target acceptance, which has to be measured AGAIN - the run of 2026-09-18 was
against a one-tag transport that no longer exists.

## HDA's second codec, and the defect one codec was hiding (2026-09-19)

The harness had never attached two codecs, and with one on the link "take the first that answers"
and "walk the codecs until one has a route" are the same code - so the open point could not be
closed by reading the driver. `hda-micro` now sits beside `hda-duplex`.

THE DRIVER TRIES EACH CODEC IN TURN, because a codec that answers its identity and then has no
audio group, no output converter or no pin that can drive one is not a bring-up failure while
another on the same link is fine. The refusal reported when none works is the LAST one's.

AND THE DEFECT: `rirb_read` WAS RESET WHEN SWITCHING CODECS. The response ring is ONE ring shared by
every codec, and that field is where the driver has read up to in it, compared against the
controller's write pointer. Resetting it makes the driver re-read entries it has consumed and take
an old answer for a new one. It was harmless for exactly as long as one codec was probed and the
loop broke out of it. THE FIRST TWO-CODEC BOOT FAILED with `no pin complex reaches an audio output
converter` - a widget walk reading stale answers - and that run is the mutation.

EVIDENCE: `driver.hda: online (codec 0 of 2, converter 2)` on all three targets.

## Two open points answered with a measurement instead of a guess (2026-09-19)

SDHCI's UHS modes and eMMC commands, and HDA's unsolicited responses, were listed as work and none
had been checked against the machine.

- UHS: the driver now reads `CAPABILITIES_1` bits 0-2 and reports `no uhs offered`. A controller
  with no faster mode and a driver that never asks for one look identical from outside; the line is
  what tells them apart, on this machine and on any later one that does offer it.
- eMMC: `-device help` lists `sd-card` and `sd-card-spi` and no eMMC part.
- HDA unsolicited responses: `-device hda-duplex,help` offers `audiodev`, `cad`, `debug`, `mixer`
  and `use-timer`. No jack presence and no runtime control of one, so the event cannot occur.
- USB Audio capture: `-device usb-audio,help` offers an output backend and more output channels.

## The ports, re-measured because the drivers changed (2026-09-19)

`drivers,pci,slow,usb`: aarch64 100 tests in 1513 s, riscv64 102 in 1822 s. Both print what x86_64
prints - `2 UAS commands outstanding at once, each on its own tag`, `task management answered`,
`codec 0 of 2`, `no uhs offered`. UAS and HDA closed on it; 34 items done, 30 open.

## The hot-plug fixture, and four tests that were overwriting the kernel (2026-09-19)

THE FIXTURE'S ABSENCE WAS HALF A STALE ASSUMPTION AND HALF A CIRCLE. The harness kept the
`pcie-root-port` off the emulated test profiles because "adding a function would renumber the bus
addresses several oracles print" and because "no test asserts on it yet". The first is true of a
function added AMONG them and false of one added after them - QEMU assigns addresses in argument
order - and the second was circular. The port is appended last now; the driver oracles print the
same addresses as before (`ahci 00:0b.0`, `sdhci 00:0c.0`, `hda 00:0d.0`, `xhci 00:05.0`), and the
aarch64 boot says `pci: 00:12.0 carries hot-plug slot 1 - empty` for the first time.

THE ORACLE ASSERTS A MACHINE SHAPE: that the scan reaches a port, that the port with the harness's
platform slot number is among those found, and that it is empty.

AND IT FOUND A DEFECT ON ITS FIRST RUN. `arm_hot_plug_slots` and `poll_slots` work through one
static, and four in-guest tests drive them over a synthetic bus - so after those tests the RUNNING
kernel held their fake: a port at `00:00.0` with slot number zero. The idle pass polls that table.
Nothing had ever asked it, so nothing had ever noticed; the tests assert on their own return values
and pass either way. The oracle failed on aarch64 and passed on x86_64 - the same tests in a
different order, which is the signature of shared state rather than of a machine.

THE FIX IS A GUARD, NOT A RULE: `HeldPorts` snapshots the table and the slots-reported flag and puts
both back on drop, including on a panic. With it the oracle passes on aarch64 where it failed, the
guard being the only difference. On all three ports: x86_64 104, aarch64 79, riscv64 81, and both
emulated boots print `carries hot-plug slot 1 - empty` where neither had printed it at all.

STILL OPEN IN THAT ITEM: a slot CHANGING state and an error record read back on the emulated ports.
Both need `device_add` against a live machine and a function with error reporting on, and the
tooling for both is x86_64.

## The driver-mutation gate was red, and the cause was this milestone's own new code (2026-09-19)

DRV-016 asks for tests over unsafe device logic and its record said "the strategy is working". That
is a claim, and the thing that checks it is `check.sh --gate driver-mutations`: a planted defect per
decision, each named for the test that must catch it.

IT WAS FAILING. Not because a mutation survived - because two ANCHORS had stopped being unique. A
defect is located by a line of source, and the recent work duplicated two of them: NCQ's
`queued_outcome` writes its error test exactly like the single-command path's, and
`task_management_iu` writes its tag exactly like `command_iu`. The gate refused rather than planting
the defect in whichever site it found first, which is the right refusal - and it is the only reason
this was visible, since nobody had run the gate since that code landed.

BOTH ANCHORS NOW CARRY THE LINE THAT TELLS THEM APART, and the two new decisions got mutations of
their own: a queued tag is outstanding while its `PxSACT` bit is SET (the opposite register and
sense from `PxCI`), and a task-management header carries the REQUEST'S tag rather than the doomed
command's. Both are caught by the tests written for them.

AND THE COVERAGE QUESTION WAS ANSWERED WHILE LOOKING: every decision module in the crate has a
fixture. The only file without one is `dev_channel.rs`, a transport program's entry point whose
logic lives in the shared `serial_port` module, which does.

40 of 40 planted defects caught.

## The emulated ports had no framebuffer at all, so a path nobody could run stayed unrun (2026-09-19)

The GOP item carried "NOT YET VERIFIED ON THE TWO EMULATED PORTS ... they are unrun, and the final
tri-architecture pass is what confirms them". The pass happened and confirmed nothing: both emulated
TEST profiles boot with no display device, so every run printed `no GOP framebuffer (serial-only boot
log)` and the descriptor code was never reached.

A `ramfb` on those profiles is what changed it, and it is the cheapest thing that could: firmware
finds it, the loader builds the descriptor, nothing binds to it. Deliberately not `virtio-gpu` - that
is the heaviest bring-up here and once put itself past the two-second READY deadline under TCG,
taking DisplayService and five services with it. ramfb has no bring-up to be late for.

AND THE TWO PORTS TOOK THE TWO HALVES OF THE ITEM'S OWN TITLE:

    aarch64   loader: GOP framebuffer found
    riscv64   loader: no GOP framebuffer ... / riscv64: ramfb framebuffer 1280x800 at 0x80e5e000

The riscv64 one is the more valuable: the simple-framebuffer path is what a machine without UEFI
takes and no boot anywhere ran it. Suites green - aarch64 79, riscv64 81.

THE LESSON IS ABOUT THE CLAIM AND NOT THE CODE: "the final tri-architecture pass confirms them" was
false the day it was written, because a pass cannot exercise a path the profile gives no device for.
A fixture was the missing half, not a run.

## The scenario runner reaches all three ports; the development boot does not (2026-09-19)

Two gates - `qemu-pcie-hotplug` and `qemu-pcie-aer` - were recorded as x86_64-only because they
drive the persistent instance through `lab.sh monitor`, and the persistent instance is x86_64's. A
SCENARIO runs cold on all three, which is what `scenario-cold` exists for, and the scenario
vocabulary had fifteen step kinds and no way to reach the monitor: a scenario could type at the
guest and move its pointer, and could not change the MACHINE.

A `monitor` step is the whole of what was missing. Its ANSWER is the failure rather than its exit
status, because this monitor reports in prose - `device_del` on a bus that cannot do it answers
`Bus 'sata.0' does not support hotplugging` and looks like success to anything checking acceptance.
One command per step, a newline refused, so a failing step can say which command failed.

AND THEN THE EMULATED RUN FOUND SOMETHING BIGGER. A cold aarch64 development boot never serves its
control channel, and the log gives the reason in the words the booted-system test uses: ten drivers
missed their control-path deadline, seven had their providers withdrawn, and every service then
failed with `no artifact at vol://system/libexec/...`. The volume mounted and lost what was under it.

SO "THE TOOLING IS x86_64-ONLY" WAS A SYMPTOM. The runner is not x86_64's; the development profile's
BOOT is what does not survive on the emulated ports, because it brings up the most devices at once
on the slowest machine - which is the condition the cascade needs. One defect explains both the
intermittent boot-test failure and why scenarios have never run on aarch64 or riscv64.

TWO FIXTURE MISTAKES OF MINE, BOTH THE SAME SHAPE. The HDA second codec and the ramfb both went into
a code path shared with a profile they were not meant for: the codec broke `qemu-pcie-hotplug` on
the interactive machine, and ramfb collided with that profile's own ramfb outright
(`duplicate fw_cfg file name`). Both are behind a `TEST` guard now. A fixture one profile needs is
attached by that profile.

## The ACPI SCI, and three "device models" that are not (2026-09-20)

THE WALLS GET CHECKED, NOT QUOTED. Three items in group 3 - CDC-ACM, the HID expansion and USB
Audio's capture half - each said what they need is "a device model to write, not a driver", and I had
read all three as an implementer's dead end. Running the commands instead: this QEMU registers
`usb-host` AND `usb-redir`, so what a guest can see is not the set of models QEMU implements;
`dummy_hcd`, `libcomposite`, `usb_f_acm`, `usb_f_hid`, `usb_f_uac1` and `usb_f_uac2` are all built
for the RUNNING kernel; and `modprobe dummy_hcd` succeeds on this machine and produces
`/sys/class/udc/dummy_udc.0`. A configfs gadget binds to that and `usb-host` hands it to the guest.
`usb_f_hid` takes an arbitrary report descriptor, which is precisely the multi-touch collection and
the gamepad the HID item has no model for.

SO THE WALL IS REAL AND IT IS A DIFFERENT WALL. It is not code that must be written; it is the
harness loading kernel modules into the DEVELOPER'S kernel and building a gadget as root, on every
run. That is a decision about a test suite's blast radius and it is the project owner's - an
implementer who quietly taught `test.sh` to modprobe things would have taken it for them. It is
question 4 in the head of the milestone now. The probe was undone: the gadget tree removed and every
module unloaded, so the machine is as it was.

THE SAME METHOD PAID AGAIN ON ACPI. The fixed-hardware event item named its own missing fixture - "a
QEMU machine whose PM1a event can be raised on demand" - and that fixture had arrived without anybody
noticing: `system_powerdown` on the monitor IS a PM1a event, and the `monitor` scenario step written
for PCIe hot-plug is what can send it. One item's tool closed another item's gate.

AND THE I/O APIC WAS ROUTING TWO OF ITS THREE LINES AS THE WRONG KIND. `route` wrote the ISA
defaults - edge-triggered, active-high - for every caller. That is right for the 16550's legacy line
and wrong for a PCI INTx pin, which is level-triggered and active-low by the bus specification. The
hot-plug path's own comment already named the consequence without connecting it to the cause: "a
level-triggered line one handler already cleared" is a line whose EDGE nobody saw, and the poll
beside it is the fallback that has been covering for it. Both are level, active-low now; the
condition a level entry needs - the source cleared inside the handler, before the EOI - is what
`poll_slots` already does with the slot's sticky bits.

## The SCI landed, and the userspace half did not answer on the first two runs (2026-09-20)

THE KERNEL HALF WORKED ON THE FIRST BOOT:
`acpi: SCI is GSI 9 (level, active-low) on vector 41` and
`acpi: power and sleep buttons armed on PM1 status 0x0600, enable 0x0602` - the FADT read, the MADT
override applied, the half-block arithmetic right - and `system_powerdown` on the monitor produced
`platform: event 1 arrived before anything was listening`, which is the handler decoding and
acknowledging a real PM1a event, in an interrupt, with the status bit in hand.

THE USERSPACE REGISTRATION PRINTED NOTHING - not its success line, not either of its failure lines -
and a first theory about WHY was wrong and is recorded here because it was nearly written into the
milestone as a fact. The theory was that `.build/cargo/development`, which holds an eighteen-hour-old
`device_manager`, is what the guest boots. It is not: that directory belongs to
`check-development-build.sh`, a gate that COMPILES the other configuration and boots nothing. The
guest's programs come from `.build/cargo/user` by way of `.build/boot/bootstrap-<arch>`, and both of
those did carry the new code. Two builds were run on the strength of the wrong theory before
`grep -rn cargo/development --include=*.sh` said who writes that directory, which is the question
that should have been asked first.

WHAT DID COME OUT OF IT AND IS WORTH KEEPING is a diagnostic decision. The bus-event registration
this one was modelled on is SILENT in two of its three failure arms - no channel, no privilege - so
a DeviceManager that failed to register would leave a power button that does nothing and a log with
nothing in it about why. The platform registration says which of the three happened, every time.

## An interrupt handler may not send on a channel (2026-09-20)

THE SYMPTOM WAS A MACHINE THAT PRINTED THE EVENT AND THEN DID NOTHING. With a listener attached,
`system_powerdown` produced `acpi: the power button was pressed` and the press never came out the
other end: DeviceManager's registration had succeeded (both sides say so in the boot log), the send
reported no error, the consumer's drain - which runs before every park, not only when the wait names
the handle - found nothing, and every arm of its handler is instrumented and none of them spoke.

THE DISCRIMINATING OBSERVATION WAS THE RUN THAT WORKED. With NO listener attached the same handler
returned cleanly and the guest carried on; with one attached the guest made no further progress, the
shell never answered again and the harness reported `teardown did not complete`. The only code the
listener adds is `Channel::send`, which takes the peer's inbox lock and then `sched::wake_object`.
Both are locks an ordinary thread can be holding at the instant a hardware interrupt arrives - and
this interrupt arrives on a SHARED line at an arbitrary instruction boundary.

SO THE HANDLER RECORDS AND THE IDLE PASS DELIVERS. `platform_event::report` is called from the
interrupt and now does exactly one thing: set a bit in an atomic. `platform_event::deliver` is
called from the BSP's idle pass, beside `settle_hot_plug` and `console_input::drain`, and does the
allocation and the send. The latch semantics are unchanged and are now the same mechanism as the
delivery: PENDING is both.

AND THE SAME SHAPE IS NEXT DOOR. `device::report` - the bus arrival and departure channel - does the
identical allocate-and-send, and `settle_hot_plug` calls it from BOTH the interrupt and the idle
hook. The idle path is what has been doing the work; the interrupt path has the same hazard this one
had, and the poll beside it is what has been covering for it. That is worth its own look by whoever
owns the hot-plug item, and it is written here rather than fixed in passing.

## Nine runs on one delivery, and what each one removed (2026-09-20)

THE ACPI MECHANISM PASSED ON THE FIRST BOOT AND THE CONSUMER HAS NOT PASSED IN NINE. Recording the
shape of that, because the method is the point and the ending is not tidy.

WHAT EACH RUN REMOVED FROM THE SEARCH, in order:
  1. The kernel half works: FADT, MADT override, level/active-low routing, ACPI mode, PWRBTN armed,
     the interrupt decoded and acknowledged - `acpi: the power button was pressed`, 201 ms after the
     monitor command.
  2. "The consumer's binary is stale" - checked with `strings` against the string just added, twice,
     and wrong both times. A theory about `.build/cargo/development` was wrong too: that directory
     belongs to a gate that compiles and boots nothing. Asking `grep -rn cargo/development
     --include=*.sh` who writes it would have cost one command and saved two builds.
  3. "The registration failed silently" - it did not; both sides print it now, which took a boot to
     learn and is kept.
  4. "The send was refused" - it was not; a refused send has its own line and never appears.
  5. "The handler wedges the machine" - it DID, and that was a real defect: `Channel::send` from an
     interrupt handler takes locks an ordinary thread can hold. The delivery moved to the idle pass.
     The symptom that proved it: with a listener attached the guest made no further progress, with
     none attached the same handler returned cleanly.
  6. "The channel is wired wrongly" - a probe sent through that same channel arrived and was decoded
     by the consumer's own handler. That removes the naming of the two ends, the privilege, the
     handle and the message shape in one run.
  7. "The consumer is asleep on a wake that never came" - instrumented, its loop made thousands of
     passes over that channel in one run.

SO WHAT IS LEFT IS THE SENTENCE NOBODY WANTS: the kernel enqueues a message on a channel, reports
success, and the consumer polling that same channel thousands of times does not see it. Every
cheaper explanation has been measured away. Writing a cause into the milestone now would be writing
a guess, and that is the one thing this tree's files are not for.

THAT EXPERIMENT WAS RUN AND IT REMOVED THE LAST TWO GUESSES. The kernel prints the koid of the
endpoint it holds and the consumer prints both of its handles' koids: the kernel holds 65, the
consumer gave 65 and kept 66, and the send at press time is on 65 with `peer closed false` and its
OWN inbox not readable. So the two objects are the two halves of one pair, the peer is alive, and the
message went to the peer rather than back to the sender. The message is in the consumer's own
channel's inbox and the consumer does not take it.

AND THE LAST MEASUREMENT SAYS WHY, WITHOUT SAYING WHAT CAUSES IT. Counting the consumer's drain
calls: with a diagnostic printing every sixteenth call the loop spun thousands of times - but the
printing was itself the cause, because each print makes the console channel readable and wakes the
loop. Without it, the drain runs during bring-up and NOT AGAIN after the press, over sixty seconds
of waiting, with a deadline in its wait that should have brought it back every second. So the
consumer is parked somewhere its own deadline does not reach, and the only place in that loop with
no deadline is the blocking receive on the supervisor channel.

TWO MORE RUNS REMOVED THE TWO REMAINING CANDIDATES AND THE ITEM IS STILL OPEN.

FIRST: the blocking receive was real and is fixed, and it was not the cause. `recv_blocking` is a
SECOND WAIT in front of the one wait - on `ERR_WOULD_BLOCK` it calls `wait(channel, 0)`, one handle
and no deadline - which is exactly what the note forty lines above that loop forbids in its own
words. It is a non-blocking take now, and an empty read is a pass rather than a park. The boot is
unchanged and the press still does not arrive.

SECOND: the channel does not answer `Closed`. `try_recv`'s error arm mapped every negative result
onto `Closed` and the loop broke on it SILENTLY, which was a real hole; a closed channel is reported
now and the handle is given up rather than polled for ever. It never fires.

SO EVERY BRANCH ON THE CONSUMER SIDE IS INSTRUMENTED AND ALL OF THEM ARE SILENT: the drain is called
before every park and in the wake branch, an empty message speaks, an unknown kind speaks, a closed
channel speaks, a refused power connection speaks. The kernel says the message was handed over, on
the right object, with the peer alive and its own inbox empty. Eighteen runs, and the one thing that
would explain it - the consumer not reaching those lines - is contradicted by the same code path
having drained a probe earlier in the same boot.

WHAT I AM NOT DOING IS GUESSING PAST THAT. The kernel half of this work is proved and is worth
having on its own; the consumer half is written down with every measurement that constrains it, and
the next person starts from a much smaller search than I did.

AND THE DIAGNOSTIC LESSON IS THE GENERAL ONE. Four of these runs went to distinguishing states a log
could not tell apart: "it never arrived", "it arrived and could not be delivered", "it was delivered
and nothing was done". Each silent arm - `let _ = send(...)`, a `None => 0` with no print, a `_ => {}`
in a match on a wire value, a report printed only in the failure case - cost a six-minute boot. They
all speak now, and that is not instrumentation to be taken out afterwards; it is what the code
should have said in the first place.

## The nineteenth run, which moved the question to another module (2026-09-20)

`Channel::peer_koid` exists now, and the delivery line names BOTH ENDS:

    platform: event 1 handed on, from object 65 to Some(66)
    DeviceManager: ... polling koid 66

THE SAME OBJECT, AT BOTH ENDS, AT THE SAME MOMENT. The message goes into endpoint 66's inbox under
that inbox's own lock; `send` returns `Ok`; the process holding endpoint 66 polls it thousands of
times over sixty seconds and `try_recv` answers `Empty` every time - which it does only when
`peek_identified` found nothing queued. The two other answers that path can give each have a line of
their own now and neither ever appears.

AND AN EARLIER MESSAGE BETWEEN THOSE SAME TWO ENDPOINTS ARRIVED, in the same boot, decoded by the
consumer's own handler. So the pairing, the handle, the privilege, the rights and the message shape
are all proved by construction; what differs between the delivery that works and the one that does
not is WHEN it was sent and FROM WHAT CONTEXT.

THAT IS A QUESTION ABOUT `object::channel` AND NOT ABOUT A POWER BUTTON, and it is where this
investigation ends rather than where it fails. The diagnostic that answers it in one boot is the one
that was missing all along and is now permanent: a delivery line that says which object a message
left and which one it reached. "It was sent" is not an answer when it does not arrive.

## Five waits with no end, in one supervisor loop (2026-09-20)

COUNTING THE LOOP'S PASSES IS WHAT BROKE IT OPEN. The drain on the platform channel runs before
every park, so counting its calls counts the loop. Logarithmically, through `debug_write` so console
takeover cannot swallow it:

    drains 1, 2, 4, 8, 16, 32 ... and then nothing, ever

The last pulse lands in the middle of bring-up. After it the loop makes no further pass - through a
chassis power button press sixty seconds later that the kernel had enqueued, on the right object,
and said so. DeviceManager was not slow and not asleep on a lost wake: it was PARKED, in a blocking
IPC call, with everything it supervises behind it.

AND THE FIRST TWO COUNTS WERE MEASURED WRONG, WHICH IS WORTH THE NOTE. An earlier version of the
same counter used `print`, which routes to ConsoleService the moment a program has a stdout - so
from display takeover onwards the counts went to a virtual terminal and the log showed one line. I
concluded from that single line that the loop had stopped, and separately that a hot spin existed
when the printing itself was causing it. Two wrong conclusions from one instrument that changed
destination halfway through the run.

**FIVE PLACES IN ONE LOOP WAIT WITH NO END, AND THE LOOP'S OWN NOTE FORBIDS ALL OF THEM.** Forty
lines above it: "One wait, so a catalogue query cannot delay a supervisor message and a supervisor
message cannot delay a query." Found, in the order they fire:

  1. THE SUPERVISOR MESSAGE. `recv_blocking(bootstrap, ...)` on the fall-through.
  2. THE DEVELOPMENT AGENT. `dev.supervise` was `recv_blocking` on the agent's bootstrap - the agent
     speaks during bring-up and then goes quiet.
  3. THE PROVIDER CATALOGUE. `serve_catalogue_once` opened with `recv_caps_blocking` on ONE client.
  4. THE DEVICE POLICY. `serve_policy_once`, the same line.
  5. AND THE DRIVER SENDS. `send_frame` used `send_blocking`, which waits for ROOM with no deadline,
     and the loop sends a heartbeat to every bound driver on every pass. One driver that stops
     reading parks the supervisor.

EACH FIX MOVED THE WALL LATER, which is how the count of them became visible at all: the last pulse
went from line 209 to 222 to 223 as the earlier ones were removed. A single instance would have been
a defect; five is a property of the file, and the property is that its stated rule was never enforced
anywhere.

WHAT EACH BECAME. A receive that would block is a PASS - the handle is in `waiting` like every other,
so going round is what puts it back under the one wait. A reply that would block retires the client,
which is what this loop already does with a closed one. A driver send waits a bounded second and
then answers `false`, which is what the heartbeat is for.

AND ONE CHANGE WAS REVERTED FOR A REASON WORTH KEEPING. Adding a function to `rt` broke the build:
`abiprobe` and `vkprobe` embed the hash of the `icdprobe` they admit, and they are staged rather
than rebuilt, so ANY change to the shared runtime leaves the staged tree holding "a selection
candidate no consumer was built against". The bounded send lives in DeviceManager and uses only what
`rt` already exports.

## And the rule became a gate (2026-09-20)

FIVE INSTANCES OF ONE MISTAKE IN ONE FILE, NONE VISIBLE TO ANYTHING, IS NOT A DEFECT TO FIX QUIETLY.
`src/tools/check-supervisor-waits.sh` refuses any blocking receive or blocking send inside
DeviceManager's supervisor loop, and it proves it refuses before it approves - it plants one after
the loop's own anchor comment and requires itself to reject it. Registered as
`./check.sh --gate supervisor-waits`.

THE MARKER IS A DECISION AND NOT AN ESCAPE HATCH. A call that genuinely must wait carries
`SUPERVISOR-WAIT-OK:` with a reason, anywhere in the comment block directly above it - which is how
this tree writes reasons, and requiring the marker on the last line would either compress every
reason into one sentence or separate it from its call.

AND THE GATE TURNED THE REST OF THE HUNT INTO A LIST. Thirteen calls, each of which then had to be
DECIDED rather than found:
  - three sends to a driver being launched: bounded, like the heartbeat. A driver that never starts
    reading must not park the program that started it.
  - nine sends to the supervisor: they keep their wait and say why. The supervisor drove that
    handshake and is waiting for exactly those answers, so the peer that could stall the send is the
    peer that asked for it.
  - one attenuated driver send: keeps its wait. Moving a capability is once per driver per
    transition, not once per pass, and a transfer that gave up half way is harder to be right about
    than one that waits.

WHICH IS THE ANSWER TO "IS THE SIXTH INSTANCE STILL OUT THERE". Inside this loop, no: every call is
now either bounded or a written-down decision, and the gate keeps it that way. Whether the loop then
reaches the press is a measurement and not a deduction, and it is the next run.

## The gates found the one thing I had not checked (2026-09-20)

RUNNING THE x86_64 GATES FOUND EXACTLY ONE DEFECT OF MINE, AND IT WAS THE ONE THE SCENARIO COULD NOT
SEE. `check.sh --gate implementation-mutations` failed with
`could not compile kernel (bin "kernel" test) due to 5 previous errors`, and all five are mine:
`ioapic::Kind`, its two redirection-entry bit constants, `Kind::bits` and `port::inw`. Every one is
reachable only from `route` or from `arch::sci`, and BOTH are `#[cfg(not(test))]` - so in the test
build they are dead code, and warnings are errors here.

**THE TEST KERNEL IS A SECOND COMPILATION AND `./build.sh --part kernel` IS NOT IT.** I built the
shipping kernel a dozen times, it said `RESULT ok` every time, and the kernel `test.sh` runs did not
compile at all. That is a worse failure than the dead code being complained about: the whole in-guest
suite cannot run. It is fixed by giving each symbol the same `cfg(not(test))` its only caller has,
which is what `route` already carried and what I did not follow through.

AND IT IS WHY THE SWEEP WAS WORTH THE HOURS. Twenty-eight scenario runs proved the ACPI path and
could not have found this, because a scenario boots the shipping kernel. The check is seconds:
`cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests`.

### What the rest of the red was

Every other failure in the x86_64 sweep is one of three things, and none is a regression:

  - PRE-EXISTING, PROVED RATHER THAN ASSUMED. `bootstrap-plan` anchors on `service_manager.rs`,
    which the commit does not touch. `dma-mode-carrier` reports on `check-run-verdict.sh`, likewise.
    `guest-verdict` fails because `virtio-multiport` is missing from the P02M0177 inventory, and
    that gate was added by somebody else. `driver-event-dispatch` fails IDENTICALLY when pointed at
    the pre-commit source with `--source`, which is the cleanest proof available.
  - ORDERING ARTEFACTS OF MY OWN BUILDS, which is the shape this tree already records twice.
    `capability-trace` wants a trace newer than the kernel beside it; `development-gate` wants the
    shipping volume and found the development one; `foreign-facilities-guest` and
    `foreign-loader-guest` want the development staging and found the shipping one. The last two and
    `development-gate` cannot be satisfied at the same time, which is the same fact as
    "check.sh cannot go green in a single pass".
  - OUT OF SCOPE FOR AN x86_64 RUN. `dma-mode-ports` and `iommu-ports` need an aarch64 build; my
    filter let them through because their scripts only MENTION the other ports.

AND `driver-event-dispatch` WAS IMPROVED ON THE WAY PAST. Two of its anchors named `unsafe fn
advance(` and `unsafe fn open_subscription(`, neither of which has existed in either version of the
file - so the gate answered "substring not found" and checked nothing. With the anchors corrected it
reaches its real assertion and says which one fails, which is the difference between a gate that is
red and a gate that is mute.

## A wait with no end found by reading, which was NOT this symptom's cause (2026-09-20)

**THE HEADING ON THIS SECTION SAID "the eighth defect" AND THAT WAS WRONG, WHICH IS CORRECTED HERE
RATHER THAN QUIETLY EDITED AWAY.** The measurement that settled the power button came afterwards and
is in its own section below: the consumer half was working, and what hid it was the serial ring. The
defect described here is real, was found by reading `rt`, and is fixed - but nothing measured says it
is what kept the press from coming out, and the run that proved the chain already had this fix in it.

**`rt::print` IS A WAIT WITH NO END, AND THE SUPERVISOR CALLS IT ON EVERY PATH.** Its body is `if out
!= 0 && send_blocking(out, bytes, 0)`: once this program has a stdout - a console channel handed to
it at start-up - every line it prints is a blocking send on a channel whose reader is a virtual
terminal. From display takeover onwards nothing drains that terminal, so the first line the
supervisor prints after takeover parks the whole loop, for ever, with the process alive and its
handles open. That is the shape the boot log shows: the last DeviceManager line sits directly after
`ConsoleService: a frame reached the display`, and then nothing at all - no chassis event, no
catalogue answer, no teardown - while the kernel says the press was handed on and the queue holds it.

The gate this file added, `check-supervisor-waits.sh`, reads call NAMES. `print(` is not one of them,
so a park one call deeper was invisible to it and to every reading of the loop. What it costs is a
supervisor that can be stopped by a terminal nobody is draining; what it cost HERE is nothing that
was measured.

FIXED BY DEFINING `print` IN THE PROGRAM. `use rt::*` is a glob import and an item defined in the
file wins over it, so one definition covers all two hundred call sites: send the line if the console
has room, write it to the machine log if it does not. Nothing is lost and nothing waits.

**AND A SECOND DEFECT IN THE SAME AREA, WHICH WAS THIS SESSION'S OWN.** The bounded park added for the
platform channel used plain `wait_any`. `sched::min_deadline` skips PERIODIC waiters and nothing else,
so a supervisor that re-arms a one-second deadline for ever keeps `run_until_idle` going for ever -
the trap the console loop's note describes, and why ConsoleService polls with `wait_any_periodic`. The
park is now periodic WHEN THE POLL IS THE ONLY REASON TO WAKE, and an ordinary wait whenever a retry,
a stop deadline or a teardown is pending: those are work in flight and have to stay visible as such.

**AND THE TWO NOTICES ABOUT THE PLATFORM CHANNEL NOW GO TO THE MACHINE LOG** rather than through
`print`, for the reason `machine_log` already states: a chassis event is a fact about the machine and
belongs in the machine's log, not on a terminal nobody is reading.

**THE KERNEL NOW SAYS WHEN A DELIVERY WAS NO USE.** `Channel::peer_unread` answers how many messages
an endpoint has handed its peer that the peer has not taken, and `platform_event::check_unread` reads
it three seconds after a delivery: if the event is still queued it says so once and dumps every
blocked thread with the koid it waits on. "It never arrived", "it arrived and nobody read it" and "it
arrived and was acted on" were one answer in the log, and that is what a day went into telling apart.

## Four verification checkers were reading a source shape that has not existed since P02M0178 (2026-09-20)

`check.sh --gate driver-event-dispatch` runs five checkers. Commit ca97d317 removed propagated
`unsafe` from the runtime wrappers, which deleted an `unsafe { ... }` block from several DeviceManager
functions - so every anchor that named `unsafe fn X(` stopped matching, and every anchor that counted
TABS was one level too deep. The gate has been failing on its FIRST anchor ever since, which is why
nothing behind that anchor was ever reached.

Re-aimed, with the reason recorded beside each: `check-driver-event-dispatch.py`,
`check-device-manager-progress.py`, `check-device-claim-result.py` and `check-driver-lifecycle.py`.
Their extracted fixtures also needed the source's later shape: a provider carries a NAME, a claim
names its ENTRY and the grant carries the kernel's stamp, an offer carries the provider name, the
node records `bind_at`, and the bounded subscription reply is `send_with_room`. Each of those now
compiles against today's source, and the mutation batteries behind them reject their mutants again:
`device-manager-progress` (8 mutants), `device-claim-result` (3 mutants) both pass.

**AND THE THREE LIFECYCLE TESTS WERE ASSERTING A CONTRACT THE SOURCE HAD REPLACED.**
`ready_receipt_deadline`, `first_reply_uses_receipt_deadline_even_without_tick` and
`direct_intake_prioritizes_new_expiry_before_traffic` all encode "the deadline is tested before the
channel is read". `drain_channel` deliberately stopped working that way and says why in its own note,
with the measurement behind it: a deadline is about SILENCE, and there is no silence when the answer
is already in the buffer - measured on a q35 controller whose AHCI driver answered twice and was
declared silent twice, costing two full bind windows. The tests never failed over it because the gate
had already been broken by the `unsafe` removal, so they had not run since.

Re-aimed at the contract that IS in the source, with the reasoning beside each: a READY taken at 99
against a deadline of 100 is timely however late the pass behind it is; one taken AT the deadline is
not. The same on the heartbeat's side for a PONG. And the third keeps the OTHER half of the rule -
a receipt that CROSSES the deadline admits the expiry from the frame's own arrival and takes the last
queue slot ahead of the reply - which needed clocks that make the crossing real (`[14, 16, 16]`,
where the pass opens live and the frame is taken expired). All thirteen pass and all eleven mutants
are rejected, including `crossing receipt misses expiry admission`, which the weaker form of that
test would have accepted.

**WHAT IS STILL OPEN IN THAT GATE AND IS NOT CLAIMED: `check-dev-channel-control.py`.** It models a
`dev_channel.rs` with `heartbeat`, `send_to_agent`, `rx.take_used` and a `pending_bytes` hand-off;
the driver was rewritten on 2026-09-15 and now holds two functions, `pump` and `ended`. Every anchor
in that checker names something that no longer exists, so its whole mutation battery - fifteen
variants - describes a program this tree does not contain. Rebuilding it is deriving an oracle
against a driver from scratch, which is its own task and not a repair; it is the last thing between
`driver-event-dispatch` and green, and the five checkers in front of it now pass.

## THE DEFECT THAT WAS ACTUALLY IT: the machine's last words never leave the ring (2026-09-20)

**`arch::poweroff` AND `arch::reset` STOPPED THE MACHINE WITH THE SERIAL RING FULL.** Once
`serial::enable_async` is on, `write_bytes` ENQUEUES - `drain_tx`, on the idle pass, is what puts
bytes on the wire - so everything said in the moments before an orderly shutdown was still in that
ring when the machine stopped, and was never transmitted. A clean power-off then ends its log
mid-sentence and reads exactly like a crash.

**AND IT IS WHAT THE CHASSIS POWER BUTTON WAS MISSING ALL ALONG.** The consumer half was working:
DeviceManager took the event, said so through the machine log, and asked the power service to stop
the machine. The machine stopped - which is the proof, once one knows to read it that way - and the
line that says the press was acted on died in the ring with it. TWO RUNS, ONE DIFFERENCE. Both booted a guest outside the scenario harness - no handshake, nothing
that could tear it down - and pressed the button through the QEMU monitor. The first, on a build that
already had every other fix from this hunt, ended with `platform: event 1 handed on` and a machine
that was gone when it was looked at again: the press WAS acted on, because nothing else in this
machine turns a guest off. The second, on the same tree plus the flush, ends:

    acpi: the power button was pressed
    platform: event 1 handed on, from object 65 to Some(66)
    DeviceManager: the power button was pressed - asking the power service to stop the machine

and then the machine is off. The flush is the whole difference between those two logs.

`exit_qemu` has flushed for this exact reason since the test report needed to arrive; the two paths a
real machine takes did not. Both call `serial::flush_sync()` now. The other two ports write
synchronously - their `flush_sync` and `drain_tx` are empty - so they have nothing to lose here, and
nothing about them was changed on an unmeasured architecture.

WHAT THIS RETIRES: the hypotheses this hunt accumulated about the delivery path - a wake that does
not arrive, a park in `wait_any`, a queue nobody drains - were all measuring a mechanism that worked.
What was broken was the LOG, in the last hundred milliseconds of the machine's life. The two things
kept from that work stand on their own: `print` in a supervisor must not park (that one WAS a real
wait with no end), and the kernel now says when a delivered event goes unread.

## Three defects between a working power button and the gate that proves it (2026-09-20)

`lab.sh scenario-cold` could not drive a cold guest at all: it waited out its handshake deadline on
every scenario, so the power button's own oracle - and the hot-plug one - could not run. Three
separate things were wrong, and all three are fixed. The scenario now passes in 2.6 s, and the
hot-plug scenario still passes beside it.

**1. A COLD RUN BOOTED A VOLUME ITS OWN BUILD NEVER WROTE.** `mkimage` lays
`system-volume-bootable-x86_64.img` into the medium, and only a volume step asked for the KERNEL ON
THE VOLUME produces that file; `build.sh` on its own refreshes `system-volume-x86_64.img`, which is a
different file with a similar name. So the guest booted whatever that volume last happened to be -
measured here at two and a half hours old and of the SHIPPING configuration, which contains no
development agent at all. DeviceManager's development path then found nothing to start, the guest
served no control channel, and the runner waited thirty minutes for a handshake that could never
arrive, on a tree where everything it was waiting for had been built minutes before. `cmd_scenario_cold`
builds that volume now, for x86_64 only, because it is the only target whose medium carries it.

THAT ALSO EXPLAINS AN EARLIER DEAD END IN THIS FILE. Six DeviceManager diagnostics added to the
development-agent path, and an unconditional probe placed after `launch_volume_drivers` returned,
all "vanished". They did not vanish: the binary carrying them was never booted. The measurement that
settled it was the ISO's own digest, identical across two runs whose DeviceManager differed.

**2. `expect` SWALLOWED THE LINE THE NEXT STEP WAS WAITING FOR.** It advanced the cursor to the end
of everything it had READ rather than to the end of its MATCH, and this scenario is two assertions on
two lines written in the same instant: the kernel's, then the consumer's. The first step read both,
matched the first and consumed the second; the second step then waited out its whole timeout for a
line that had already gone past. The cursor stops at the end of the match now, which needed a small
conversion - the oracles search ANSI-stripped text and the cursor counts raw bytes.

**3. A MACHINE THAT POWERS ITSELF OFF HAD NO WAY TO SAY SO.** The teardown gives a scope back by
DRIVING the guest - a keystroke, a prompt, a reset, a question about what is still held - and this
scenario's success is a machine that is no longer there. It reported `the scenario passed but its
scope was not restored` on the run where everything worked. A scenario declares `ends = "powered-off"`
now, and the teardown asserts the opposite thing: that nothing answers any more.

## A fixture machine QEMU refused to start at all (2026-09-20)

`check.sh --gate qemu-virtio-iommu-x86_64` died in eight seconds with `-device
hda-duplex,bus=hdabus.0,audiodev=snd0: audiodev 'snd0' not found` - before a single guest
instruction. The x86_64 runner attaches an HDA controller and codec to EVERY machine it builds, from
`qemu_attach_nvme`, and names `snd0` on them; the `-audiodev` that DEFINES `snd0` sat inside
`if [[ "$dma_fixture" != "1" ]]` beside the sound CARD. So every DMA-fixture machine carried a codec
with no backend behind it, which QEMU refuses outright.

An `-audiodev` is a backend definition and not a device: defining it costs a fixture nothing. It is
appended for every x86_64 machine now, and the sound card stays inside the branch. The gate passes in
638 s - the controller leaves bypass, five hostile cases are refused by the hardware, an ordinary
endpoint passes real traffic, and the default machine is the isolated one.

The other two ports were checked and need nothing: the only device naming `snd0` there is the
virtio-sound card, which is attached in the same branch as its backend.

## And the full x86_64 suite reproduces a failure this file already carries (2026-09-20)

`./test.sh --arch x86_64` with no tags fails in `kernel.services.a_tty_a_job_left_raw_comes_back_cooked`
with `xab\n` where the test typed `ab\n` - byte for byte the failure recorded on 2026-09-19 as a
full-suite-only order dependence that no scoped selection reproduces. Nothing in this session's work
touches the console input path, and the reproduction is identical, so it stays what it was: open,
reproducible, and not attributed. The run also refreshed the capability trace, which is what
`capability-trace` wanted: that gate passes now.

## `bootstrap-plan` was reading a source shape that has not existed since P02M0178, and behind it were two real disagreements (2026-09-20)

The gate compares two sides of the same fact: the hand-written bootstrap ladder and the role plan the
manifest declares. It could not read either. `check-bootstrap-plan.py` anchors `relaunch_service` on
`unsafe fn` and splits `bootstrap.rs` into function bodies on the same prefix - and P02M0178 removed
propagated `unsafe` from the runtime wrappers, so the body map came out EMPTY. Every service whose
ladder is one helper call then compared an empty sequence against its declared roles. Both anchors
now read a top-level `fn` with or without its prefixes.

WITH THE GATE READING AGAIN, TWO DISAGREEMENTS WERE UNDER IT:

**The graph service is handed a role its manifest does not declare.** `bootstrap_system_graph_service`
sends `DISPLAY` - DisplayService's observation root, minted as a client - between `SUPERVISOR` and
`SERVE`, and `system_graph_service.rs` reads that tag. Only the declaration was missing, so it is
added: a client of `display_service`'s `STATS` root, interface `liber:display@1/display-stats`,
optional for the same reason the ladder guards it. The two sides agree now.

**And `font_catalogue` is declared `restart = "transparent"` by a supervisor that cannot restart it.**
`relaunch_service` re-runs exactly three bootstraps - config, device and graph - because the broker
holds a root for each. There is no font root: the catalogue's `SERVE` and `ADMIN` roots go to
PermissionManager at bootstrap and nothing re-delivers them. So the manifest promises a transparent
restart that would spend a restart budget, reap the endpoints and fail, which is the exact shape this
gate's own note says it exists to prevent.

THE FIX IS NOT A ONE-LINER IN EITHER DIRECTION, AND THAT IS WHY IT IS RECORDED RATHER THAN DONE.
Making the mechanism cover it means a broker root for fonts AND a path that re-delivers a restarted
catalogue's roots to PermissionManager - which is a question about how a restarted provider's roots
reach the component that hands them out, not about fonts. Making the declaration honest instead means
retracting a property a service was given deliberately. This one belongs to whoever owns the
supervisor's restart contract; what this session did was make the gate able to see it.

## The gate accounting at the end of this session (2026-09-20)

TWENTY-THREE gates were run individually and pass: `implementation-mutations` (on a retry - the
mutant kernel met the machine's known intermittent `rustc` failure on the first), `virtio-multiport`,
`source-history-hygiene`, `targeted-cache`, `provider-media-order`, `verify-model`, `source-hygiene`,
`supervisor-waits`, `no-fixed-provider-slots`, `boot-harness`, `development-build`, `capability-trace`,
`qemu-virtio-iommu-x86_64`, `dma-mode-carrier`, `run-verdict`, `milestone-index`,
`declared-interfaces`, `provider-routing`, `guest-verdict`, `kernel-allocations`, plus the two
scenarios (`power button`, `pcie hotplug`) and `test.sh --arch x86_64 --tags boot`.

EIGHT OF THOSE WERE RED WHEN THIS SESSION FOUND THEM, and each is recorded above with what was wrong.
Four were this session's own regressions, four were older: an image that had not been rebuilt, a
trace older than the kernel beside it, a fixture machine QEMU refused to start, four verification
checkers reading a source shape that has not existed since P02M0178, an inventory missing five guest
gates, and four allocation sites the scanner could not tell from real ones.

WHAT IS STILL RED, AND WHY EACH ONE IS NOT CLAIMED:
  - `driver-event-dispatch`: five of its six checkers pass now; `check-dev-channel-control.py` models
    a `dev_channel.rs` rewritten on 2026-09-15 and its fifteen mutants name functions that no longer
    exist. That is deriving an oracle, not repairing one.
  - `bootstrap-plan`: one real disagreement left - `font_catalogue` is declared transparently
    restartable by a supervisor that cannot restart it. Both directions are decisions; see above.
  - `virtio-iommu-protocol`: `check-iommu-completions.py`'s fixture no longer compiles against the
    kernel's own types - `dma_policy::Admission` moved, `Entry::class`, `Slot::entry` and
    `Slot::policy` are gone, and four calls changed arity. The same staleness class, a bigger repair.
  - `no-suppression`: twenty-two lint suppressions, in the kernel's tests and in all three
    architectures' PCI files. Answering them honestly means building aarch64 and riscv64 to see what
    the warnings become, which is the one thing this session may not do until everything else is done.
  - `development-gate`, `foreign-facilities-guest`, `foreign-loader-guest`, `development-lifecycle`:
    each needs a staging state the others exclude, which is the ordering artefact this tree already
    documents rather than a defect.
  - `dma-mode-ports`, `iommu-ports`, `qemu-numa`: aarch64 and riscv64, deferred by the standing rule.
  - `./test.sh --arch x86_64` with no tags reproduces the recorded full-suite-only tty order
    dependence, unchanged and still not attributed.

## The slow-architecture run, which is what it was for (2026-09-20)

**IT FOUND A BUILD FAILURE OF THIS SESSION'S OWN IN ITS FIRST FOUR MINUTES.** `platform_event::report`
is called by x86_64's ACPI SCI handler and by the hardware suite, and by nothing else - so on the two
device-tree ports a shipping kernel reaches it from nowhere and `--deny=warnings` refused the build.
It is scoped now to where it is called from, `#[cfg(any(target_arch = "x86_64", test))]`, with the
note that says what a dead-code warning there would MEAN: that this system has no fixed-hardware
event source on those ports yet, and the day one arrives its port adds itself. Both ports build
(aarch64 after one retry - the machine's known intermittent `rustc` SIGILL - and riscv64 clean).

**AND THEN BOTH PORT SUITES, RUN WHOLE: aarch64 271 tests in 3151 s and riscv64 273 in 3725 s, each
with ONE FAILURE and the same one** - `kernel.boot.init_package_starts_system_manager`, the
end-to-end boot chain. Nine of twenty-four
services report online and the other fifteen never start, every one of them at `ProcessService: no
artifact at vol://system/libexec/...`. The riscv64 boot tag fails identically. The x86_64 boot tag
passes.

WHAT THE NEW STORAGE DIAGNOSTICS SAID, AND WHERE THEY LED:
  - In the full aarch64 suite, one instance hit `vol://system NOT built: the handed-over image does
    not mount as a LiberFS volume`, which is the silent `exit()` this session made speak. The refusal
    is named by kind now - a short image, a foreign one and a machine that could not hold the
    volume's free maps send a reader to three different places.
  - In the BOOT TAG on both ports the live volume is built fine, and the test still fails. The line
    that answers why is the mount that precedes it: `vol://system mounted through its block provider`
    - a DISK - followed by several refusals of other disks. The port's test machine presents a
    formatted scratch disk, that disk becomes `vol://system`, and the handed-over live image never
    gets to be the system volume. x86_64 ends the same sequence with `is a live copy in memory`.

SO THE FINDING IS: NOT that the ports cannot boot - their driver suites pass, 271 tests here - but
that the end-to-end chain test has never been green on them. **The reason is one line further down
and is recorded in its own section below: the system volume the port machine attaches carries no
kernel, so the loader rejects it as a root.** The reading in this paragraph - that it was medium
SELECTION, the question `provider-media-order` asks - was this session's own and is retired by the
loader's own line.

AND ONE MEASUREMENT CORRECTED ITSELF BEFORE IT BECAME A CONCLUSION. The count `copy_tree` returns is
TOP-LEVEL entries, not files; two is what a healthy system volume has. Reading it as a file count
made an empty volume out of a full one for one run, until the same line was read on x86_64 - where
the test passes and the number is also two. The line says `top-level` now, which is the half of it
that was missing.

## Why the boot chain fails on both ports, in the loader's own words (2026-09-20)

The diagnosis took four log lines the loader prints about itself:

    loader: bootstrap set verified against a SIGNED etc/boot.manifest2 on the system volume
    loader: system volume found but it has no kernel; using the boot volume
    loader: bootstrap set assembled from the boot medium (the system volume did not answer)

The port runner attaches a system disk built from `system-volume-<arch>.img`, which `build.sh`'s
volume step writes WITHOUT a kernel on it - the kernel goes on the volume only when a build is asked
for it, and that is what produces `system-volume-bootable-<arch>.img`. So the loader finds a system
volume, verifies its manifest, rejects it as a root because it carries no kernel, and falls back to
the boot medium. The running system then reads a `vol://system` that holds neither `libexec/` nor
`drivers/`, which is what `no artifact at vol://system/libexec/...` and `virtio-blk is named by the
registry and not on the volume` are saying, fifteen services later.

IT IS THE SAME FAMILY AS THE COLD-SCENARIO DEFECT ON x86_64 recorded above: a guest booting a volume
that a DIFFERENT build step produces, so the volume it boots and the system it was built from are
two different things. There the runner never built it; here the runner builds the wrong one of two
artifacts whose names differ by one word.

WHAT IT IS NOT: it is not the medium-SELECTION question `provider-media-order` asks - that reading
was this session's own, and the loader's line retires it. The disk the storage service mounts is the
one the machine attaches; what is wrong is what is ON it.

## Two probes that narrow the port boot-chain failure, and the one they leave (2026-09-20)

**PROBE 1 - THE PORTS' BOOT MEDIUM NAMES NO SYSTEM VOLUME.** Running the riscv64 boot tag with the
system disk detached made the loader say it in one line: `the boot medium names no system volume;
using the first LiberFS volume found`. The x86_64 media are assembled by `mkimage`, whose
`resolve_volume_pairing` puts the volume's UUID into the signed manifest and calls a medium that
carries a volume with no pairing a BUILD ERROR. The ports' ESP is assembled by `qemu_build_esp`,
which stages and signs the volume package but names no pairing - so on those machines the root is
whichever LiberFS volume the firmware enumerates first.

**PROBE 2 - AND DETACHING THE DISK IS NOT THE FIX.** With no system disk the `no artifact at
vol://system/...` failures disappear entirely - so the live root built from the medium's volume
package does hold the system - but only FOUR of twenty-four services come online instead of nine:
the services that need a writable system volume have nowhere to put it. The two probes together say
the defect is in WHICH volume the running system treats as its root, not in what any one volume
holds.

WHAT IS LEFT IS ONE EXPERIMENT AND ONE DECISION, AND NEITHER IS A GUESS:
  - THE EXPERIMENT: `ProcessService` looks up `vol://system/libexec/...` and is refused, while other
    tests in the same suite read and write `vol://system` successfully. Those are two different
    clients of the same name. Instrumenting which volume each one resolves - the live copy from the
    medium or the attached disk - is what separates "the root selection is wrong" from "the root is
    right and the lookup is wrong", and nothing measured so far separates them.
  - THE DECISION: the loader records `ROOT_BLOCK` only in the branch where the system volume answered
    with a BOOTSTRAP SET. A volume that is selected and carries no kernel drops its bootstrap set -
    deliberately, "the halves are chosen together or not at all" - and with it the root selection,
    leaving `ROOT_NONE`. Whether a kernel-less volume should still be NAMED as this boot's root is a
    question about the boot contract, and it belongs to whoever owns that rule rather than to an
    implementer who found the machine it shows up on.

## The port boot-chain failure, measured to its last link (2026-09-20)

Four instruments were added, each answering the question the previous one left, and together they
walk the whole chain. Every one of them is a line this tree wanted anyway.

  1. **WHICH BACKING SERVES `vol://system`.** Both a live copy and a disk announce themselves while a
     boot mounts candidates, so a log carrying both says nothing about which one clients talk to.
     The answer, on the ports AND on x86_64, is the same: the boot test's own storage serves the
     DISK. That retired the "the ports choose the wrong medium" reading.
  2. **WHICH DISK.** The line carries the volume's uuid now. It is `951b8cd9d5…`, byte for byte the
     uuid `build.sh` wrote beside the image it made in that same run. The right disk, mounted, with
     the programs on it - and it is served at log line 130, a hundred and thirty-nine lines before
     anyone asks for a file, so it is not a race either.
  3. **WHAT THE VOLUME HOLDS WHEN A LOOKUP MISSES.** `ProcessService`'s "no artifact at ..." now says
     what the volume answers for the directory - and for its ROOT, because a root that lists while
     `libexec` does not is a wrong volume, and a root that is refused as well is something else.
  4. **AND WHICH REFUSAL IT IS.** `denied` would be a scope; `not found` is not.

THE ANSWER IS `[root: refused, not found] [libexec: refused, not found]`. The volume that
`ProcessService` is holding a client to does not have a root. It is not the one StorageService serves
under that name, which by then has been serving the right disk for a hundred lines. So the defect is
in the CLIENT `ProcessService` is given at bootstrap on the port profile, and in nothing downstream
of it: not the medium, not the disk, not the selection, not the timing, not the lookup.

THAT IS ONE QUESTION, IN ONE PLACE, WITH EVERYTHING AROUND IT MEASURED: which volume client the port
boot chain hands `ProcessService`, and why it is not the one that answers to `vol://system`. It is
left there deliberately - a hand-off in the boot chain is a contract between three programs, and this
session has already recorded two places where guessing at one cost a day.

## AND IT IS NOT A PORT DEFECT AT ALL: a race the x86_64 margin hides (2026-09-20)

The instruments that walked the chain also flipped it. With TWO `print` lines added where the system
volume is constructed - naming which backing serves `vol://system` - the x86_64 boot tag FAILED with
the ports' exact signature: `no artifact at vol://system/libexec/...`, ten services online, the rest
never started. Removing those two lines made it pass again in 52 s. One variable, both directions,
three runs.

So the end-to-end boot chain's hand-off of `ProcessService`'s volume client is TIMING-SENSITIVE, by a
margin two log writes can spend. The device-tree ports do not have a defect of their own here: they
are ten to twenty times slower under TCG and lose the same race every time, which is why it looked
like theirs.

WHAT THAT CHANGES ABOUT THE FINDING:
  - The instruments are gone from the tree. They answered their question and their cost is the
    defect's own trigger, which makes them the one kind of diagnostic that cannot stay.
  - What is left is one race in one hand-off, reproducible ON x86_64 by adding two prints to
    `live_volume`'s callers - which is a far better reproduction than a fifty-minute emulated suite.
  - The earlier sections above reason about mediums, pairings, kernels on volumes and root
    selections. Every measurement in them stands; the CONCLUSION each drew - that the ports choose
    or carry the wrong thing - does not. The uuid line settled that: the volume served is the one
    the build made, mounted before anyone asks for a file.

THE NEXT STEP IS NAMED AND IS ON x86_64: put those two prints back, reproduce in under a minute, and
find which of the two clients `ProcessService` ends up holding. Nothing in this needs an emulated
port or an hour.

## The last link: the volume answers `Io` (2026-09-21)

Four more probe cycles on riscv64 - each one a build and a five-minute boot tag - walked the refusal
down to a single error code. `ProcessService`'s client lists `vol://system/libexec` and the volume
answers **`Error::Io`**.

That is not any of the things the earlier sections reasoned about. The name resolves - so the client
is talking to the right volume, and the "its volume has no root" reading two sections above is
RETIRED: it came from a probe of my own that called `list("libexec")` without the `vol://` scheme,
which the path parser answers `NotFound` to before it ever looks at a volume. A probe that cannot
distinguish its own malformed argument from the answer it is hunting is not a measurement, and this
one produced a confident wrong conclusion before the source of `list_dir_name` retired it.

SO WHAT IS LEFT IS: reading the system volume's directory THROUGH ITS BLOCK PROVIDER fails with an
I/O error at the moment the services are launched, while the same volume mounted cleanly a hundred
log lines earlier - its partition table and both superblocks were read over that same provider's
request channel, which is a line the storage service prints precisely to prove the provider served.

AND THAT REFRAMES THE PRINT EXPERIMENT ONE LAST TIME. x86_64 has TWO volumes answering to
`vol://system` in that test - a memory-backed live copy and the disk - and it passes when the live
copy is the one `ProcessService` holds. Two log lines were enough to change which. So the race is
real and the Io is real, and the second is what the first exposes: a client that ends up on the
DISK-backed volume meets a provider that has stopped answering.

THE NEXT PERSON'S FIRST QUESTION IS WHICH, AND THE LOGS ALREADY SUGGEST IT: the boot test is a
lifecycle drill that stops and restarts drivers, and `virtio-blk` is among them. A volume whose block
driver went down under it would answer exactly this. That is one grep away from being certain and it
is not a guess this file will carry.

## And the cause, with the correlation measured on three architectures (2026-09-21)

One grep settled it. `DeviceManager: virtio-blk stopped answering its control path inside the deadline
its registry entry declares` - and the count of that line against the count of `no artifact at
vol://system/...` across four runs:

    run                                   control-path timeouts   services that could not load
    x86_64, boot tag, passing                        0                       0
    x86_64, boot tag, + two log lines               16                       7
    riscv64, boot tag                                4                       7
    aarch64, full suite                             10                       7

**SO THE CHAIN IS: a driver misses its control-path deadline -> DeviceManager tears it down -> the
system volume's block provider goes with it -> every read of that volume answers `Io` ->
`ProcessService` cannot load the fifteen services that live on it.** The deadline is
`heartbeat-deadline = 100` in the driver's registry entry - one second at this timer - and it is
declared per entry.

THAT IS WHY TWO LOG LINES WERE ENOUGH. They cost the boot a fraction of the same second. An emulated
port under TCG spends it by default, which is why both ports lose every time and x86_64 loses only
when something takes a slice of it away.

WHAT IS NOT DECIDED, AND IS NOT AN IMPLEMENTER'S TO DECIDE: whether a one-second control-path deadline
is right and the driver is at fault for missing it under load, or whether the deadline is a
reference-host number that an emulated target cannot meet - which is the same shape as the test
budgets in `test-kernel.sh`, each raised on a measurement and each with its own note. What IS decided
by the measurement above is that the two are one defect and not three, that it is not the ports', and
that it reproduces on x86_64 in under a minute.

EVERY PROBE FROM THIS HUNT IS OUT OF THE TREE. The two that named the backing served `vol://system`
are the defect's own trigger; the one in `ProcessService` made an RPC on a shared client in an error
path. What stays is what was right on its own terms: the storage service's refusals are named by kind,
its silent `exit()` speaks, and `machine_log` no longer drops a line when the ring is full.

## And it is not this session's bounded send (2026-09-21)

The obvious suspect was checked and cleared. This session made the supervisor's per-pass heartbeat
send bounded - `send_with_room`, one second - where it had been a wait with no end, so a send that
gave up could have looked like a driver going silent. It cannot: the caller records the two outcomes
apart, `beat.asked(now)` for a ping that went and `beat.unsendable(now)` for one that did not, and
`unsendable` pushes the due time forward by a whole deadline rather than counting against the driver.
A ping that could not be sent costs the driver nothing.

So the timeouts are what the line says: a ping WAS sent and the answer did not come back through the
supervisor's loop within the deadline. The margin is one second, and it has to cover the send, the
driver's answer, and the supervisor reading it on a pass that is also doing the boot's logging - which
is why two log lines elsewhere in the system were enough to spend it.

WHICH LEAVES THE DECISION IN A SHARPER FORM THAN "IS A SECOND ENOUGH": a heartbeat miss tears the
driver down, and when that driver is the system volume's block provider, a liveness hiccup becomes a
machine that cannot load its own services. Whether a provider the ROOT depends on may be torn down on
a heartbeat alone is a lifecycle question, and it is the one worth answering first - the deadline is
only how often it gets asked.

## The circle the teardown closes (2026-09-21)

The logs finish the story on their own. After the heartbeat misses, DeviceManager does exactly what
the lifecycle says: it restarts the drivers. Four `restarting virtio-blk`, then virtio-gpu,
virtio-snd, virtio-rng, virtio-console, xhci, nvme - and every one of them answers:

    DeviceManager: virtio-blk is named by the registry and not on the volume; trying the next candidate
    ...
    DeviceManager: 0 of 0 device(s) online

A REBIND READS THE DRIVER'S ARTIFACT OFF THE SYSTEM VOLUME - `Recovery` holds a storage client for
exactly that, and says so in its own comment. For every driver except one that is right. For the
driver that SERVES the system volume it is a circle: the provider goes down, the volume goes with it,
and the artifact needed to bring the provider back is on the volume that is gone. The restart cannot
succeed, and the machine ends with `0 of 0 device(s) online` on a disk it was reading a second
earlier.

THAT IS THE SAME SHAPE AS THE BOOT: phase one launches the boot drivers from the init PACKAGE
precisely because no volume is mounted yet. A rebind of one of those drivers needs the same source
for the same reason, and takes the other one.

SO THE DECISION AT THE TOP HAS A SECOND HALF THAT IS NOT A DECISION AT ALL. Whether a heartbeat miss
may tear down the root's provider is a lifecycle question for whoever owns it. That a boot driver
cannot be restarted from the volume it provides is not a question: it is a circularity, it is
reachable from a single missed deadline, and the bytes that would break it are the ones phase one
already used.

## The circle is cut, and measured (2026-09-21)

`Recovery` keeps the init package now, and `start_candidate` falls back to it when the volume cannot
answer for a driver's artifact. Two things had to be right and only one of them was obvious: the
volume is read by the entry's NAME and a package is keyed by its ARTIFACT - phase one uses the
second - so the first attempt looked the driver up under a key no archive has and fired zero times.

WITH THE KEY CORRECTED, ON riscv64:

    DeviceManager: virtio-blk is not readable on the volume; starting it from the init package this boot came up on   (x4)
    driver.virtio-blk: online (00:01.0) ... (00:04.0)
    DeviceManager: bind window virtio_blk = 318 / 270 / 194 tick(s) from BIND to READY

All four block drivers come back. `is named by the registry and not on the volume` fell from ten to
six, and the six that remain are the drivers the init package does NOT carry - gpu, snd, rng,
console, xhci, nvme - which is correct: it carries the boot set, and those are not in it.

x86_64's boot tag passes unchanged either side of the change (53 s, 52 s).

WHAT THE BOOT STILL DOES NOT DO, AND IT IS THE NEXT LINK RATHER THAN THIS ONE: the services were
marked `FAILED to start` while the volume was gone, and nothing starts them again once it is back.
A service whose start failed because its volume was momentarily unavailable is not the same thing as
one that failed on its own terms, and ServiceManager does not tell them apart today. That is a
lifecycle question of the same family as the one above it - and with the circle cut, it is now the
only thing between these ports and a green boot chain.

## And the last link, with the fact the decision needs (2026-09-21)

The supervisor already has the machinery: `restart_service` rebuilds a service's bootstrap and is
driven from the supervision loop for a service that came up and then died, under the restart POLICY
its manifest row declares - `transparent` or `escalate`, with a budget. What it does not cover is a
service that never came up: a failed START sets `State::Failed` with a `Reason`, and nothing asks
again.

So the question is not "should there be a retry" - there is one - but whether a start that failed
belongs under the same policy as a death, and whether it spends the same budget. That is a lifecycle
rule, it has two defensible answers, and the tree does not state one today. What this session can say
is the measurement under it: on both ports the services failed to start for ONE reason, the system
volume was briefly unreadable, the volume came back seconds later, and nothing asked again.

## `no-suppression`: twenty-two down to six, and the six are one question (2026-09-21)

The gate refuses any `allow(dead_code|unused…)` under `src/` and names the three answers it accepts:
a cfg that says where the code is reached from, a deletion, or the caller that is missing. Sixteen of
the twenty-two were answered, each verified by building all three architectures AND the test kernel:

  - SIX WERE SIMPLY STALE. `nth_offer_of`, `offer_token_of`, `send_connect`, `write_to_object`,
    `stream_round` and `stream_drain` all have callers today; the suppressions outlived whatever
    made them true.
  - TWO WERE A WRAPPER AND A HELPER WITH NO CALLER AT ALL. `loader::rollback::letter` wraps
    `Slot::letter` and every "caller" is the method it wraps; `dma_mode::mode_name` the same shape.
    Deleted, with the imports they were the last users of.
  - TWO WERE A PORT-ONLY PATH. `independent_tree` and the node name it prints are the DEVICE TREE's
    independent producer, which x86_64 does not have - its own comment said so in prose and now says
    it in `#[cfg(not(target_arch = "x86_64"))]`.
  - THREE WERE A RE-EXPORT A TEST BUILD DOES NOT NAME. The hot-plug half of each backend's PCI
    surface is read by handlers that are `not(test)`; the re-export carries the same condition now,
    split from the half every build does name.
  - TWO WERE THE HOST-TEST SEAM. The allocator hook's body and the whole heap module are not
    registered there; both carry `#[cfg(not(feature = "host-tests"))]` instead of an allow.
  - ONE WAS A VERSION NUMBER NOBODY CHECKED. `release-required.toml` carries `schema = 1`, parsed and
    never read. It is compared against what this build understands now, so a list written by a later
    writer is refused rather than read under rules it was not written to.
  - AND ONE WAS A COMMENT THAT WAS WRONG. `sdhci`'s `Dma` kept a `handle` field "because the handle
    is what holds the region" - but dropping that struct gives nothing back and nothing closes the
    handle, so the field was documentation pretending to be a mechanism. Removed, with the truth
    written where the region is created.

THE SIX THAT REMAIN ARE ONE QUESTION AND ARE LEFT DELIBERATELY. `hot_plug_ports` and
`slot_interrupt_line` exist on all three backends and in the shared PCI module, and only x86_64 has a
caller: arming a slot's interrupt routes a legacy line through an I/O APIC. Removing the allow makes
the whole chain dead on both ports - measured, not assumed - so the three answers the gate accepts
are: cfg the pair to x86_64 (which says the HAL surface is NOT the same shape on the three ports,
against what its own comment states it is for), give the ports a caller (a feature), or delete the
shims (the same statement as the first). That is a HAL-contract decision, it is one decision covering
all six, and it is the only thing between this gate and green.

## `bootstrap-plan` is red over one manifest row, and the remaining half is a decision (2026-09-21)

The gate compares the manifest's `restart = "transparent"` set against what `relaunch_service` can
actually re-run, and the two disagree by exactly one name:

    font_catalogue: the manifest says `transparent` but relaunch_service cannot re-run its bootstrap
    ladder ['config_service', 'device_service', 'system_graph_service']
    plan   ['config_service', 'device_service', 'font_catalogue', 'system_graph_service']

WHAT `transparent` MEANS HERE, from the supervisor's own definition: the service is restarted per
the ladder AND ITS CLIENTS RE-RESOLVE THROUGH THE BROKER. Both halves, and the second is what makes
it transparent rather than merely restartable. The manifest row argues the first half correctly -
the catalogue holds nothing but what the font directory says, so a fresh instance is in the same
place - but that is an argument about `state_class`, not about clients.

MOST OF THE SECOND HALF IS ALREADY THERE, which is worth knowing before anyone reads this as a large
job. PermissionManager already re-resolves its ordinary font connection: `Capability::FontCatalogue
=> (&mut clients.font, CAP_FONT)` goes through `connect_or_resolve(held, clients.broker, name)`,
which is the same path `config` and `device` take. What is missing on that side is four small
joins - a `font` root on `Broker`, `CAP_FONT` in `permission_manager`'s grant row, `CAP_FONT` in
`service_of_cap`, and the root in `serve_resolve`'s match - plus a `font_catalogue` arm in
`relaunch_service` whose bootstrap sends FONTDIR and ADMIN before SERVE, because that program's
bootstrap is positional.

AND THE PART THAT IS NOT AN IMPLEMENTATION IS THE ADMIN FACE. `CAP_FONTADMIN` is `take_end_of` at
boot - the admin root is MOVED to PermissionManager, which grants it to `lsfont` - and there is no
re-resolution for it. After a restart that handle names an instance that has ended, and nothing says
so. Making it survive means letting an ADMIN root be re-resolved through the broker, and that is a
statement about where administrative authority may be handed out from rather than a missing branch.
So the two honest answers are:

  a. Resolve the admin face through the broker as well, and say in the grant set why an admin root
     may travel that way. The rest of the work is the five joins above.
  b. Leave the admin face outside the restart, and have the manifest say what is true: the
     ordinary face is transparent and the admin face is not - which today's two-value `restart`
     column cannot express, so it would need a third state or a second column.

NEITHER IS TAKEN HERE. Both are defensible, the choice is about authority rather than about
mechanism, and the gate is red in the meantime - which is the correct state for a claim the system
cannot yet honour.

AND ONE THING THE GATE'S OWNERS SHOULD SEE: the two milestones that own `bootstrap-plan` are both
marked complete in the index and neither carries an open item, while the gate they own is failing.
The `milestone-index` gate checks the other direction - a `[x]` over a document with open tasks -
and there is no check that a completed milestone's gate still passes.

## The kernel's input ring kept bytes for a console that had not attached yet

FOUND BY THE FULL aarch64 SUITE, and it is a real defect rather than a test artefact. The terminal
test `kernel.services.a_tty_a_job_left_raw_comes_back_cooked` types `ab\n` into a fresh
ConsoleService and asserts the cooked line comes back whole. In the full suite it was handed
`xab\n`. It passes alone on aarch64 (99s) and it passes on x86_64, which is why it had never been
seen: the `x` comes from a different suite entirely.

WHERE THE `x` CAME FROM. `kernel.object.the_console_and_display_syscalls_refuse_a_caller_without_the_capability`
types one `x` through `SYS_CONSOLE_FEED` to prove that the capability check lets a properly
privileged caller past the gate. Its own comment says the byte is harmless because "with no console
attached it answers WOULD_BLOCK" - and that was not what the code did.

`console_input::enqueue` pushed every byte into the pending ring whatever the console's state, then
called `drain`, which returns at its first line when nothing is registered. So the byte sat in the
ring. Two hundred tests later the terminal test attached a console, and the ring was emptied into it
oldest-first, which put a character in front of the line the test had typed.

The second half is the same shape and was found reading the first: a registration whose peer has
gone is never removed, so `drain` reaches `channel.send`, the send fails, and the comment there says
the byte "STAYS at the front either way". It stays there until the next attach, and then it is
delivered to the next console. Every boot test that installs a stand-in console and drops its end
leaves the input path in exactly that state.

WHAT IT WOULD HAVE COST IN PRODUCTION, not in the suite: after a SystemManager crash the recovery
console's first line begins with whatever the previous shell had not read. That is input crossing a
session boundary - a keystroke is typed AT something, and what it was typed at is the console that
was attached when it arrived.

THE FIX, in `src/kernel/console_input.rs`, is two lines of policy and one shared question:

  - `listener()` answers "is there a console that could take a byte right now", which is registered
    AND with its peer alive. `shell_listening`, `enqueue` and `drain` all ask it, so the three
    cannot drift apart.
  - `enqueue` refuses a byte when there is no listener instead of keeping it. This restores the
    return value the function has always documented - "false if no shell is attached or its endpoint
    has closed" - which is the condition `supervise` reads to end its boot round, and which
    `sys_console_feed` turns into `ERR_WOULD_BLOCK`. The object test's comment is now true.
  - `attach` clears the ring. What the previous console never took is not this one's input.

NOTHING RELIED ON THE OLD BEHAVIOUR. Type-ahead before a console exists is not a feature anything
uses: `guest-console.py` waits for `shell attached` before it types, the kernel's own first-prompt
nudge is fed only once `shell_listening()` is true, and `supervise` reads the serial line only while
listening. The pump at `main.rs` drained the UART unconditionally, and those bytes are now dropped
at the door, which is what a machine with no terminal does with a keystroke.

PINNED BY `kernel.console.input_that_arrived_with_no_console_is_not_the_next_console_s_input`, which
asserts both halves - a byte fed with nothing registered and a byte fed to a dead registration - and
then that a freshly attached console is handed its own first byte and nothing else. It leaves no
console behind, by the rule it is asserting.

## The emulated suites' wall clocks had never been met by a run that finished

FOUND IMMEDIATELY AFTER THE FIX ABOVE, and it is the reason that fix looked at first like a
regression. With the terminal test repaired, the aarch64 suite ran past the point where it used to
stop and ended at the wall instead: TIMEOUT after 80m, 4811 s in, still inside
`xhci_driver_enumerates_the_usb_bus`.

THE RUN WAS NOT SLOWER. Measured test by test against the previous run, the same tests to the same
point cost 3620 s before and 3745 s after - 3.5 %, which is ordinary machine noise. What changed is
that the previous run STOPPED EARLY: it failed at the terminal test with sixty-three tests still to
go, and a suite that ends at test 394 fits inside a wall that a suite of 457 does not.

So 80m had never been met by an aarch64 run that reached the last test. Both kinds of run that
fitted inside it had given up first - one at a failing assertion, one at the wall - and a budget
nothing has ever completed under is not a measured budget.

MEASURED, both emulated suites run beside each other on an otherwise quiet host:

    aarch64   427 tests   5065 s   (old wall 4800 s)
    riscv64   430 tests   6005 s   (old wall 5400 s)
    x86_64    439 tests    271 s   under KVM, for scale

Set from those: aarch64 120m (measurement x 1.42), riscv64 140m (x 1.40). Both clear the file's own
rule of a fifth over the measurement with room for the next several tests rather than exactly one.
Taking the figures under concurrency is the pessimistic side to be wrong on - a solo run is cheaper
than these numbers say, so the headroom is real.

AND THE CHECK THAT EXISTS FOR THIS DID NOT CATCH IT. `verify-model check` compares each
`FULL_TIMEOUT` against a modelled full-suite cost. It called riscv64 short by twenty-four seconds
(5400 against a required 5424) and said nothing about aarch64 - because its estimates, 4520 s for
riscv64 and less for aarch64, are well under what the suites actually cost. The model is a
regression over history; when it and a stopwatch disagree, the run that reached the last test wins.
The slowest single test is 1309 s on aarch64 and 1057 s on riscv64, both comfortably inside the
2400 s per-test stall window, so nothing there needed moving.

## The CDC-ACM oracle: bytes go out of the machine and come back

THE ITEM'S REMAINING HALF WAS "A DECISION RATHER THAN AN AFTERNOON", AND THE DECISION WAS BASED ON
SOMETHING THAT WAS NOT TRUE. The note said a kernel test could not drive `console-stream` because it
is a generated IDL and "the kernel links `driver-protocol` and four `*-proto` crates but not
`proto`". `console-stream` is generated into `device-proto`, which is line 48 of the kernel's
`Cargo.toml` and has been for as long as the audio suites have needed a provider catalogue. There
was no dependency to add.

NOR IS THE CLIENT BEHIND THE FEATURE GATE. `channel-client-impl` guards the CHANNEL TRANSPORT -
three items calling the userspace runtime's syscalls - and not the generic `Client<T>`, whose
`attach`, `write` and `receive` encode and decode through the generated codec for any `T`
implementing `wire::Transport`. That trait's own comment says what was missing and that it was
expected: "the userspace impl sends on a channel and blocks for the reply; TESTS USE AN IN-MEMORY
LOOPBACK".

SO THE ORACLE IS WRITTEN AND IT PASSES: `usb-cdc-acm: 9 byte(s) out and the same 9 back`. The bytes
go from the test, through `console-stream`, through the driver's bulk OUT endpoint, out of the
machine through `usb-host` into `dummy_hcd`, into the gadget's tty, into `serial-echo.py`, and all
the way back. It is the second oracle in this tree whose peer is outside the machine.

WHAT THE TEST IS MADE OF:

  - `bind_xhci_controller`, EXTRACTED rather than copied. Finding the controller, minting its MMIO
    and MSI-X, claiming the device and sending the four-resource `BIND` is sixty lines that say
    nothing about what either oracle is for, and a second copy is a second place for the handshake
    to drift when a resource is added. The existing enumeration test uses it and still passes.
  - A `Transport` over a kernel channel, which is what the trait asked for. It also NUMBERS THE
    CAPABILITIES A REPLY CARRIES, because `receive`'s reply hands over the inbound stream and the
    decoder refuses a reply that does not carry exactly one handle: a handle is an index into
    something, and in a kernel test the transport is the table.
  - `USB_GADGET` PASSED AT COMPILE TIME, like `TEST_TAGS` and `LIBER_NO_DT_PROFILE`. Without a
    gadget there is no device, and the test says so in the log and returns rather than passing
    quietly - a run that says nothing is how a test that stopped testing anything goes unnoticed.
  - A WALL-CLOCK WAIT AND NOT AN ITERATION COUNT. `run_until_idle` returns the moment the run queue
    is empty, so a loop counted in passes spins twenty thousand times in a few milliseconds and
    gives up long before a process on the HOST has read a tty and written back. Every other driver
    oracle here PULLS and is paced by its own round trip; `console-stream` PUSHES its inbound
    frames, so the budget has to be the clock.

AND THE DEFECT THAT ACTUALLY STOPPED IT WAS ON THE HOST, FOUND BY MEASURING RATHER THAN BY READING.
The guest's write reported nine bytes taken and `transmit` is SYNCHRONOUS - it rings the doorbell
and waits for the completion - so the controller had accepted the transfer. The host echoed nothing.
The bisection: `/dev/ttyACM0` exists beside `/dev/ttyGS0` because `dummy_hcd` gives this machine BOTH
ends of the cable and Linux's own `cdc_acm` binds the host side; writing to one and reading the other
proved the cable carries bytes; and watching `/dev/ttyACM0` during a run showed it DISAPPEARING the
moment QEMU starts, which is `usb-host` detaching the kernel driver exactly as it should.

So the passthrough was right and the ECHO PROCESS WAS HOLDING THE WRONG DESCRIPTOR. The harness
starts it beside the gadget, which is before QEMU - so it opened the port while LINUX was still the
host, and a few seconds later the function was torn down and brought back up under a new one. A
descriptor held across that sees nothing afterwards. `serial-echo.py` now REOPENS the port while
nothing has arrived and keeps it once something has, and it says on stderr what it echoed as it
happens rather than only in a `finally` a killed process never reaches - which is what told the two
halves apart in the first place.

## And the oracle found a driver defect, which is what an oracle is for

IT PASSED ONCE AND THEN FAILED THREE TIMES IN A ROW, with the host reporting `echoed 9 byte(s)`
every time. So the bytes left the machine, the host echoed them, and the guest saw none - which is a
different failure from the one the item had been waiting on and a much better one to have found.

THE FIRST HALF IS IN `wait_transfer_len`, AND THE SHAPE WAS ALREADY IN THE FILE. That function is
the synchronous wait a control or bulk transfer does: it drains the event ring until the completion
it wants appears. It has an exception that KEEPS a completion belonging to the NETWORK adapter's
receive endpoint, and the comment beside `Xhci::net_rx` says exactly why:

    a frame that arrived during a disk read was dropped AND its standing receive transfer was never
    re-posted, which stops the link for good rather than losing one packet

THE SERIAL ADAPTER HAD NO SUCH EXCEPTION, so its completion fell through to `handle_hid_event` and
was discarded - and `Serial::posted` stayed true, so nothing re-posted either. One lost chunk was a
stream that never carried another.

AND THE TIMING IS NOT A RARE INTERLEAVING, IT IS THE ORDINARY ONE. `transmit` rings the doorbell and
WAITS for its own completion; the echo of a nine-byte write on a loopback is already coming back
while that wait is running. So the serial receive's completion lands inside `wait_transfer_len`
almost every time, which is why the failure was three in four rather than one in a hundred.

FIXED THE SAME WAY THE NETWORK PATH WAS: `serial_rx` records the endpoint when the adapter binds,
`serial_pending` keeps one completion, the service loop drains it before anything still on the ring,
and both are cleared when the adapter goes away - because a stashed completion for a device that has
gone names a slot the next one will be given, and draining it then would deliver one adapter's bytes
on another's stream.

THE SECOND HALF WAS THE TEST'S OWN, AND IT IS A PROPERTY OF THE CONTRACT RATHER THAN A BUG IN IT.
`Session::deliver` DROPS a chunk when nothing holds the stream, which is the right answer: a stream
nobody opened has no consumer, and buffering for one that may never arrive is how a driver grows an
unbounded queue. The consequence is an ORDER - a consumer opens its inbound stream BEFORE it writes -
and the oracle had it the other way round, so the echo arrived in the window between the write and
the `receive`. On a link this fast that window is where the echo always is.

THE ORACLE NOW READS: attach, receive, write, read. Four consecutive runs, four passes, where the
same test with the same driver was one in four before either half was fixed.

## And a third thing the oracle found, which was in this file rather than in a driver

`the xHCI controller is taken, as DeviceManager takes it: AlreadyClaimed`.

THE CLAIM OUTLIVES THE DRIVER PROCESS, AND THIS FILE SAID SO WITHOUT NOTICING WHAT IT MEANT. The
enumeration oracle's own comment reads "Bus mastering goes off when the driver process dies and the
transferred capability dies with it; the CLAIM ends with this test kernel" - which was true while
exactly one test ever took this controller. The moment a second one does, it is answered
`AlreadyClaimed` and cannot bind at all: the first test's claim is still current, and terminating
its driver does not give the device back.

So both oracles now end with `driver.terminate()` and then `device::release_claim(key)`, and THE
ORDER IS NOT A PREFERENCE. Releasing turns off bus mastering and MSI-X on the function, so a driver
still running would be one whose device had stopped answering under it; taking the process away
first makes the release the end of something rather than the middle of it.

WORTH SAYING PLAINLY: this was not a defect the suite could have reported before, because nothing
had ever asked for the controller twice. Adding a second oracle on one device is what turned a
sentence in a comment into a failure, and the sentence is now a release.

## The development profile cannot be staged on this machine, and the reason is the toolchain

THE PCIe HOT-PLUG ITEM'S LAST CLAIM IS A COLD SCENARIO ON THE EMULATED PORTS, and the final long
pass is where it was to be made. It cannot be, and what stops it is not the code.

`lab.sh scenario-cold` builds with the DEVELOPMENT profile, which it must: it drives the guest over
the same control protocol the persistent instance uses, and an ordinary build stages none of that.
The development profile also stages two QUARANTINE artifacts, `abiprobe` and `vkprobe`, and both
carry an identity record naming the ICD candidate they were linked against:

    build-shared: vkprobe admits icdprobe at 02c8a011..., and the staged icdprobe is 4c080817...

BISECTED RATHER THAN GUESSED AT, and the first two answers were both wrong:

  1. "A stale staged tree", which is what the message's own advice says - "clear it and build again".
     Cleared and rebuilt on aarch64: SAME FAILURE, same two digests.
  2. "Arch-specific, because x86_64 has been building all session". Ran the same scenario on x86_64:
     SAME FAILURE, with x86_64's own pair of digests. The ordinary profile passes there because it
     does not stage these two at all, which is why nothing had noticed.
  3. Cleared `.build/image/x86_64-unknown-none` entirely and built it in ONE development-profile
     pass: SAME FAILURE. So it is not a half-built tree either.

WHAT THE DIGEST ACTUALLY COVERS IS THE ANSWER. `staged_identity_digest` hashes the descriptor of
`.note.liber.identity`, and that record names the TOOLCHAIN:

    compiler=Debian clang version 19.1.7 (3+b1)
    compiler-sha256=0a3c5593...
    linker=LLD 21.1.8 (...)
    archiver=Debian LLVM version 19.1.7

So `icdprobe`'s identity moves when the host's clang, lld or llvm-ar does. `abiprobe` and `vkprobe`
are `producer = "audit"` - they are NOT built from this tree, they are artifacts produced elsewhere
and staged - and they record the digest of the icdprobe THAT machine built. This machine's clang is
not that machine's clang, so the two can never agree, and nothing in this tree can make them: the
consumers cannot be rebuilt here.

IT IS NOT MINE AND THE CHECK IS RIGHT. `icdprobe.c` is C and untouched by this session's work; the
mechanism is the toolchain in the note, which no source change of mine reaches. And the check is
doing exactly what it exists for - a consumer admitting a candidate it was not built against is a
selection that would resolve to something the consumer never saw.

SO THE ITEM'S LAST CLAIM IS BLOCKED ON REPRODUCING THE AUDIT ARTIFACTS AGAINST THIS TOOLCHAIN, which
is a thing the audit environment does and this tree cannot. Recorded here rather than worked around:
the alternative would be to weaken the check or to stage a consumer whose selection record is a lie,
and both are worse than an item that stays open with its reason written down.

## The long pass, finished

    port            full suite      extended 3D      CDC-ACM oracle
    x86_64 (KVM)    440 passed      60/60            1 passed (22 s)
    aarch64 (TCG)   428 passed      60/60            1 passed (1674 s)
    riscv64 (TCG)   431 passed      60/60            1 passed (1248 s)

Zero failures anywhere. `scene3d-extended 60 passed, 0 failed, 0 unsupported, 0 untested` and `and
carries Scene3D Extended Profile 1` on all three, which is the half a host fixture cannot give: the
equations are exact and their expected values come from the profile, so a guest run is what says the
ARITHMETIC AGREES THERE rather than only on the machine that built the tree.

AND riscv64 TOOK 7017 s, which is over the 5400 s budget it had at the start of this pass. Raising
it to 140m from a measurement rather than a guess is what kept this run from ending at the wall -
and a run that ends at the wall reports a TIMEOUT, which reads exactly like a hang.

THE CDC-ACM ORACLE RAN ONE PORT AT A TIME BY NECESSITY. A machine has one USB gadget, and a second
run setting one up beside the first takes the device from it; the oracle REQUIRES its device when
`USB_GADGET=acm` is compiled in, so a port that lost the gadget fails rather than skips. That is the
right way round, and it is why the three runs are sequential rather than the two emulated ones being
run beside each other the way their full suites were.
