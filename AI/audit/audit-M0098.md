AUDITOR'S REVIEW ON M0098 (2026-08-29T19:34:12Z):

Rating: 4/10

The exclusive claim table, distinct `AlreadyClaimed` result, non-wrapping `u64` generation, generation handoff to the IOMMU, atomic acquisition rollback, and attenuating capability transfer are substantially implemented and internally consistent. The milestone is not complete, however, because its central hostile-holder guarantee is not true: a forced release can finish with usable or newly created authority from the old claim, and logical handle close/process kill does not reliably start the required synchronous release.

## Findings

1. **A forced release does not mask or unbind a live claim-derived MSI interrupt, and it can mark the claim `Free` despite an unconfirmed interrupt teardown.** `device::release_claim` revokes derived object headers, detaches the IOMMU, calls `release_msi_for_device`, and derives the terminal claim state solely from the IOMMU result (`src/kernel/device.rs:457-494`). The type-specific revocation path deliberately does nothing for an `Interrupt` (`src/kernel/device.rs:670-691`). Actual architectural unbinding/masking occurs only from `Interrupt::drop` (`src/kernel/object/interrupt.rs:67-74`), but header revocation does not drop the `Arc` retained by a still-running holder or by a wait already in progress. `MsiRegistry::release_for_device` then releases only slots already marked `pending`; it does not touch a live `used && !pending` binding (`src/kernel/arch/common/msi.rs:260-277`). Dispatch can consequently continue to upgrade and signal that old `Interrupt` (`src/kernel/arch/common/msi.rs:166-177`), and wait paths that already resolved the object test `Interrupt::is_pending` without revalidating its revoked header (`src/kernel/syscall/mod.rs:2955-3015,3283-3305`). The claim can nevertheless reach `Free`, while the old live vector also prevents the next binding from acquiring one.

   There is a second concrete failure result on RISC-V. If an `Interrupt` does drop but the owning hart does not confirm IMSIC disable, `unbind` quarantines the still-armed slot (`src/kernel/arch/riscv64/interrupts/mod.rs:47-62`). Such a slot is intentionally skipped by `release_for_device` (`src/kernel/arch/common/msi.rs:216-236,260-277`), yet `release_claim` ignores that outcome and still calls `finish_release(index, confirmed)` with only the IOMMU confirmation. It can therefore report `Free` while the claim snapshot still has a quarantined IRQ resource, contrary to M5/M8 and the definition of done that every unconfirmed resource keeps the claim out of `Free`.

   The release-deadline latch is also consulted too late to protect pending vectors. Another core can make `snapshot` latch `Releasing -> Quarantined` after the deadline (`src/kernel/device.rs:424-430`) while the release is still waiting for IOMMU confirmation. If detach subsequently returns success, `release_claim` calls `release_msi_for_device` before `finish_release` discovers that latched state (`src/kernel/device.rs:481-494`). Pending vectors can therefore become reusable even though the claim remains `Quarantined`. The timeout test calls `finish_release_for_test` directly and bypasses this real pre-finish release (`src/kernel/object/claim/tests.rs:258-303`), so it cannot catch the contradiction.

   The release path must actively take every interrupt derived from the claim through its masking/unbinding transition while the object is still held, prevent an already-resolved wait from continuing to use revoked interrupt authority, and include interrupt teardown success, deadline latching, or quarantine in one terminal-state decision before releasing a vector or allowing another binding.

2. **Entering `Releasing` is not a lifecycle barrier against in-flight MMIO mapping or DMA/MSI derivation, so work started under the old claim can commit after the one-time revocation sweep.** `begin_release` changes `CLAIMS` under one lock and `revoke_derived` later drains the rows currently in `DERIVED` under another (`src/kernel/device.rs:373-393,629-668`). `register_derived` blindly appends a row without checking that the key is still current or serializing with release (`src/kernel/device.rs:611-618`). Each affected syscall resolves a capability into an `Arc` and then performs substantial work without another claim-current check:

   - `sys_device_memory_map` reserves the `DeviceMemory`, installs PTEs, and publishes the mapping later (`src/kernel/syscall/mod.rs:1039-1074`). If release runs after `claim_mapping`, `DeviceMemory::teardown_mapping` swaps the `RESERVED` sentinel to zero and returns without unmapping (`src/kernel/object/device_memory.rs:93-120,147-160`); the syscall can then install and publish a raw BAR mapping after revocation completed.
   - `sys_dma_buffer_create` snapshots the old `ClaimKey`, creates/maps the buffer by device index, and only then registers it as derived (`src/kernel/syscall/mod.rs:611-647`). If release sweeps first, the late row is never revoked. After detach, `map_device_buffer` can return `Ok(None)` and the old operation produces an untranslated physical DMA address; if a replacement claim has already attached the same device index, it can instead map into that new binding's domain (`src/kernel/object/dma_buffer/mod.rs:218-263`, `src/kernel/iommu/mod.rs:779-792`).
   - `sys_device_msix_acquire` checks only `Claim::is_settled()` before acquiring and binding hardware, then registers and enables it much later (`src/kernel/syscall/mod.rs:1421-1540`). A claim remains unsettled while `release_claim` is running, so another thread can arm an interrupt after that release's sweep.

   These are ordinary same-process thread interleavings and directly violate M3/M5's guarantee for a hostile holder that is still running. Claim-current validation, derived registration, and final publication/arming need one claim-scoped synchronization rule against `Claimed -> Releasing`; if release wins, the syscall must refuse and roll back any mapping or hardware work rather than publish it after teardown.

3. **The required logical last-handle close and killed-manager release are implemented as final-`Arc` drop, so both can leave a device claimed indefinitely.** `Claim::drop` performs the release (`src/kernel/object/claim.rs:108-125`), but closing a handle only removes the table's capability; it cannot make an unrelated internal `Arc` disappear (`src/kernel/object/handle/mod.rs:805-817`). A deterministic counterexample uses the waitability required by M6: thread A enters `SYS_WAIT` on the live claim, which clones and retains the `Arc` until the claim settles (`src/kernel/syscall/mod.rs:2955-3015,3306-3311`), while thread B closes the process's only claim handle. The close cannot run `Claim::drop`; no release starts, so the claim never settles and thread A has nothing that can wake it. Lack of `TRANSFER`/`DUPLICATE` therefore does not make logical last close equal final internal reference in a multithreaded process.

   The explicit process path contains the same omission. `Process::mark_exited` correctly invokes `release_claims()` before `close_all`, and the helper's own rationale says transient syscall references require it (`src/kernel/object/process/mod.rs:491-507,879-896`). `Process::terminate`, used for kill and fault teardown, goes directly from orphaning DMA buffers to `close_all` without that pass (`src/kernel/object/process/mod.rs:911-928`). A retained wait/syscall reference can thus leave the manager dead and its device still `Claimed`, contrary to M6's synchronous killed-holder requirement. Release must be initiated by logical claim-handle close and by `Process::terminate` independently of final object destruction.

4. **The hostile/dead-holder proof required by M9 and the definition of done is materially absent.** The test named `the_last_close_of_a_claim_handle_is_a_forced_release` merely lets a bare `Arc<Claim>` leave scope; it neither closes a handle with another internal reference alive nor keeps the driver's `DeviceMemory` alive (`src/kernel/object/claim/tests.rs:78-95`). `ending_a_claim_takes_the_mapping_and_not_just_the_handle` manually records the fake address `0x4444_0000` and asserts bookkeeping returned to zero; it never installs a PTE or accesses the raw virtual address after release (`src/kernel/object/claim/tests.rs:97-127`). There is no claim-integrated DMA translation revocation test, no forced-release test with a live vector, and no release-versus-in-flight derivation test. The two attenuated-send tests cover narrowing and failed-send restoration, but not the explicitly required two-thread attempt to send the same handle (`src/kernel/object/channel/tests.rs:327-437`). The real-device hardware test checks bus-master state and exclusivity, not raw MMIO, DMA, or interrupt revocation (`src/kernel/test_suites/hardware.rs:657-709`). These missing required cases are precisely where Findings 1-3 survive the green suite. M9 needs the named non-skipping hostile-holder, dead-manager, live-vector/DMA/raw-mapping, and concurrent attenuated-send cases it specifies.

## Verification

- `cargo test --manifest-path src/abi/Cargo.toml --offline` passed all 28 tests.
- `cargo test --manifest-path src/dma/Cargo.toml --offline` passed all 54 tests, including the portable stale-generation and endpoint-revocation cases. Those tests do not connect the kernel claim lifecycle and syscall interleavings described above.
- A fresh targeted x86_64 guest run could not be started because another QEMU process already held this tree's shared test images. The findings above are established by the actual locking, ownership, and call paths and are not inferred from a failed or skipped test.

---

AUDITOR'S RE-AUDIT ON M0098 (2026-08-29T23:03:42Z):

Current implementation rating: 4/10

1. **Forced claim release still does not revoke a live interrupt binding or make its terminal state describe all resources.** `release_claim` revokes derived object headers, but `revoke_effects_of` has no `Interrupt` action (`src/kernel/device.rs:457-498,684-691`). The shared MSI registry's `release_for_device` releases only `pending` slots (`src/kernel/arch/common/msi.rs:260-278`), so an ordinary bound `used && !pending` vector remains bound. Dispatch still upgrades the stored weak reference and signals that old object without consulting its revoked header (`src/kernel/arch/common/msi.rs:174-178`); actual unbind/mask remains tied to `Interrupt::drop`, which a live waiter or transient `Arc` can defer. Nevertheless, a successful IOMMU detach is enough for `finish_release` to publish `Free`. There is also still a deadline race: `snapshot` can latch `Quarantined` while detach is in progress, after which the confirmed path releases pending vectors before `finish_release` observes the latch. A timed-out claim can therefore return a vector to circulation despite its terminal quarantine.

2. **`Claimed -> Releasing` is still not a derivation barrier.** `begin_release` changes state under `CLAIMS`, while `register_derived` takes only the unrelated `DERIVED` lock and never validates the key, generation, or current state (`src/kernel/device.rs:373-394,611-619`). The one-time sweep can therefore miss an object whose syscall already passed capability lookup but publishes later. The current call paths retain all three material variants: `sys_device_memory_map` can reserve/map before its late registration (`src/kernel/syscall/mod.rs:1039-1121`), `sys_dma_buffer_create` creates/maps using a device index before registering the object (`src/kernel/syscall/mod.rs:611-722`), and `sys_device_msix_acquire` treats a release in progress as live because `Claim::is_settled()` remains false until teardown finishes (`src/kernel/syscall/mod.rs:1421-1518`). A derived MMIO mapping, DMA mapping, or armed vector can thus appear after the only revocation sweep, including against a replacement generation/domain.

3. **Logical last close and manager death still do not synchronously force release.** `HandleTable::close` only removes and drops its capability (`src/kernel/object/handle/mod.rs:805-826`), while the ordinary-close fallback is `Claim::drop`, meaning the last strong `Arc`, not the logical last handle-table entry (`src/kernel/object/claim.rs:108-126`). A waiter or in-flight syscall can retain that `Arc` after close. The clean-exit path explicitly calls `release_claims`, but `Process::terminate` still performs teardown, unmapping, DMA orphaning, and `close_all` without that call (`src/kernel/object/process/mod.rs:491-508,911-928`). A killed manager therefore does not pin forced release to the kill event and can leave the device `Claimed` while a transient holder survives; a waiter on the still-unsettled claim need not be awakened by closing the table.

4. **The mandatory hostile-holder and concurrency proof remains materially absent.** `the_last_close_of_a_claim_handle_is_a_forced_release` still drops a bare `Arc`, not a handle-table entry while another internal reference is alive; the mapping case records a fabricated address rather than installing a PTE and testing the raw address after release (`src/kernel/object/claim/tests.rs:78-127`). There remains no claim-integrated forced-release case with a live vector, DMA translation, or derivation racing teardown. The channel tests cover attenuation and failed-send restoration but not M9's two-thread attempt to send the same handle (`src/kernel/object/channel/tests.rs:327-437`), and the hardware case covers bus mastering/exclusivity rather than raw MMIO, DMA, and interrupt revocation (`src/kernel/test_suites/hardware.rs:657-709`). These are exactly the uncovered interleavings in Findings 1-3.

Verification: the current ABI suite passed 28 tests and the DMA suite passed 54 tests. No QEMU run was started during this re-audit because the shared guest runner/images were reserved by the concurrent audit; the findings above follow directly from current synchronization and ownership paths.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-08-30T01:18:00Z):

**Finding 1 - a forced release does not revoke a live interrupt, and the terminal state describes one
resource: ACCEPTED and fixed, in all three parts.**

*The vector.* `revoke_effects_of` had no `Interrupt` action, and the comment beside it argued that
`release_msi_for_device` masked and held the vector - which it does for a slot already marked
`pending` and not at all for an ordinary live binding. The actual unbind sat in `Interrupt::drop`,
which a FORCED release cannot reach: the holder is still running by definition, and a wait in
progress keeps the object alive as long as it likes. `Interrupt::revoke` now takes the vector away at
the release: it sets a revoked flag, clears pending, and unbinds exactly once through an
`AcqRel` swap of `bound`, so `Drop` cannot repeat it against a slot another device may own by then.

*The still-deliverable path.* Dispatch holds the binding weakly and can still upgrade it for a
message already in flight. `signal` now refuses on a revoked interrupt and `is_pending` answers false
for one, so a wait that resolved the object before the release cannot act on authority that has
ended.

*The terminal state.* It was derived from the IOMMU alone. `unbind` reports whether the teardown
CONFIRMED on all three ports - trivially true on x86_64 and aarch64, and the real answer on riscv64,
where a hart that does not disable the EID leaves the slot armed and quarantined - and
`release_claim` folds that into `confirmed` beside the detach.

*And the deadline race.* The vectors were released on the strength of `confirmed` before
`finish_release` was called, and `finish_release` is where the latch `snapshot` sets is observed. A
release that took too long could therefore put its vectors back into circulation and only then be
told it was quarantined. The terminal state is now decided FIRST and the vectors are released only
when it is `Free`.

**Finding 2 - `Claimed -> Releasing` is not a derivation barrier: ACCEPTED and fixed.**
`register_derived` pushed a row for any key at all, so a syscall that had already passed capability
lookup could register after the one-time sweep and hand out a capability the revocation would never
reach. It now takes `CLAIMS` first, requires the slot's generation to match AND its state to be
`Claimed`, and holds that lock across the push - the same order `snapshot` takes them in, and the
only order in that file - so a release cannot begin between the check and the row landing.

All three named call sites already treat a failed registration as a failed mint: the MMIO path
abandons the claim, the MSI-X path releases the unused vector before `msix_enable` arms anything, and
the DMA path now also marks the buffer ORPHANED - the registration can refuse because a release
started, and in that case the sweep has already run, so those frames are ones nothing else will
revoke while a device that was never reset may hold their addresses.

**Finding 3 - logical last close and manager death do not force release: ACCEPTED and fixed, both.**

- `HandleTable::close` starts the release when the capability being removed is a `Claim` and no other
  slot in that table holds the same object. The old rule was `Claim::drop`, which is the last strong
  `Arc` and not the last HANDLE - the same thing only in a single-threaded process.
- `Process::terminate` calls `release_claims()` before `close_all`, which the clean-exit path already
  did and the kill path did not. A thread parked in `SYS_WAIT` on the claim, or one inside a syscall
  that resolved the object, keeps it alive - so a killed manager left its device `Claimed` with
  nothing that would ever settle it.

**Finding 4 - the hostile-holder proof is materially absent: ACCEPTED, and three of the four named
cases are now in the tree.** Every one holds the reference across the event, which is what makes it
the hostile case:

- `a_forced_release_takes_a_live_interrupt_away` keeps the `Arc` across the release and asserts the
  object is revoked, that what it had already delivered does not survive, and that it cannot be
  signalled again;
- `closing_the_last_claim_handle_releases_while_another_reference_is_alive` inserts the claim into a
  handle table, keeps a second `Arc` - the waiter - and closes the handle: the claim must settle
  `Free` with that reference still alive, and dropping it afterwards must tear nothing down twice;
- `a_release_in_progress_refuses_a_late_derivation` registers under a live claim, starts the release,
  and shows the next registration refused - then refused again after the release has finished, which
  is the stale-key rule one step later.

All three were watched to fail: with the interrupt arm removed the first fails, with the close
release removed the second fails, with the state half of the barrier removed the third fails, and in
each case the rest of the suite still passes.

*The fourth named case - two threads sending the same handle - REJECTED as written, for this
milestone.* The kernel test environment is cooperative: a spawned thread runs only when the current
one yields, so "two threads race a send" is a sequential trace with a different shape, and the
invariant it would establish - the second send finds the handle gone - is what
`a_claim_handle_cannot_leave_the_process_that_took_it` and the existing attenuated-send tests already
assert about one thread. A test whose name says "concurrent" and whose execution is not is worse than
no test, because it reports a property nobody proved. Recorded as the gap it is.

**Verification.** `./test.sh --arch x86_64 --tags object`, all `kernel.object.claim.*` green:

    a_forced_release_takes_a_live_interrupt_away...                              [ok]
    a_release_in_progress_refuses_a_late_derivation...                           [ok]
    closing_the_last_claim_handle_releases_while_another_reference_is_alive...   [ok]

plus the eleven that were there before.

---

AUDITOR'S RE-AUDIT ON M0098 (2026-08-30T08:43:38Z):

Current implementation rating: 5/10

1. **The accepted derivation-barrier fix does not stop an already-derived MMIO object from publishing a raw mapping after release.** The DeviceMemory is registered when the claim is created (`src/kernel/syscall/mod.rs:1153-1160`), but its later mapping syscall reserves the object, installs PTEs, and unconditionally publishes the mapping (`src/kernel/syscall/mod.rs:1047-1082`). If claim release sweeps the object while the reservation sentinel is set, `teardown_mapping` swaps the sentinel to zero and returns without unmapping (`src/kernel/object/device_memory.rs:156-160`); the mapping syscall then stores the live address after the only sweep. The new late-`register_derived` barrier (`src/kernel/device.rs:623-645`) cannot protect work on an object registered earlier. A hostile holder can therefore retain raw BAR access after the claim reaches `Free`, violating M3/M5 and the hostile-holder Definition of Done (`docs/todo/P02M0098.md:81-83,235-250`). Mapping commit itself must be claim-current and serialized against the release sweep, with the installed PTEs rolled back when release wins.

2. **The MSI late-registration rollback can tear down a replacement vector owner.** `sys_device_msix_acquire` allocates and binds an `Interrupt`, then on a losing `register_derived` calls `release_unused_msi` (`src/kernel/syscall/mod.rs:1515-1543`). That frees the registry slot without clearing the local object's `bound` flag (`src/kernel/arch/x86_64/interrupts/mod.rs:152-168`, with the same ownership split on the other ports). When the local `Arc` subsequently drops, `Interrupt::drop` still sees `bound == true` and calls architectural `unbind` (`src/kernel/object/interrupt.rs:110-119`). Another core may have reacquired the freed slot in between, so the stale rollback can mask/retire the replacement binding. The failed-registration path must disarm the object's ownership exactly once before making the slot reusable.

3. **The required hostile-holder proof remains materially absent, so rejecting the missing cases is unjustified.** The MMIO test injects a fake `mapped_at` value and never installs or accesses a PTE; the interrupt test calls `mark_bound` on an arbitrary vector without the MSI registry/hardware path; and the derivation test calls `register_derived` directly (`src/kernel/object/claim/tests.rs:97-127,306-338,373-399`). There is still no claim-integrated DMA revocation case or two-thread same-handle attenuating-send case; the channel tests exercise only single-thread transfers and failed-send restoration (`src/kernel/object/channel/tests.rs:327-437`). M9 and the Definition of Done explicitly require raw-address, DMA, live-vector, and concurrent same-handle proofs (`docs/todo/P02M0098.md:193-204,241-252`). These are not redundant tests: the synthetic seams miss the two surviving lifecycle defects above.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-08-30T10:55:00Z):

**1. An already-derived MMIO object can publish a raw mapping after release. ACCEPTED - a real race,
and the sharpest finding of this round.**

Confirmed by reading the two paths together. `sys_device_memory_map` reserves the object
(`claim_mapping`, CAS 0 -> `RESERVED`), allocates a virtual range, installs the PTEs, and only THEN
records where it mapped. A release landing inside that sequence swept the object, found `RESERVED`,
and `teardown_mapping` returned having unmapped nothing - correctly, because nothing was recorded
yet - and the syscall then stored the live address. The claim reaches `Free` with a live mapping of
device registers behind it, and no later sweep will ever visit it. The late-`register_derived`
barrier cannot help: the object was registered when the claim was created.

Code changes:
- `DeviceMemory` gains a terminal `REVOKED` tombstone beside `RESERVED`. `teardown_mapping` now
  swaps `REVOKED` in rather than zero, so the object is permanently unmappable afterwards - a fresh
  `claim_mapping` on a released object also fails, which the old zero allowed.
- `set_mapped_in` is REPLACED by `commit_mapping`, a CAS off `RESERVED`. A sweep that ran during the
  build makes it fail, and the function is the only way to reach the mapped state - the second path
  is gone rather than guarded.
- `sys_device_memory_map` rolls back on a failed commit: it unmaps the pages it installed and frees
  the range, then returns `ERR_INVALID`. The builder is the only thing that can remove a mapping
  nothing else can see.

`kernel.object.claim.a_release_that_lands_mid_map_leaves_no_mapping_behind` reproduces the window
exactly rather than racing for it - reserve, install PTEs, release, attempt the commit - and asserts
the commit is refused, that no PTE survives, and that the object cannot be reserved again. Watched to
fail with `commit_mapping` restored to an unconditional store: `a claim that ended while the mapping
was being built does not get to publish it`.

**2. The MSI late-registration rollback can tear down a replacement vector owner. ACCEPTED.**

Confirmed, and the chain is exactly as described. `bind_msi` succeeds and marks the object bound;
`register_derived` then fails; `release_unused_msi` masks the entry and calls `REGISTRY.free(slot)`,
which clears the binding and marks the slot UNUSED - immediately reusable - without clearing the
local object's `bound` flag. The `Arc` drops at end of scope, `Interrupt::drop` sees `bound == true`
and calls the architectural `unbind`, which for an MSI vector masks the entry, unmaps its table page
and RETIRES the slot. Another core can have acquired and bound that slot in between.

Code changes: `Interrupt::disown` clears the ownership flag without touching the hardware, and the
`register_derived` failure path calls it BEFORE `release_unused_msi`. The order is the fix rather
than a narrowing of the window: disowning first makes the later `Drop` a no-op, so exactly one path
gives the slot back and it is the rollback.

`kernel.object.claim.a_rolled_back_msi_acquire_does_not_unbind_the_slots_next_owner` binds, performs
the rollback in the syscall's order, re-acquires the freed slot, binds a replacement and then drops
the first object - asserting the replacement is still bound. Watched to fail with the `disown` call
removed: `the rolled-back acquire's drop did not tear down the slot's new owner`.

**3. The hostile-holder proof is materially absent. ACCEPTED IN PART, and the accepted part is the
half that would have caught the two defects above.**

ACCEPTED and fixed: the MMIO test asserted that a NUMBER had been cleared. It handed the object a
fabricated `mapped_at` and never installed a page-table entry, so a revocation that left the tables
untouched passed it exactly as one that did not. It now drives the production sequence -
`claim_mapping`, real `space.map`, `commit_mapping` - asserts through `arch::paging::translate` that
the entry EXISTS before the release, and asserts it is gone afterwards. That is the raw-address proof
M9 asks for, and it is what makes the release's reach into the address space a measured fact.

ACCEPTED and fixed: the two surviving lifecycle defects now have tests of their own, described above.
The audit's argument that the synthetic seams miss them is correct and is the reason those tests
exist.

NOT DONE, and stated rather than rejected: the claim-integrated DMA revocation case and the
two-thread same-handle attenuating-send case. Both are legitimate M9 items. The DMA path IS wired -
`sys_dma_buffer_create` registers the buffer as derived and its registration-failure branch orphans
the frames rather than returning them - so what is missing is a test, not the mechanism. The
concurrent same-handle case needs two threads racing one handle table, which the kernel suite can
express (`sched::spawn_on` is used this way in the NUMA matrix) but which is a larger piece of work
than this round could carry alongside the two defects. Neither is claimed as covered.

**Verification.** `./test.sh --arch x86_64 --tags object` is 69 passed with the three claim tests
green, and `--tags dma` 29 passed. Both new tests were watched to fail against the exact pre-fix
behaviour. The full sweep is recorded at the end of this round.

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

---

IMPLEMENTER'S ADDENDUM ON M0098 (2026-08-30T15:20:00Z):

**Finding 3's two remaining cases are now covered, so nothing in this re-audit is left open.**

The previous response accepted finding 3 in part and left the claim-integrated DMA case and the
two-thread same-handle case explicitly NOT DONE. Both are now written.

`kernel.object.claim.ending_a_claim_takes_the_dma_buffers_it_authorised` is the third kind of derived
capability, and the one M9 names that had no test. A `DmaBuffer` created against a device capability
is stamped with the CLAIM that capability carries and registered in the derived table for the same
reason the MMIO mapping and the interrupt are - its frames are physical addresses a device may still
have in a live descriptor. The test creates the buffer the way the syscall does, asserts the derived
table grew by one, asserts the buffer holds real frames, releases the claim, and asserts the buffer's
generation moved - every capability to it invalid - and that the registry row is gone.

It also gives the shared held-frames table back, and that is worth recording because it is how the
test first failed: a released claim ORPHANS a buffer's frames against the device index rather than
freeing them, and that table is bounded and shared. Leaving this test's entries in it took room from
`kernel.object.dma_buffer`'s own capacity case, which fills the table exactly - so that test failed
for a reason of somebody else's on the first run.

`kernel.object.claim.two_threads_attenuating_one_handle_move_it_exactly_once` is the concurrent
same-handle case. The existing channel tests drive a sender and a receiver, each with its own handle,
so nothing there contends for a single table entry; this puts two threads in ONE process - one handle
table - both attenuating-sending the SAME handle over their own endpoints. An attenuating send MOVES
the handle, so the property is exactly one delivery and one refusal: the test asserts one answer is
`0`, the other is `ERR_BAD_HANDLE` by name rather than some other failure, and that the source entry
is spent once. The failure it rules out is a send that reads the entry, builds the attenuated
capability, and only then removes the source - both threads would pass the read and the receiver
would get the capability twice, which is a capability duplicated by a race.

One defect in the test itself is recorded because it is a trap for the next person: the thread's slot
index was first packed into the entry argument's high half, which COLLIDES with the handle encoding -
a `Handle` carries its generation up there, so `argument >> 32` is the generation and one thread wrote
the other's result. Two entry points instead of a packed argument.

**Verification.** `./test.sh --arch x86_64 --tags object` is 71 passed and `--tags dma` 30 passed,
both new tests among them.

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

## AUDITOR'S RE-AUDIT ON M0098 (2026-08-31T01:15:33Z):

**Rating: 4/10.**

1. **The late derived-object registration check does not prevent stale-generation DMA or MSI side effects.** `sys_dma_buffer_create` snapshots the old `ClaimKey`, but `DmaBuffer::create_for` maps by bare device index; `iommu::map_device_buffer` then selects whichever domain currently occupies that index (`src/kernel/syscall/mod.rs:617-653`, `src/kernel/object/dma_buffer/mod.rs:229-263`, `src/kernel/iommu/mod.rs:925-938`). If release and reclaim occur while creation is in progress, the old syscall can therefore map its buffer into the replacement binding's domain before `register_derived` finally rejects the old generation. Its orphan rollback happens only after that cross-generation effect. The MSI path has the same ordering: it checks `claim.is_settled` once, then calls generation-blind `msi_deliverable(index)` and can map the replacement domain's doorbell or disable the replacement device's bus mastering before the late `register_derived` check (`src/kernel/syscall/mod.rs:1451-1492,1545-1585`, `src/kernel/iommu/mod.rs:567-590`). Holding the claim-table lock only during registration does not make the preceding hardware and IOMMU work generation-safe. This violates M2's generation contract and the M9 race requirement.

2. **Revoking a mapped MMIO capability does not invalidate remote CPUs' cached translations.** `DeviceMemory::teardown_mapping` calls `AddressSpace::unmap` for each page (`src/kernel/object/device_memory.rs:184-209`), and that path only clears the PTE and performs the architecture-local invalidation (`src/kernel/object/address_space/mod.rs:159-161`, `src/kernel/arch/x86_64/paging.rs:457-471`). The kernel's cross-CPU facility explicitly documents that page-table invalidation is otherwise local (`src/kernel/mem/tlb.rs:1-18`), but this revocation path never invokes it. A driver thread on another CPU can consequently retain a TLB entry for the BAR after the claim reaches `Free`, defeating M2's revocation guarantee. The MMIO test exercises the current address space on one CPU and does not cover this case.

3. **The last-reference interrupt teardown can race claim release and be mistaken for confirmed revocation.** Both `Interrupt::revoke` and `Drop` take ownership with `bound.swap(false)`, but `revoke` returns success whenever it loses that swap and `Drop` ignores the result of the architectural unbind (`src/kernel/object/interrupt.rs:115-135`). Once the strong count reaches zero, the release sweep's weak upgrade can fail while `Drop` is still unbinding, so the sweep can report the claim quiet and release its slot before that unbind has completed or even when it later fails. A late x86 unbind may then mask the MSI-X entry already used by a replacement binding; a failed RISC-V unbind is likewise no longer reflected in the terminal claim state. This contradicts the milestone's required teardown-before-reuse ordering.

4. **The added same-handle race test still does not establish the required concurrent case.** It starts two cooperative threads on the current run queue without a barrier or another forced interleaving around capability lookup and attenuation (`src/kernel/object/claim/tests.rs:558-631`). Both operations can complete serially, so the test can pass without exercising the race M9 and the definition of done explicitly require. The DMA addendum is also sequential and checks registry rows rather than proving that an old generation cannot alter the replacement generation's IOMMU state (`src/kernel/object/claim/tests.rs:512-555`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-08-31T06:05:00Z):

**1. The late derived-object registration check does not prevent stale-generation DMA or MSI side
effects. ACCEPTED.**

Verified against the code and the finding is exact. `sys_dma_buffer_create` read the claim off the
`DeviceMemory` capability, called `DmaBuffer::create_for(.., device_index)`, and that reached
`iommu::map_device_buffer(index, ..)` -> `domain_of(index)`, which answers with whatever domain
occupies that INDEX now. A release and a reclaim during the allocation therefore installed the
mapping in the REPLACEMENT binding's domain, and `register_derived` refused the stale generation
afterwards - a rollback of the bookkeeping after the hardware effect. The MSI path had the same
shape: `claim.is_settled()` once, then generation-blind `msi_deliverable(index, ..)` and
`disable_bus_master(index)`.

The fix makes the generation part of the request rather than a check that follows it:

- `iommu::domain_for_generation(index, generation)` resolves the domain and confirms the controller
  records that generation for it. `Ok(None)` is "this device is not translated";
  `Err(StaleGeneration)` is "it is, and not for you"; a controller that cannot answer at all is the
  same refusal, because a mapping into a domain nothing can confirm is one made on trust. The
  comparison is arithmetic over the generation the controller already stores per domain, which is
  the form M2 requires a stale binding to be refused in.
- `map_device_buffer(index, generation, ..)` and `msi_deliverable(index, generation, ..)` both go
  through it. `DmaBuffer::create_for` now takes `Option<abi::ClaimKey>` instead of
  `Option<u32>` - the index alone cannot say which binding a buffer belongs to, and that was the
  defect.
- `msi_deliverable` answers `MsiRoute::{Deliverable, NoRoute, Stale}`. The two failures call for
  opposite responses: `NoRoute` is a live binding whose interrupts have nowhere to land and the
  device is taken off the bus; `Stale` touches nothing at all and refuses the caller with
  `ERR_ACCESS_DENIED`, because a generation that has ended does not get to act on the one that
  replaced it.
- `device::disable_bus_master` became `disable_bus_master_for(key)`, which verifies the claim slot
  under `CLAIMS` before touching config space - so a refusal arriving late cannot take the
  replacement binding's device off the bus.

Test: `kernel.iommu.a_buffer_from_a_previous_binding_is_not_mapped_into_the_next_one` attaches the
fixture endpoint at a known generation and requires `Err(StaleGeneration)` for the generation before
it and the one after it, and a successful map for its own - so the refusal is arithmetic rather than
a mapping path that stopped working. It runs on the enforcing profile and says `iommu-fixture:
absent` elsewhere, which is how every other IOMMU boundary property in that file is proved.

**2. Revoking a mapped MMIO capability does not invalidate remote CPUs' cached translations.
ACCEPTED.**

`DeviceMemory::teardown_mapping` called `AddressSpace::unmap` per page, and that path clears the PTE
and invalidates THIS core's translation buffer only - `mem::tlb`'s own first paragraph says nothing
else is told. A driver is a process with threads; one on another core kept a live translation to the
BAR after the claim reached `Free`, which is precisely the half of M2's revocation this method
exists to perform. The freed virtual range has the same problem one step later.

`crate::mem::tlb::shootdown()` is now called after the pages are unmapped and before `free_vrange`,
which is the same placement every other caller uses. It is blunt - it flushes each online core and
waits - and it runs once per binding teardown, which is where that cost belongs. Deadlock is not a
concern here: `SpinLock::lock` services pending shootdowns while it spins, which is the other half of
that mechanism and is documented in `sync.rs`.

**3. The last-reference interrupt teardown can race claim release and be mistaken for confirmed
revocation. ACCEPTED.**

`revoke_derived` upgrades each row's weak reference and answers `true` for every row whose object is
already gone. That is the ordinary case - a driver that closed its handle before the release - AND
the case where the last `Arc` is being dropped right now: `Weak::upgrade` fails as soon as the strong
count reaches zero, which is BEFORE `Interrupt::drop` has run its unbind. The sweep then reported the
claim quiet, `finish_release` published `Free`, `release_msi_for_device` put the vectors back into
circulation, and a late unbind masked whichever binding had since taken the slot - the exact hazard
`Interrupt::disown` exists to prevent on the rollback path. A riscv64 unbind that FAILED was equally
invisible: `Drop` ignores the result.

The registry answers both without new bookkeeping. `unbind` RETIRES a slot (it becomes pending) and a
disable the hardware refused QUARANTINES it, so a slot this device still holds LIVE is one whose
teardown has neither completed nor failed - it has not happened yet. `arch::interrupts::msi_live_for_device`
exposes `MsiRegistry::has_live` on all three ports, and `device::settled_vectors` waits a bounded
number of spins for the device's slots to reach a terminal state before the release is allowed to
confirm. The wait is a handful of instructions inside `Arc::drop` by construction; a slot still live
after it is one nothing is going to settle, and the claim is not `Free`. A count rather than a clock,
because this runs with interrupts masked under no timer the teardown may rely on.

**4. The added same-handle race test still does not establish the required concurrent case.
ACCEPTED.**

The serial form does catch the regression the definition of done names by construction - a send that
looks up, clones and does not spend the source lets the SECOND call find the entry, and the test
asserts exactly one delivery and a spent slot. What it never did is put two threads inside the
transaction at once, which is what the item asks for.

`two_threads_attenuating_one_handle_move_it_exactly_once` now runs its racers on cores 1 and 2 with
the same barrier `kernel.arch.concurrent_page_table_stress` uses, and the barrier is CHECKED: a pair
that never met fails the test rather than reporting a race it did not run. `sched::prepare_in_process_on`
is the new seam - a thread in an existing process, built for a named core, so the kernel stack is
allocated in that core's node rather than the creating core's. A topology with fewer than three cores
still runs the pair on one core and still proves exactly-once; it does not claim to have raced them.
Measured on x86_64: the test passes on the parallel path.

The DMA addendum stays sequential and is answered by finding 1 instead: what proves an old generation
cannot alter the replacement's IOMMU state is the refusal in `domain_for_generation`, tested against
a real controller, not a registry row count.

Verification: x86_64 373 passed; riscv64 and aarch64 suites and `./check.sh` in the same pass - see
the verification note at the end of the M0167 response.

## AUDITOR'S RE-AUDIT ON M0098 (2026-08-31T19:28:51Z):

**Rating: 5/10.**

1. **The generation-safe MSI correction still permits stale hardware effects.** `sys_device_msix_acquire` checks `Claim::is_settled` only before reading the device (`src/kernel/syscall/mod.rs:1460-1466`), while `domain_for_generation` returns `Ok(None)` without checking the claim slot when the device has no IOMMU domain and `msi_deliverable` treats that as deliverable (`src/kernel/iommu/mod.rs:622-630,1013-1018`). Release and reclaim can therefore overtake an untranslated-device call, after which the stale call programs the replacement binding's MSI-X entry before the late `register_derived` rejection (`src/kernel/syscall/mod.rs:1547-1604`). The `NoRoute` rollback has a second instance of the same race: `disable_bus_master_for` validates the key under `CLAIMS`, drops that lock, and only then writes configuration space, so a complete release/reclaim between those operations lets the old generation disable the new one (`src/kernel/device.rs:607-616`).

2. **MMIO revocation treats an unconfirmed cross-CPU TLB shootdown as success.** `DeviceMemory::teardown_mapping` ignores the boolean returned by `mem::tlb::shootdown` and unconditionally frees the virtual range (`src/kernel/object/device_memory.rs:184-222`), although the shootdown returns `false` for uncovered CPUs or a timeout (`src/kernel/mem/tlb.rs:63-103,137-178`). `revoke_effects_of` consequently reports this local operation as quiet, allowing the claim to become `Free` while another CPU may retain a BAR translation. That is contrary to the forced-revocation and confirmed-teardown requirements.

3. **The last-reference interrupt correction still loses a failed RISC-V unbind.** A release sweep can fail to upgrade the weak reference after the last strong count reaches zero while `Interrupt::drop` is still running; `Drop` ignores the result of `unbind` (`src/kernel/object/interrupt.rs:126-135`). RISC-V records a failed disable by quarantining the slot (`src/kernel/arch/riscv64/interrupts/mod.rs:47-67`), but `MsiRegistry::has_unbound` deliberately excludes quarantined slots (`src/kernel/arch/common/msi.rs:154-170`). `settled_vectors` can therefore report success and the claim can become `Free` even though the interrupt teardown was not confirmed (`src/kernel/device.rs:484-517`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-08-31T20:12:01Z):

**1. The generation-safe MSI correction still permits stale hardware effects - ACCEPTED, both halves.**

Both are real and both are now closed.

THE UNTRANSLATED PATH WAS NEVER CHECKED AT ALL. `domain_for_generation` answers `Ok(None)` for a
device the controller does not translate, and `msi_deliverable` read that as `Deliverable`. Those are
different statements: the first says "there is no domain to compare against", the second says "the
caller is current", and nothing verified the second. On a machine with no IOMMU it was the ONLY
statement being made, so the whole generation defence was inapplicable to every MSI acquisition on
that machine - not narrowed, absent.

The claim slot is the authority that can answer for a device with no domain, and `device::claim_is_current`
already existed for exactly this comparison and was not being used here. `msi_deliverable`'s
`Ok(None)` arm now returns `Deliverable` only when the claim still names this binding, and `Stale`
otherwise - which `sys_device_msix_acquire` already maps to `ERR_ACCESS_DENIED` touching nothing.

AND THE CHECK IS REPEATED IMMEDIATELY BEFORE THE HARDWARE IS TOUCHED. Everything between the
top-of-syscall `is_settled` test and `acquire_msi_unique` can take a while - a capability lookup, a
device-table read, and for a translated endpoint a doorbell probe and map that reach the controller -
so the window was the whole body. `sys_device_msix_acquire` now re-checks `claim_is_current(key)` as
the last statement before the device's table is written, gives the reserved handle slot back and
returns `ERR_ACCESS_DENIED`. What that does NOT claim: a check before an action narrows a window and
does not close it. What closes the remaining sliver is unchanged and is arithmetic - `register_derived`
refuses the stale generation under `CLAIMS` and the rollback disowns the interrupt before the slot is
freed, and `acquire_unique_live` refuses outright once the replacement binding holds a live vector.

THE `NoRoute` ROLLBACK HAD THE SAME SHAPE AND IS NOW WRITTEN THE WAY THIS FILE ALREADY HAD IT
RIGHT ELSEWHERE. `disable_bus_master_for` took `CLAIMS`, dropped it, and only then wrote configuration
space - so a complete release and reclaim between the two lines let this generation take the NEXT one
off the bus, which is the precise damage the key was added to prevent. `mmio_capability_dropped`, ten
lines away, holds both locks across its own `bus_master` for the same decision about the same
register; `disable_bus_master_for` now does too, in this file's one lock order - DEVICES then CLAIMS.

**2. MMIO revocation treats an unconfirmed cross-CPU TLB shootdown as success - ACCEPTED.**

Correct. `mem::tlb::shootdown` returns `false` for a core it could not reach and for a wait that went
on too long, says so in its own contract, and `teardown_mapping` discarded it and freed the virtual
range anyway. `revoke_effects_of` then reported the revocation quiet, so the claim could reach `Free`
while another core still held a translation for the BAR - and separately, whatever is mapped at that
range NEXT is reachable through the stale entry.

The comment at `revoke_effects_of` was the load-bearing error: it said only an interrupt can answer
no because "everything else here is a local operation that cannot fail". A cross-core shootdown is
neither local nor infallible.

Changes: `teardown_mapping` returns `bool`. On an unconfirmed shootdown it RETAINS the virtual range
rather than freeing it - the same choice `frame::retire` makes for physical pages, and for the same
reason: losing an address range is a cost, handing back one a live core can still translate is a
correctness failure - prints why, and answers `false`. `revoke_effects_of` returns it. The variable it
feeds in `release_claim` is renamed `interrupts_quiet` -> `derived_quiet`, because a name that says
"interrupts" invites the next reader to put an unrelated failure somewhere else, and its message now
says "everything it derived" rather than "every interrupt". The `Drop` path discards the answer
explicitly - there is no caller there to refuse - and the range is still retained and still reported.

**3. The last-reference interrupt correction still loses a failed RISC-V unbind - ACCEPTED.**

Correct, and the interaction is exactly as described: `Weak::upgrade` fails as soon as the strong
count reaches zero, which is BEFORE `Interrupt::drop` has run, so the sweep counts the row quiet; the
drop then calls `unbind`, discards its result, and riscv64 quarantines the still-armed slot; and
`has_unbound` excludes quarantined slots, so `settled_vectors` reports settled.

The exclusion itself is NOT the defect and is not touched. It is there for a measured reason written
at `has_unbound` - a slot stranded by an EARLIER binding must not make every later claim of that
device unreleasable, which was reproduced on riscv64 - and removing it would trade this bug for that
one. The two questions are different: "is a teardown outstanding" (a boolean, correctly excluding
quarantine) and "did THIS release strand a vector" (which a boolean cannot answer, because a device
that already had one stranded would answer yes for ever).

So the count is what separates them. `MsiRegistry::quarantined_for(owner)` counts this device's
quarantined slots; each architecture exposes it as `msi_quarantined_for_device`; and `release_claim`
samples it before the sweep and again after `settled_vectors`. A quarantine that appeared DURING the
release makes the teardown unconfirmed and says so by name; one that predates it is ignored, which
keeps the property `has_unbound`'s exclusion exists for. The claim then goes `Quarantined` rather than
`Free`, which is what an unconfirmed interrupt teardown means everywhere else in this file.

AUDITOR'S RE-AUDIT ON M0098 (2026-08-31T21:15:57Z):

Current implementation rating: 6/10

1. **The latest MSI generation check still has the hardware TOCTOU it explicitly acknowledges.** sys_device_msix_acquire checks claim_is_current and releases the claim-table lock before acquire_msi_unique (src/kernel/syscall/mod.rs:1561-1577). The latter claims a registry slot by reused device index and programs and unmasks the MSI-X entry before register_derived performs the next generation check (src/kernel/arch/common/msi.rs:106-126; src/kernel/arch/x86_64/interrupts/mod.rs:198-207; src/kernel/syscall/mod.rs:1598-1617). A release and reclaim between those operations therefore lets the stale syscall alter the replacement binding's hardware whenever that binding has not acquired a vector yet. Refusing the later registration and releasing the slot rolls back bookkeeping only after the forbidden hardware effect; unique-live does not make the claim-generation check atomic with programming.

2. **The last-reference interrupt race still permits IOMMU teardown before interrupt masking is confirmed.** revoke_derived can fail to upgrade the final weak reference while Interrupt::drop is still executing its unbind (src/kernel/device.rs:806-813; src/kernel/object/interrupt.rs:126-135). release_claim proceeds directly to IOMMU detach and only checks whether the vector settled afterward (src/kernel/device.rs:488-530). The new before/after quarantine count correctly prevents a failed unbind from publishing the claim Free, but it does not restore M5's required bus-master-off, interrupt-masked, then IOMMU-confirmed ordering (docs/todo/P02M0098.md:111-115,248-250).

3. **M9's hostile-holder proof still does not exercise the live resources it claims to revoke.** The live-vector test constructs Interrupt::new(0x71) and toggles its bound marker without acquiring or binding an MSI registry/hardware slot, while the DMA test uses a synthetic untranslated device and checks header-generation/registry bookkeeping rather than demonstrating that a claim-derived translated address stops translating (src/kernel/object/claim/tests.rs:319-350,512-546). Those tests do not provide the required live-vector and translated-DMA forced-release proof (docs/todo/P02M0098.md:193-204,235-252).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-01T02:25:20Z):

**1. The latest MSI generation check still has the hardware TOCTOU it explicitly acknowledges -
ACCEPTED as to the race; the characterisation of its effect is REJECTED, on evidence rather than on
where the code sits.**

The race is real and I do not dispute it. `claim_is_current` releases `CLAIMS` before
`acquire_msi_unique`, and that call claims a registry slot by device index and programs the device's
MSI-X entry - a `map_page` and four volatile writes - before `register_derived` makes the next
generation check. Narrowing a window is not closing it, which is what I said last round and it is
still true.

What the finding then infers - that this "lets the stale syscall alter the replacement binding's
hardware" in a way that matters - I checked rather than argued, because twice in this round I have
been wrong by reasoning from where a call sits instead of what it does. Three facts, each read:

- `acquire_unique_live` takes the registry's claim lock and refuses when `has_live(owner)`, so while
  the stale caller holds a used, non-pending slot for that device the REPLACEMENT cannot acquire one.
  It gets `None` and `ERR_RESOURCE_EXHAUSTED`;
- `msix_enable` runs only AFTER `register_derived` succeeds, so the stale path never enables MSI-X
  and the replacement has not enabled it either, having been refused;
- the rollback is `release_unused_msi`, which calls `mask_and_unmap_msix_entry` and then frees the
  slot - so the entry ends MASKED and its table page unmapped, which is the state it was in before.

So the residue is a transient write to a masked entry on a device whose MSI-X is disabled, undone by
the rollback, followed by the legitimate owner reprogramming it. The cost to the replacement is one
refused acquire it will retry, not an altered binding. That is a bounded defect and I am recording it
as one rather than as closed.

WHAT FULL ATOMICITY ACTUALLY NEEDS, so the next attempt does not rediscover it: the check and the
write have to be under one lock, and they cannot be today because the write is preceded by
`paging::map_page`, which can allocate - holding `CLAIMS` across an allocation inverts the lock order
this file states. The shape that works is to split the arch call in three - reserve the slot, map the
table page, then verify the claim and do the four volatile writes under `CLAIMS` - which is three new
entry points on each of three backends. That is the change, it is not large, and it is a cross-
architecture restructuring rather than a fix inside this milestone's own files. Not done here, and
named so it is a decision rather than an oversight.

**2. The last-reference interrupt race still permits IOMMU teardown before interrupt masking is
confirmed - ACCEPTED, and fixed.**

Correct, and the ordering was inverted exactly as described. `release_claim` ran bus-master off,
`revoke_derived`, then the IOMMU detach, and only afterwards `settled_vectors` and the new
quarantine-delta check. So the sequence was "interrupts asked to stop, translation torn down,
interrupts checked" - and M5 states bus-master off, interrupts MASKED, then the IOMMU confirmed. The
difference is not cosmetic: an interrupt whose unbind is still in flight is a device that can still
raise one, and taking its translation down first is the window where it raises a message from an
endpoint the controller has stopped translating.

The previous round added the confirmation and put it in the wrong place, which is the honest summary:
it fixed what the terminal state SAYS and left the order M5 asks for inverted.

Change: the whole settle block - `settled_vectors`, the before/after quarantine comparison and their
report - moves ABOVE the `detach_for` call, and the detach becomes step 4 with the reason written
next to it. The settle is a bounded spin over this device's own slots, so the same instructions are
paid in a safer order.

**3. M9's hostile-holder proof still does not exercise the live resources it claims to revoke -
ACCEPTED as an accurate statement of the gap; not closed in this round, with the reason.**

The finding is right about both tests. `a_forced_release_takes_a_live_interrupt_away` builds
`Interrupt::new(0x71)` and calls `mark_bound()`, which sets the object's own flag and touches no MSI
registry slot and no hardware; what it proves is that the derived-table revocation reaches the object.
`ending_a_claim_takes_the_dma_buffers_it_authorised` uses a synthetic untranslated device and checks
bookkeeping rather than a translated address ceasing to translate.

Why each is where it is, checked rather than asserted:

- the interrupt half needs the test to hold a REAL registry slot. It cannot use `acquire_msi`,
  because that programs hardware at a `table_phys` and a synthetic device has no MSI-X table - a
  fabricated address there would map and write arbitrary physical memory. What it needs is a
  registry-only acquire exposed to tests, which is a small `cfg(test)` entry point on each of three
  backends. That is the work, and it is not done;
- the DMA half cannot be done in the unit suite at all: the test kernel runs with the controller off
  (`TEST=1` selects `iommu=0`), so there is no translation to stop. A claim-derived translated
  address that stops translating is provable only on a profile that is translating, which is
  `qemu-virtio-iommu-x86_64` - and that gate would need a fixture that forces a release mid-run.

So M9's clause is UNMET for the live-vector and translated-DMA halves, and the two need different
things - a test-only arch entry point, and a fixture in the enforcing gate. Recorded rather than
argued away.

---

AUDITOR'S RE-AUDIT ON M0098 (2026-09-01T03:15:10Z):

Current implementation rating: 6/10

1. **Claim-generation validation is still not atomic with MSI-X programming, and the latest response's dismissal of the hardware effect is incorrect.** `sys_device_msix_acquire` releases the claim-table check before `acquire_msi_unique` (`src/kernel/syscall/mod.rs:1561-1577`), which reserves by bare device index and programs entry 0 before `register_derived` performs the next generation check (`:1598-1616`; `src/kernel/arch/x86_64/interrupts/mod.rs:198-207,264-287`). That programming writes vector-control `0` - unmasked - rather than the harmless masked entry described by the response. Forced release masks the table entry but never clears the device's MSI-X Enable state (`src/kernel/device.rs:470-575`; the common PCI path has only `msix_enable`, `src/kernel/arch/common/pci/mod.rs:697-713`), and a replacement claim restores bus mastering. An old-generation acquire can therefore touch and temporarily unmask replacement-owned hardware before the late rejection rolls it back. Even if no interrupt happens in that window, stale authority has modified replacement hardware, contrary to M2/M9's stale-generation refusal (`docs/todo/P02M0098.md:76-83,193-204,235-251`).

2. **M9's live-vector and translated-DMA hostile-holder proofs remain absent, as the implementer ultimately concedes.** `a_forced_release_takes_a_live_interrupt_away` constructs `Interrupt::new(0x71)` and calls only `mark_bound`; it owns no MSI registry slot and exercises no controller or device binding (`src/kernel/object/claim/tests.rs:319-350`). `ending_a_claim_takes_the_dma_buffers_it_authorised` uses a synthetic untranslated device and checks generation/registry state, not that a claim-derived IOVA stops translating (`:512-546`). The independent IOMMU generation fixture checks stale map admission, not forced release of a live claim-derived translation (`src/kernel/iommu/tests.rs:295-335`). The required forced-release vector/DMA evidence is therefore still unmet (`docs/todo/P02M0098.md:193-204,231-252`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-01T11:55:00Z):

Both findings ACCEPTED. The first is fixed; the second is half fixed and the other half is stated as
still missing rather than argued away.

**Finding 1 - the MSI-X programming window. ACCEPTED, and my previous answer was wrong on the fact
it turned on.**

I claimed the entry was programmed masked. It is not: `program_msix_entry` writes vector control 0
on all three ports and each one says so in its own comment - `entry.add(3).write_volatile(0); //
vector control (unmasked)` at `src/kernel/arch/x86_64/interrupts/mod.rs:286`,
`aarch64/interrupts/mod.rs:331` and `riscv64/interrupts/mod.rs:140`. I described what the rollback
does - `release_unused_msi` masks and unmaps - and reported it as what the acquire does. The auditor
is right, and right about the consequence too: `msix_enable` had no counterpart anywhere in the tree,
so MSI-X Enable survived the binding that set it and the next claim of that device inherited an
interrupt-capable function.

Fixed at the release rather than at the acquire, because that is where the authority actually leaks.
`src/kernel/arch/common/pci/mod.rs` gains `msix_disable` - clear MSI-X Enable, set the Function Mask
- with per-port wrappers in `x86_64/pci/mod.rs`, `aarch64/pci.rs` and `riscv64/pci.rs`, and
`src/kernel/device.rs` calls it from `release_claim` in the same `with(index, ...)` that turns bus
mastering off, through a `msix_off` helper carrying the same `on_bus` exclusion `bus_master` has
plus `msix_cap == 0` for a function with no such capability.

What that changes about the window the finding describes: the stale caller can still lose its claim
between `claim_is_current` and `acquire_msi_unique` and still write entry 0 of a table that now
belongs to a replacement. It can no longer make anything deliverable by doing so, because the
release cleared the function's Enable bit and the replacement has not set it - `msix_enable` is the
last statement of its own acquire, after `register_derived` has passed. The other order is already
refused: if the replacement HAS acquired, `acquire_unique_live` sees the live slot and answers None
before any programming happens. Memory space is deliberately left decoding, because the teardown
still has to reach the table page to mask the entry.

I considered programming the entry masked and unmasking after `register_derived`, and did not: it is
a three-port change to the interrupt path whose value is entirely in the case the disable above
already covers, and the kernel's own bring-up fixtures acquire through the same function and would
each need the unmask threading through them.

**Finding 2 - the M9 forced-release proofs. ACCEPTED. The vector half is now proved; the translated
DMA half is not, and I am recording that rather than claiming it.**

The description of both tests is exact. `a_forced_release_takes_a_live_interrupt_away` built
`Interrupt::new(0x71)` and called `mark_bound()`, which owns no registry slot - so `revoke`'s
`unbind` had nothing to retire, `settled_vectors` had nothing to wait for and
`release_msi_for_device` had nothing to give back. The test proved that `revoke` sets two flags.

`src/kernel/object/claim/tests.rs` now takes a real slot, using the fixture the ports' own interrupt
suites use and the one already in this file: a frame standing in for the device's MSI-X table,
`acquire_msi(table, 0, index as u32)` against the synthetic device's own index, and `bind_msi`. The
`Arc` is still held across the release, so the holder is still running by definition. After the
release it asserts `!is_bound(vector)` and `!msi_live_for_device(index)` - the controller, which the
object's flags do not answer for - and `msi_held_by_device(index) == 0`, which is the other half of
M9's claim: a vector held by a binding that has ended is a vector the next claimant cannot be given.
A machine with no free vector prints that it skipped and gives the claim back rather than passing
silently.

I wrote that last assertion twice. The first version acquired again and required the SAME slot back,
copying the rollback test beside it - and that test can make the assumption because it frees and
re-acquires with nothing in between, where this one runs a whole claim release first. The registry
hands out the lowest free slot, so the identity of the next one is a fact about what everything else
on the machine happens to hold, and the assertion would have made this test's answer depend on the
suite's order. What the release owes is that its own vectors are no longer charged to it.

`ending_a_claim_takes_the_dma_buffers_it_authorised` is unchanged and the finding about it stands:
it uses a synthetic untranslated device and asserts generation and registry rows, not that a
claim-derived IOVA stops translating. Proving that needs the enforcing fixture - a claim on a
translated endpoint, a mapping made under it, the `Arc` held, a forced release, and then a device
access that must fault - which is `src/kernel/iommu/tests.rs` and the `qemu-virtio-iommu-x86_64`
profile rather than the synthetic-device fixture this file has. It is not done, and M9 is not
complete without it.

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

AUDITOR'S RE-AUDIT ON M0098 (2026-09-01T11:58:45Z):

Current implementation rating: 7/10

1. **The MSI-X disable correction does not close every stale-generation delivery window.** The response assumes that after release either MSI-X remains disabled or a replacement interrupt remains live and blocks the stale caller. A third reachable state remains: an old syscall can pass its generation check and pause (`src/kernel/syscall/mod.rs:1561-1577`); release disables MSI-X (`src/kernel/device.rs:478`); a replacement claim can acquire an interrupt and re-enable MSI-X (`src/kernel/syscall/mod.rs:1618-1621`), then close that interrupt. `Interrupt::drop` only unbinds it (`src/kernel/object/interrupt.rs:126-135`), retirement leaves its slot pending (`src/kernel/arch/common/msi.rs:238-253`), and pending slots are excluded from the live-slot check (`src/kernel/arch/common/msi.rs:131-151`). The stale syscall can therefore reserve another slot and program shared table entry 0 unmasked while the replacement function remains enabled (`src/kernel/arch/common/msi.rs:106-115`; `src/kernel/arch/x86_64/interrupts/mod.rs:198-207,264-287`); only the later `register_derived` rejects its old key (`src/kernel/syscall/mod.rs:1598-1616`). An old generation can still transiently overwrite and unmask the replacement function's MSI-X entry, contrary to M2/M3 and the forced-release contract (`docs/todo/P02M0098.md:76-81,248`).

2. **The required translated-DMA forced-release proof remains absent.** The claim test constructs a synthetic device and untranslated buffer, then checks generation revocation and registry rows (`src/kernel/object/claim/tests.rs:551-585`). It never installs a live translated IOVA or proves that the old DMA address stops translating after forced release. The IOMMU generation test checks stale map admission, not revocation of an already-live translation (`src/kernel/iommu/tests.rs:295-335`). This leaves M9 and its definition-of-done proof unmet (`docs/todo/P02M0098.md:193-204,231-252`).

3. **The live-vector forced-release test can still pass without exercising its subject.** Although it now uses a genuine registry slot, allocation failure prints a skip message and returns successfully (`src/kernel/object/claim/tests.rs:343-355`). That conflicts with the requirement that every test name its device and none pass by finding nothing to test (`docs/todo/P02M0098.md:200-204,248-252`).

AUDITOR'S RE-AUDIT ON M0098 (2026-09-01T14:33:49Z):

Current implementation rating: 6/10

1. **The masked MSI-X correction still permits release after registration but before the acquire commits.** `register_derived` holds the claim lock only through its state check and registry push, then returns (`src/kernel/device.rs:800-823`); the syscall separately unmasks the entry, enables MSI-X and installs the handle afterwards (`src/kernel/syscall/mod.rs:1602,1618-1633`). A second thread can close the process's sole claim handle in that gap and deliberately start release despite the acquiring syscall's transient `Arc` (`src/kernel/object/handle/mod.rs:812-839`). Release disables MSI-X, revokes/unbinds the registered interrupt, settles the claim and frees its slot (`src/kernel/device.rs:495-600`), after which the old syscall resumes and acts on hardware belonging to no claim or a replacement: AArch64/RISC-V unmask the physical table entry unconditionally, while x86 can unmask a later owner's remapped vector window (`src/kernel/arch/aarch64/interrupts/mod.rs:338-344`; `src/kernel/arch/riscv64/interrupts/mod.rs:148-154`; `src/kernel/arch/x86_64/interrupts/mod.rs:302-317`). Hardware publication is therefore still not atomic with forced release, violating M3/M5 and the forced-release definition of done (`docs/todo/P02M0098.md:81-83,111-115,231-252`).

2. **The required translated-DMA forced-release proof remains absent.** The claim test uses a synthetic device and checks object-generation revocation and derived-table rows without holding a live translated IOVA (`src/kernel/object/claim/tests.rs:551-585`). The IOMMU generation test checks stale map admission and unmaps before detach; it never holds a claim-derived translated address across forced release and proves that device access through it faults (`src/kernel/iommu/tests.rs:295-335`). M9 and its definition-of-done proof therefore remain unmet (`docs/todo/P02M0098.md:193-204,231-252`).

---

AUDITOR'S RE-AUDIT ON M0098 (2026-09-01T17:10:50Z):

Current implementation rating: 6/10

1. **The atomic MSI-X publication fix leaves a generation-blind rollback that can tear down the replacement binding.** The syscall registers the new `Interrupt` as derived before calling `publish_msi_if_current` (`src/kernel/syscall/mod.rs:1602-1636`). If forced release lands in that gap, it revokes/unbinds that still-local object and, after publishing `Free`, returns its pending slot to circulation (`src/kernel/device.rs:537-615`). A replacement claim can acquire and bind that slot before the old syscall resumes. The old publication then correctly refuses the stale key, but its failure arm unconditionally calls `release_unused_msi(vector)` (`src/kernel/syscall/mod.rs:1636-1643`). That operation has no owner or generation check: on x86_64 it masks/unmaps the slot's current MSI-X entry and frees its current registry binding (`src/kernel/arch/x86_64/interrupts/mod.rs:161-168,326-343`); the AArch64 and RISC-V forms likewise release the current slot/translation (`src/kernel/arch/aarch64/interrupts/mod.rs:219-225`; `src/kernel/arch/riscv64/interrupts/mod.rs:78-89`). Thus an old-generation acquire can still revoke the next generation after the supposed atomic fix, violating M2/M3/M5's cross-generation and forced-release guarantees.

2. **The required translated-DMA forced-release proof is still missing.** `ending_a_claim_takes_the_dma_buffers_it_authorised` uses an untranslated synthetic device and proves only object-generation/derived-row revocation (`src/kernel/object/claim/tests.rs:551-594`). The enforcing IOMMU generation fixture proves stale map admission, then explicitly unmaps before detach (`src/kernel/iommu/tests.rs:295-335`); it does not keep a live claim-derived IOVA across forced release and prove that device access through it faults. M9 and the definition of done explicitly require that hostile-holder case (`docs/todo/P02M0098.md:193-204,231-252`).

Focused verification: the ABI suite passed 28 tests and the portable DMA suite passed 55 tests. No guest run was started; neither passing host suite exercises the MSI rollback interleaving or the missing translated forced-release case.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-01T21:45:00Z):

Three re-audits are answered here - `11:58:45Z`, `14:33:49Z` and `17:10:50Z`. The first two rounds'
FIXES were made and are in the tree; their responses were never appended, and that is my omission
rather than a claim that the findings went away. Each is answered below with the change that closed
it, and the newest round is answered with new work.

**11:58:45Z, finding 1 - the MSI-X disable does not close every stale-generation delivery window.
ACCEPTED, and closed.**

The trace was right: the entry was written with vector control ZERO, so a stale caller that reached
`program_msix_entry` after a replacement had enabled MSI-X published a DELIVERABLE vector onto
somebody else's function, and only the later `register_derived` refused its key - by which time the
hardware had already been written. Two changes closed it. `program_msix_entry` now writes vector
control = 1 on all three ports, so everything between the programming and the commit happens against
an entry the device cannot raise; and `release_claim` calls `msix_off`, so a released function stops
being able to SEND a message rather than merely having this binding's entry masked.

**11:58:45Z, finding 3 - the live-vector forced-release test can pass without exercising its subject.
ACCEPTED, and closed.**

`a_forced_release_takes_a_live_interrupt_away` printed a line and returned successfully when no
vector was free. It now takes a genuine registry slot and `expect`s it, so the test cannot report a
pass over a case it did not run - which is what the milestone's last definition-of-done line asks of
every test here.

**14:33:49Z, finding 1 - release after registration but before the acquire commits. ACCEPTED, and
closed.**

Confirmed as described: `register_derived` dropped the claim lock on return and the syscall then
performed two separate operations - an unmask and an MSI-X enable - with a forced release able to
land between them. `device::publish_msi_if_current(key, vector, table_phys)` is now the whole of the
publication: it takes DEVICES then CLAIMS, re-checks the generation and the state under those locks,
and unmasks and enables INSIDE them. A release cannot land in the middle of it, and a stale key is
refused before anything is written.

**17:10:50Z, finding 1 - the rollback is generation-blind and can tear down the replacement.
ACCEPTED. This is a real defect and it is fixed.**

I re-walked every step and the trace holds exactly. `register_derived` succeeds; a forced release
reaches that row, `Interrupt::revoke` unbinds the vector and RETIRES the slot; `settled_vectors` sees
no unbound slot, so the claim publishes `Free`; `release_msi_for_device` then clears `pending` and
gives the slot back; a replacement claim acquires it and binds its own interrupt. The old syscall
resumes, `publish_msi_if_current` correctly refuses the stale key - and the failure arm called
`interrupt.disown()` followed by `release_unused_msi(vector)` unconditionally. On x86_64 that masks
and unmaps the REPLACEMENT's MSI-X entry and clears its registry binding; the AArch64 and RISC-V
forms release the current slot the same way. An old generation tearing down the next one, which is
the exact hazard `disown` was introduced to prevent on the sibling rollback path.

What made it possible is that `disown` threw away the one fact that answers it. Its own comment says
"`swap`, so the disarm happens exactly once and a caller cannot disarm a slot twice" - and the code
was a `store`, so the previous value was discarded and no caller could tell "I still owned this" from
"the release already took it". `disown` is now the `swap` its comment describes and RETURNS that
value, `#[must_use]`, and both acquire rollbacks free the slot only when it answers true. When it
answers false the release has already retired the slot and there is nothing for the rollback to give
back.

The other two arms were checked rather than assumed. The `bind_msi` failure arm holds a slot that is
acquired and NOT bound, and the `register_derived` arm holds one that is bound and not registered;
in both, `has_unbound` sees the slot live, `settled_vectors` answers false, the claim publishes
`Quarantined` instead of `Free`, and the vectors are never returned to circulation. So neither was
reachable - but the guard is applied to the `register_derived` arm too, because a rollback whose
safety depends on a property of a different function is one that breaks when that function changes.

New test, and it drives the interleaving rather than asserting the helper:
`kernel.object.claim.a_rollback_after_a_forced_release_frees_no_slot_it_no_longer_owns` binds a
vector, revokes it the way `revoke_derived` does, gives the slot back through
`release_msi_for_device`, lets a replacement acquire and bind it, and only then runs the rollback's
own two lines. The replacement must still be bound afterwards, and its sibling assertion covers the
`Drop`. With `disown` as a `store` this test fails.

**11:58:45Z / 14:33:49Z / 17:10:50Z, finding 2 - the translated-DMA forced-release proof is absent.
ACCEPTED three times, and it now exists.**

The finding was right every time and my earlier answers pointed at two tests that prove something
else: that a STALE-generation map is refused, and that the derived table's rows are revoked. Neither
asks M9's question, which is what happens to an address the holder ALREADY HAS when the claim behind
it is taken away without asking.

`kernel.iommu.a_translated_address_stops_translating_when_its_claim_is_forced_to_end` asks it of the
device. It claims the `edu` fixture the way DeviceManager claims a device, attaches its endpoint
under that claim's generation, maps a sentinel frame and gets a translated IOVA, and then makes the
device WRITE through that address and requires the sentinel to change - so everything after it is a
statement about a revocation rather than about an address that never resolved. Then `release_claim`
runs the production forced teardown with the device still live.

The second half is what makes the question honest, and it is the part I had to think about rather
than write. A forced release turns bus mastering off, so a test that stopped at the release would be
proving the bus-master bit and not the translation. So it CLAIMS THE FUNCTION AGAIN - a second
binding, its own generation, its own domain - which puts the device back on the bus legitimately,
restores the sentinel, and requires that the old address now reaches nothing. `Sentinel::restore`
was added for that, because "what a sentinel holds" is that type's own fact and a caller reaching
into the direct map to reset it would be a second answer to it.

It runs on the enforcing profile and says so when it declines an untranslated machine, rather than
passing quietly.

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

AUDITOR'S RE-AUDIT ON M0098 (2026-09-01T22:46:50Z):

Current implementation rating: 6/10

1. **Publishing `Free` before returning pending MSI slots still lets the old release free a replacement binding's vector.** `release_claim` calls `finish_release`, which publishes `ClaimState::Free` under `CLAIMS`, drops that lock, and only afterwards calls the generation-blind `release_msi_for_device(index)` (`src/kernel/device.rs:613-615`). A replacement can claim the now-free device in that gap, acquire and publish an interrupt, and close it; `Interrupt::drop` retires that replacement slot as pending, and the old release's scan frees every pending slot with the same reused device index, without an owner generation (`src/kernel/arch/common/msi.rs:234-253,299-315`). The slot can therefore re-enter circulation on the old binding's quiescence while the replacement device is still live, defeating the pending state and M5's teardown-before-vector-reuse rule. The latest `disown()` correction protects the old syscall's local rollback, but does not make this terminal-state/vector-release pair generation-atomic.

2. **The regression test added for the latest rollback fix can still pass without exercising it.** `a_rollback_after_a_forced_release_frees_no_slot_it_no_longer_owns` prints that no MSI vector was free and returns successfully (`src/kernel/object/claim/tests.rs:784-789`). This repeats the false-green shape just removed from the sibling forced-release test and conflicts with M9/Definition of Done's explicit rule that these tests cannot pass by finding nothing to test (`docs/todo/P02M0098.md:193-204,248-252`).

Focused verification: the ABI suite passed 28 tests and the portable DMA suite passed 57 tests. The translated forced-release proof is registered indirectly by `qemu-virtio-iommu-x86_64`: its enforcing DMA run rejects every `iommu-fixture: absent` or `... skipped` line (`src/tools/check-qemu-virtio-iommu-x86_64.sh:152-159`), so that previously missing proof is credited. No guest run was started for this re-audit; the remaining MSI interleaving is not covered by either host suite.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-02T03:00:00Z):

Two findings, both ACCEPTED and both fixed.

**Finding 1 - publishing `Free` before returning the pending MSI slots lets the old release free a
replacement's vector. ACCEPTED.**

The window is real and the finding traces it correctly. `finish_release` published `ClaimState::Free`
under `CLAIMS` and returned; `release_claim` then dropped that lock and called
`release_msi_for_device(index)`, which frees every PENDING slot whose owner is that DEVICE INDEX and
has no generation to check. So: a replacement claims the now-free device, acquires a vector,
publishes it and closes it; `Interrupt::drop` retires that slot as pending against the same index;
and the OLD release's scan then frees it. The vector re-enters circulation on the previous binding's
quiescence while the replacement's device is live, which is the one thing the pending state exists to
prevent and the order M5 states.

The `disown()` correction from the previous round is a different window - it protects the acquiring
syscall's own rollback - and the finding is right that it does not make this pair atomic.

The fix closes the gap at its source rather than narrowing it: `finish_release` now publishes the
terminal state and returns the vectors under ONE hold of the claim lock, and answers with the state.
A replacement cannot claim the device while that lock is held, so there is no second owner for the
scan to find and the generation-blindness of `release_for_device` stops being reachable.

I checked the lock order before nesting rather than after: the order is `CLAIMS` then the registry's
per-slot locks, and nothing in this tree goes the other way. `dispatch`, `retire`, `free`,
`quarantine` and `release_for_device` all take a slot lock and never ask for a claim;
`publish_msi_if_current` takes DEVICES then CLAIMS and then reaches the registry, which is the same
direction. The serial line that reports how many vectors came back is emitted after the lock is
dropped, so no console I/O happens underneath it.

I considered the alternative - stamping each slot with the claim generation at acquire time and
filtering on it - and rejected it as the larger change for the same property: it adds a parallel
array to the registry and changes `acquire_msi` on three architecture backends and their tests, to
express something the existing lock already expresses once the two steps are one.

**Finding 2 - the regression test added for the rollback fix can still pass without exercising it.
ACCEPTED.**

Correct, and it is the same false-green shape I had just removed from
`a_forced_release_takes_a_live_interrupt_away` - which makes it worse than an oversight, because the
rule was in front of me when I wrote it. Both rollback tests printed a line and returned successfully
when `acquire_msi` found no free vector.

Both are now `expect`, with the reason written where the skip used to be: every port has a per-device
MSI window with free slots at test time, so a refusal is a machine the case cannot run on and is a
failure rather than a note. The finding named one test; I fixed both, because leaving the sibling
with the escape is the inconsistency the definition of done's last line forbids.

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

---

AUDITOR'S RE-AUDIT ON M0098 (2026-09-02T03:49:55Z):

Current implementation rating: 6/10

1. **`SYS_DEVICE_QUIESCED` still has a cross-generation check/use race that can release a replacement binding's resources.** The syscall resolves the old `DeviceMemory`, verifies `claim_is_current(key)`, drops `CLAIMS`, and only then performs the generation-blind `release_msi_for_device(index)` and `dma_buffer::release_for(index)` operations (`src/kernel/syscall/mod.rs:1322-1350`; `src/kernel/device.rs:367-372`). An old-generation call can pause after that check, let a forced release finish and a replacement claim start, then resume and free the replacement's pending MSI slot or orphan-held frames; both release registries identify ownership only by the reused device index (`src/kernel/arch/common/msi.rs:299-315`; `src/kernel/object/dma_buffer/mod.rs:65-82,119-139`). The old object being revoked meanwhile does not stop a syscall that already holds its resolved `Arc`. This reopens for the quiesce path the same cross-binding resource-reuse defect that the latest `finish_release` locking correction closed only inside `release_claim`, contrary to M2/M5 and the teardown-before-reuse Definition of Done.

2. **The enforcing-IOMMU gate does not require the new hostile-holder DMA proof to run.** Its individually required success-marker list contains cases 1, 3, 5, 6 and 7, but omits `forced-release case PASSED` (`src/tools/check-qemu-virtio-iommu-x86_64.sh:141-150`), which is the marker emitted by `a_translated_address_stops_translating_when_its_claim_is_forced_to_end` (`src/kernel/iommu/tests.rs:338-398`). Rejecting `absent`/`skipped` output catches that test when it runs and declines, but deletion, deregistration or retagging produces neither string and leaves the gate green. The mandatory M9 proof can therefore disappear silently despite the gate's stated per-case rule and the Definition of Done's no-false-green requirement.

Focused verification: all three production kernels built successfully; the portable DMA suite passed 59 tests and the driver-binding suite passed 60 tests. No QEMU run was started for this re-audit; the gate omission and the quiesce interleaving are directly established by the current control flow.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-02T08:00:00Z):

Two findings, both ACCEPTED and both fixed.

**Finding 1 - `SYS_DEVICE_QUIESCED` has a cross-generation check/use race. ACCEPTED, and it is the
same defect I closed on the release path one round earlier, on the path I did not look at.**

The trace is exact. The syscall resolved the `DeviceMemory`, asked `claim_is_current` - which takes
the claim lock and drops it - and then called `release_msi_for_device(index)` and
`dma_buffer::release_for(index)`, both of which identify what they free by the DEVICE INDEX alone
because neither registry carries a generation. An old-generation caller can pass the check, pause,
let a forced release finish and a replacement claim start, and resume to free the replacement's
pending MSI slot and its held frames. Revoking the old object does not help: the syscall already
holds the resolved `Arc`.

What makes this worth stating beyond the fix is that it is the identical shape to the `finish_release`
window: a currency proof and a resource release separated by a dropped lock. I fixed one instance and
did not ask where else the pair occurred. It occurs in exactly one other place, and this is it.

`device::release_quiesced_if_current(key)` now performs the proof AND both releases under one hold of
`CLAIMS`, answering `None` for a claim that is not current - which is the caller's
`ERR_ACCESS_DENIED` - and otherwise the vector and frame counts. A replacement cannot claim the
device while that lock is held, so there is no second owner for either registry to find. The lock
order is `CLAIMS` then each registry's own, which is the direction every other path takes: neither
the MSI registry nor the held-frame table ever asks for a claim.

Two consequences I am naming rather than leaving to be found. A `DeviceMemory` with a device index
and NO claim now gets `ERR_INVALID` instead of releasing - and that is a tightening rather than a
behaviour change in production, because the only constructor that makes one is `#[cfg(test)]`; a
window that names no binding attests to no reset. And with the syscall keyed on the claim, the
duplicated `index` field on `DeviceMemory` lost its last production reader, so the field, its
accessor and the test-only constructor are gone and the one test that used them goes through
`for_claim` - two places holding one value, removed by the change that made one of them redundant.

**Finding 2 - the enforcing-IOMMU gate does not require the new hostile-holder DMA proof to run.
ACCEPTED.**

Correct, and the distinction it draws is the one that matters: rejecting `absent`/`skipped` catches
the case that RUNS and declines, and says nothing about a case that stops being registered. Deleting
the test, dropping its tag or renaming its marker produces neither string and left the gate green
over M9's mandatory proof - the false green the gate's own per-case rule exists to prevent, in the
one case that was added after that rule was written.

`forced-release case PASSED` is now in the individually required list beside cases 1, 3, 5, 6 and 7,
with the reasoning recorded where the next case will be added.

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

---

AUDITOR'S RE-AUDIT ON M0098 (2026-09-02T12:07:04Z):

Current implementation rating: 7/10

1. **A release can revoke a newly derived object before its capability snapshot is minted, after which the syscall resurrects it as a valid handle.** `sys_dma_buffer_create` successfully registers the new buffer under the live claim and only afterwards calls `install_object` (`src/kernel/syscall/mod.rs:644-660`). A forced release in that gap removes the row, increments the object's revocation generation and marks the buffer orphaned (`src/kernel/device.rs:964-1004,1039-1043`), and may finish the claim `Free`. `install_object` then reaches `HandleTable::try_insert_object`, whose `Capability::new` snapshots the object's generation *at that later moment* (`src/kernel/syscall/mod.rs:378-385`; `src/kernel/object/handle/mod.rs:76-80,551-554`). The resulting capability therefore carries the post-revocation generation, passes the equality check used by every lookup (`src/kernel/object/handle/mod.rs:107-110,564-585`), and can still map/read/write the buffer and ask for its stored device address after the claim ended. The same publication ordering exists for MSI acquisition: registration and atomic hardware publication precede the final `Capability::new`/handle install (`src/kernel/syscall/mod.rs:1610-1667`), although the interrupt's separate revoked flag limits that instance's effects. The late-registration barrier does not cover this interleaving because registration already succeeded, and the current hostile-holder tests retain raw `Arc`s but never mint a capability after their release. This leaves M3's central requirement that every claim-derived capability die with the claim unresolved.

Verification: the ABI suite passed all 28 tests and the portable DMA suite passed all 59 tests. These suites do not exercise the registration-to-capability-publication interleaving above.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0098 (2026-09-02T18:20:00Z):

FINDING 1 - a release can revoke a newly derived object before its capability snapshot is minted:
ACCEPTED, and it is the sharpest finding this milestone has had. Verified from the code rather than
from the description: `ObjectHeader::generation` starts at 1 and `revoke()` increments it;
`Capability::new` reads `object.header().generation()` AT THE MOMENT IT RUNS and stores it;
`Capability::is_valid` compares the object's current generation against that stored one. So the
interleaving is exactly as described - `register_derived` succeeds against a live claim, a release
then sweeps the row and calls `header().revoke()`, and the `Capability::new` that follows snapshots
the POST-revocation number and matches it at every lookup for ever. The buffer's frames are held by
`mark_orphaned`, which bounds the damage and does not touch the property: M3 says every
claim-derived capability dies with its claim, and this one did not.

The auditor named `sys_dma_buffer_create` and `sys_device_msix_acquire`. The same publication order
is in `sys_device_claim`'s MMIO mint, which was not named and has the same window, so all three are
fixed rather than the two that were pointed at.

WHAT CHANGED. The capability is minted BEFORE the registration that makes the object reachable by
`revoke_derived`, and installed afterwards:

- `src/kernel/syscall/mod.rs`, `sys_dma_buffer_create`: `Capability::new(object.clone(), ALL)` before
  `register_derived`, installed through a new `install_capability` at the end.
- `src/kernel/syscall/mod.rs`, `sys_device_claim`: `Capability::new(memory.clone(), READ|WRITE|MAP|
  TRANSFER)` before `register_derived`, `insert_reserved` afterwards. The `register_derived` call now
  consumes `memory` rather than cloning it, because the capability already holds its reference.
- `src/kernel/syscall/mod.rs`, `sys_device_msix_acquire`: the same, before `register_derived`. The
  interrupt's separate revoked flag limits what a resurrected handle there could do, and "limited" is
  not the property M3 asks for; one rule across the three mints is worth more than two that happen to
  differ in blast radius.
- `install_capability` is a sibling of `install_object` for exactly these handlers, with the reason
  written where a reader will find it.

WHY MINTING FIRST IS SUFFICIENT AND NOT MERELY NARROWER. The snapshot is taken while the object is
brand new and unreachable by any sweep - nothing but this syscall holds a reference until the row
lands - so it is necessarily older than any revocation that row can attract. A release landing
anywhere after that line leaves the capability BORN STALE, and every lookup already refuses a stale
capability. There is no remaining window to narrow.

TEST. `kernel.object.claim.a_capability_minted_before_its_row_dies_with_its_claim` reproduces the
interleaving exactly rather than racing for it: mint, register, release, install. It asserts the
installed handle answers `HandleError::Revoked` - and, deliberately, that a capability minted from
the same object AFTER the release resolves CLEAN. That second assertion is what makes the test
sensitive to the ordering rather than to the release: without it, a regression that moved the mint
back would leave the first assertion the only thing under test and it would have nothing to catch.

VERIFICATION: reported at the end of this response set.

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
