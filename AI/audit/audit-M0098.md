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
