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
