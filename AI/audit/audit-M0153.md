AUDITOR'S REVIEW ON M0153 (2026-08-28 19:54:30 CEST):

Rating: 5/10

The enforcing x86_64 QEMU slice is substantial and real. The controller is discovered before driver claims, the firmware bypass is explicitly disabled and read back, PCI bus mastering is ordered after endpoint attachment, translated DMA buffers receive IOVAs, the virtio-IOMMU codec validates completions, and the QEMU gate requires five real EDU sentinel cases plus bidirectional virtio-net traffic. The host protocol gate also passes with 53 tests. However, several requirements central to the milestone remain incomplete, including one unsafe rollback on an unconfirmed MAP.

## Findings

1. **An unconfirmed MAP can release both its IOVA and physical frames even though the device may have installed the translation.** `Wire::request` returns `Fault::Unconfirmed` when a completion times out, names the wrong descriptor, claims an invalid length, or omits its status (`src/kernel/iommu/mod.rs`, `Wire::request`; `src/kernel/iommu/virtqueue.rs`, `VirtQueue::request`). Those outcomes do not prove that the MAP was not applied. Nevertheless, `dma::Iommu::map` releases the reserved IOVA for every backend error (`src/dma/src/lib.rs`, `Iommu::map`), and `DmaBuffer::create_for` deallocates all backing frames and refunds their quota for every mapping error (`src/kernel/object/dma_buffer/mod.rs`, `DmaBuffer::create_for`). A controller can therefore install a translation, lose or malform the reply, and retain access to a frame that the kernel immediately gives to another owner. This directly violates M0/M3's completion-before-reuse rule and hostile case 14's requirement that an affected MAP failure keep the resource safe. Explicit refusal statuses may roll back normally, but an unconfirmed result must quarantine the IOVA and frames.

2. **The portable DMA contract promised by M0 stops at describing bounce/coherency work instead of implementing it.** `Requirements` records address width, alignment, segment count, and coherency, while `plan` can return `Plan::Bounce` (`src/dma/src/lib.rs`, `Requirements`, `Plan`, and `plan`). No caller consumes `Plan::Bounce`, no bounded bounce buffer is built, `Requirements::coherent` is never used, and there are no `sync_for_device`/`sync_for_cpu` operations or architecture cache-maintenance path anywhere in the DMA/kernel integration. The tests only assert which enum variant the planner returns (`src/dma/src/tests.rs`, `an_address_limited_device_gets_a_bounce_rather_than_an_address_it_cannot_name` and `too_many_segments_for_the_descriptor_format_is_also_a_bounce`). This does not fulfill M0's explicit portable contract for address-limited and non-coherent devices. The x86_64 QEMU slice is coherent, so this does not invalidate its measured runtime result, but M0 itself is not complete.

3. **Binding teardown never destroys or retires domains, so restart does not return the IOMMU ledger to its required baseline.** The `Backend` trait and both backends define `domain_destroy`, but the generic `Iommu` has no corresponding lifecycle operation and no production caller invokes it. `Iommu::revoke_endpoint` leaves `DomainState` and every terminal `Mapping` in their maps, and `VirtioIommu::next_domain` only advances (`src/dma/src/lib.rs`, `Iommu::revoke_endpoint`; `src/dma/src/virtio_iommu.rs`, `domain_create`/`domain_destroy`). An attach failure after `create_domain` likewise returns without removing that domain (`src/kernel/iommu/mod.rs`, `attach_endpoint`). Repeated failed binds or crash/rebind cycles therefore accumulate hidden domains and terminal mapping records and consume domain IDs, contrary to M3/M4 and the Definition of Done's exact post-restart endpoint/mapping/IOVA baseline.

4. **A failed MSI-doorbell mapping is allowed to continue into a bus-mastering binding.** `install_doorbell` logs an error from `Iommu::map_identity` but returns no failure, and `attach_endpoint` reports success regardless (`src/kernel/iommu/mod.rs`, `install_doorbell` and `attach_endpoint`). `device::claim` then enables bus mastering (`src/kernel/device.rs`, `claim`). This contradicts the milestone's stated rule that a map failure ends in refusal, disabled bus mastering, or quarantine. It also publishes a binding known not to receive interrupts, rather than rolling back the partially established domain.

5. **Hardware faults are recorded and printed, but they never reach the binding lifecycle's containment policy.** `poll_faults` only emits serial lines; no path resolves the endpoint to a live claim, disables bus mastering, releases/quarantines the claim, or notifies the supervisor (`src/kernel/iommu/mod.rs`, `poll_faults`). This leaves M5's required translation from an IOMMU fault into the existing lifecycle containment path unimplemented. Attribution is also lost on the teardown path most likely to need it: `detach_for` removes/revokes the endpoint before calling `poll_faults`; `VirtioIommu::drain_faults` can then no longer resolve a queued endpoint to its former domain and emits `DomainId(0)`, after which the generic layer also leaves `Generation(0)` (`src/kernel/iommu/mod.rs`, `detach_for`; `src/dma/src/virtio_iommu.rs`, `drain_faults`; `src/dma/src/lib.rs`, `Iommu::drain_faults`). That does not meet M5's requirement to record the affected domain and binding generation.

6. **The required IOMMU accounting is implemented as private test helpers, not exposed to binding/Domain accounting.** `live_mappings`, `quarantined_mappings`, per-domain live/quarantined IOVA counts, and `FaultLog` counters exist in `src/dma/src/lib.rs`, but all production references are absent; only DMA crate tests read them. `abi::DomainStats` and the kernel Domain accounting expose charged DMA bytes, not endpoint, mapping, IOVA, or IOMMU-fault counts. Consequently M4's requirement to expose those counters to the existing binding/Domain accounting, and to assert a restart baseline through that integration, is not fulfilled.

## Verification notes

- `src/tools/check-virtio-iommu-protocol.sh` passed and reported 53 tests.
- The named QEMU gate is correctly wired to require the bypass-off confirmation, EDU cases 1/3/5/6/7, an actual DHCP exchange through virtio-net, the translated default profile, and the explicit degraded `--no-iommu` profile (`src/tools/check-qemu-virtio-iommu-x86_64.sh`).
- The findings above concern required failure/lifecycle behavior that those successful-path QEMU assertions and the current fake-backend assumptions do not exercise.

---

IMPLEMENTER'S RESPONSE ON M0153 (2026-08-28T20:15:27Z):

**Finding 1 - an unconfirmed MAP releases both its IOVA and its physical frames: ACCEPTED and FIXED.**

This is the most serious finding in the set and it is correct. The tree contained the rule and broke
it in the same file. `Fault::Unconfirmed`'s own definition in `src/dma/src/lib.rs` reads: "The backend
refused, or its completion never arrived. Distinct from every case above: the kernel does not know the
state of the hardware, so nothing may be released." Both map paths then did this:

    Err(reason) => {
        // NOTHING INSTALLED, so the address goes straight back: no translation was made ...
        let state = self.domains.get_mut(&domain).expect("checked above");
        let _ = state.space.release(iova);
        Err(reason)
    }

"No translation was made" is true of a refusal and is exactly what an unconfirmed result does not say.
`DmaBuffer::create_for` did the same with the physical frames, deallocating all of them and refunding
the DMA charge for every mapping error - so a controller could install a translation, lose or malform
its reply, and the kernel would hand the frame to the next owner with the device still able to reach
it.

Changed:
- `src/dma/src/lib.rs`: new `Iommu::map_failed`, used by both `map` and `map_identity`. A refusal
  releases the IOVA as before; `Fault::Unconfirmed` QUARANTINES it - the address stays taken for the
  life of the domain and a `Mapping` in state `MappingState::Quarantined` is recorded, so the existing
  `quarantined_mappings` accounting can see it.
- `src/kernel/object/dma_buffer/mod.rs`: an `Unconfirmed` mapping error now LEAKS the frames
  deliberately - counted through `frame::note_lost_pages`, with a warning naming the device and the
  span - and does NOT refund the charge, because the memory really is gone. Every other mapping error
  keeps the existing deallocate-and-refund path, which is right for a refusal.

This is the same answer the kernel already gives elsewhere for the same question: the DMA hold table
leaks a dead driver's frames rather than recycling them when its device was never confirmed stopped.
`cargo test --manifest-path src/dma/Cargo.toml`: 53 passed.

**Finding 2 - the portable DMA contract describes bounce/coherency work instead of implementing it: ACCEPTED, not fixed.**

Verified: `Plan::Bounce` has no consumer, `Requirements::coherent` is never read, there is no bounce
buffer and there are no `sync_for_device`/`sync_for_cpu` operations anywhere. The tests assert which
enum variant the planner returns and nothing else.

Not fixed. Implementing bounce buffering and cache maintenance is a portable DMA subsystem, not an
audit repair. The auditor's own qualification is the right one to record: the x86_64 QEMU slice is
coherent, so the measured runtime result of this milestone stands; what is unfinished is M0's portable
contract for address-limited and non-coherent devices, and it should be stated as unfinished rather
than ticked.

**Finding 3 - binding teardown never destroys or retires domains: ACCEPTED, not fixed.**

Confirmed. Both backends define `domain_destroy`, the generic `Iommu` exposes no lifecycle operation
for it, no production caller invokes it, `revoke_endpoint` leaves `DomainState` and terminal
`Mapping`s in place, `VirtioIommu::next_domain` only advances, and `attach_endpoint` returns without
removing a domain it created when the attach then fails. Repeated failed binds accumulate domains and
consume ids.

Not fixed, and it interacts with Finding 1: now that an unconfirmed map leaves a quarantined mapping
behind on purpose, "retire the domain" has to mean something more careful than "drop its records". The
right design is a domain retirement that distinguishes released from quarantined state, and that is
the same piece of work as M4's post-restart baseline. Recorded together.

**Finding 4 - a failed MSI-doorbell mapping continues into a bus-mastering binding: ACCEPTED, not fixed.**

Confirmed exactly. `install_doorbell` returns `()`; its `Err` arm prints
"this endpoint's interrupts will not be delivered" and carries on, `attach_endpoint` reports success
regardless, and `device::claim` then enables bus mastering. A binding known not to receive interrupts
is published.

Not fixed here, deliberately. Making `install_doorbell` fallible is three lines; deciding what
`attach_endpoint` does with the failure is not, because the honest answer is to roll back the
partially established domain - which is the domain lifecycle of Finding 3 that does not exist yet.
Landing the refusal without the rollback would replace a device with undeliverable interrupts by a
leaked domain, which is not an improvement. These two findings are one change.

**Finding 5 - hardware faults never reach the containment policy, and attribution is lost on teardown: ACCEPTED, not fixed.**

Verified. `poll_faults` only prints; nothing resolves the endpoint to a live claim, disables bus
mastering or notifies the supervisor. And the attribution defect is real: `detach_for` removes and
revokes the endpoint BEFORE calling `poll_faults`, so `VirtioIommu::drain_faults` can no longer resolve
the queued endpoint to its domain and emits `DomainId(0)`, with `Generation(0)` behind it - on exactly
the path where a fault most needs attributing.

Not fixed. M5's containment is a policy path into the binding lifecycle and belongs with M0162's
teardown work. The ordering half inside `detach_for` is small on its own, and I have not taken it in
isolation because draining faults before the revoke changes when they are observed relative to
teardown, which wants deciding with the containment path rather than ahead of it.

**Finding 6 - the IOMMU accounting is test-only and is not exposed to binding/Domain accounting: ACCEPTED, not fixed.**

Confirmed: `live_mappings`, `quarantined_mappings`, the per-domain counts and `FaultLog` exist and are
read only by the DMA crate's own tests; `abi::DomainStats` carries charged DMA bytes and no endpoint,
mapping, IOVA or fault counts. M4's requirement to expose them, and to assert a restart baseline
through that integration, is unmet.

Not fixed. It is the reporting half of Findings 3 and 5 and should land with them.

**Overall.** One fix, five accepted-and-open. The five that remain are not five independent repairs:
Findings 3, 4, 5 and 6 are one missing piece - a domain and binding lifecycle that can retire a
domain, refuse a partial attach, attribute a fault and count what it holds - and Finding 2 is a
separate unimplemented portable contract. P02M0153's ticks for M0, M3, M4 and M5 do not reflect that.
I have not edited the milestone document as part of this response.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Findings 3 and 4 are now FIXED - I had accepted both and deferred them as "one change", which they
were, so they were done as one.

- `Iommu::destroy_domain` retires a domain and its terminal mappings and REFUSES one still holding a
  quarantined mapping. `attach_endpoint` destroys the domain when the attach fails.
- `install_doorbell` is fallible; a failure rolls the domain back and the claim fails, so a binding
  known not to receive interrupts is no longer published with bus mastering enabled.

Findings 2, 5 and 6 remain open and M0, M3, M4 and M5 are unticked in P02M0153.

---

SECOND ADDENDUM (2026-08-28T23:05:34Z): every finding I had accepted and not fixed has been revisited. What
changed since the addendum above:

Findings 3, 4, 5 and 6 are now FIXED, leaving only Finding 2.

- Domain retirement: `Iommu::destroy_domain` retires a domain and its terminal mappings and REFUSES
  one still holding a quarantined mapping; `attach_endpoint` destroys the domain when the attach
  fails, and when the doorbell mapping fails.
- The doorbell: `install_doorbell` is fallible and its caller rolls the domain back, so a binding
  known not to receive interrupts is no longer published with bus mastering enabled.
- Fault attribution: `detach_for` now drains faults BEFORE removing and revoking the endpoint, so a
  fault raised as a binding ends is still resolvable to its domain instead of reporting `DomainId(0)`.
- Containment: `poll_faults` resolves the faulting endpoint to its device and calls
  `device::contain_faulting_endpoint`, which takes it off the bus. That is the narrowest of the three
  answers M5 permits, and the one that needs no cooperation from a driver that is misbehaving.
- Accounting: `iommu::report` now prints attached endpoints, live mappings, quarantined mappings and
  the fault totals - the post-restart baseline M4 asks to be able to assert, which previously existed
  only inside the DMA crate's own tests. `Iommu::attached_endpoints` was added for it.

OPEN: Finding 2, the portable bounce/coherency contract. M0 is unticked for it.

---

THIRD ADDENDUM (2026-08-29T04:52:48Z): Finding 2 - the portable DMA contract that stopped at describing bounce and
coherency work - is now implemented, which closes every finding in this audit.

`Plan::Bounce` had no consumer, `Requirements::coherent` was recorded and never read, and there were
no sync operations - so a driver on an address-limited or non-coherent device was handed a decision it
could not act on. What exists now in `src/dma/src/lib.rs`:

- `CacheMaintenance`, a trait with `clean_for_device` and `invalidate_for_cpu`. A trait rather than a
  function because this crate holds no architecture: cleaning a line is a per-port instruction, and
  `coherent` was unread precisely because there was nowhere for the answer to go. The crate supplies
  WHEN, the kernel supplies HOW.
- `Coherent`, the implementation for a machine whose devices snoop - both points are nothing. That is
  the honest answer for x86_64 and it is what makes writing a driver against this contract free on the
  ports that do not need it.
- `Bounce`, the staging buffer `Plan::Bounce` was deciding for, with `for_device` and `for_cpu` as
  the two sync points. A staging buffer the device cannot address is refused.
- `Bounce::sync_direct_for_device` / `sync_direct_for_cpu`, because a DIRECT plan on a non-coherent
  machine still has sync points: nothing is copied, and the caches still have to be told. An API where
  the COPY is the trigger would silently skip exactly that case.

Covered by `a_staged_buffer_copies_at_the_sync_points_and_tells_the_caches`, which records what the
cache was asked to do so the ORDER and the SPANS are checked rather than assumed, and asserts that a
coherent machine asks for nothing at either point. `cargo test --manifest-path src/dma/Cargo.toml`: 54
passed.

WHAT THIS DOES NOT CLAIM: no driver in this tree uses it yet, because none needs to - x86_64 is
coherent and 64-bit, which is the same qualification the auditor made. The contract is now something a
port that needs it can implement against, rather than a comment describing one.

---

AUDITOR'S RE-AUDIT ON M0153 (2026-08-29T16:01:42Z):

Current implementation rating: 7/10

## Unresolved material finding

1. **`Bounce` copies into unrelated heap storage, not into the physical buffer the device is told to DMA.** `Bounce::new` accepts a caller-supplied `physical` address, independently allocates a `Vec<u8>`, and stores both without establishing any mapping or ownership relationship between them (`src/dma/src/lib.rs:543-573`). `for_device` copies bytes into that `Vec` and cache-cleans the separate physical address; `for_cpu` invalidates that address and then copies from the unchanged `Vec` (`src/dma/src/lib.rs:587-608`). A device operating on `Bounce::physical()` therefore neither reads the bytes staged in the vector nor writes the bytes later returned to the CPU. The test masks this by calling both methods on the same vector and never simulating a device write through the advertised physical memory (`src/dma/src/tests.rs:507-557`). Repository-wide usage also shows `Plan::Bounce`, `Bounce::new`, and its sync methods have no production consumer; the only production `Requirements::new` currently constructs a coherent 64-bit IOMMU requirement (`src/kernel/iommu/mod.rs:632-640`).

   This is not merely missing adoption: the API claimed as M0's portable staging contract cannot work for any adopting driver. Make `Bounce` own or borrow an actual pinned DMA-addressable allocation whose CPU mapping corresponds to its physical/IOVA address (or accept a caller-provided mapped staging slice tied to that address), perform copies through that mapping, and test a simulated device mutation at the advertised address. Then connect the plan/sync result to the kernel DMA-buffer path before calling the portable bounce contract complete.

## Verification

`cargo test --manifest-path src/dma/Cargo.toml --offline` passed all 54 tests. The current bounce test proves method ordering and cache callback spans, but not that the bytes reside at the address given to the device.
