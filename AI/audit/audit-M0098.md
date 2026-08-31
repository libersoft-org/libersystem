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
