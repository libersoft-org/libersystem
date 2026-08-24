# What each configuration costs, and what it covered

A configuration's numbers are part of its result. A later run that explores fewer states is a
DIFFERENT result and may not quietly replace a committed one - which is why the digests of the
specification and the configuration are recorded beside the counts.

## `spike.cfg`

The smallest configuration that can show a transfer racing a close: two processes, one transferable
object, two slots each, a queue that holds one message, two message identities, and a generation
ceiling of two so RETIREMENT is reachable inside the search rather than being a branch no state
takes.

| | |
| --- | --- |
| TLC | 2.19 of 08 August 2024 (rev 5a47802), from the JAR pinned in `toolchain.lock` |
| Java | OpenJDK 25.0.4.1 (the pin's floor is 11) |
| Result | model checking completed, no error |
| States generated | 76257 |
| Distinct states | 15242 |
| Search depth | 25 |
| Wall clock | 2.4 s, one worker |
| Peak resident | 650 MiB (the JVM's default heap on this machine, not a measured requirement) |
| `Transfer.tla` | `d1e1f72850da9b4e…` |
| `Capability.tla` | `06a14e7a3b280bce…` |
| `spike.cfg` | `443609df3eb2556d…` |

Checked: `TypeOK`, `TransferIsLinear`, `AuthorityNeverWidens`, `NoForgery`, `QuotaConserved`,
`QueueBounded`, `SlotOwnershipUnique`, `PostCommitCopyoutIsTerminal`, `ClosedProcessCannotResurrect`,
`StaleHandlesStayDead`, `ReceiveIsTransactional`, `MessageIdentityStable`, `TypeSealing`,
`RevokedSnapshotCannotOperate`, and the step property `GenerationsOnlyAdvance`. No fairness: every
safety invariant holds without it.

### What it found on its way to passing

THE FIRST RUN DID NOT PASS, and that is the result worth recording. `QuotaConserved` was violated in
three states - `Init`, `Book`, `Terminate` - because `close_all` rebuilds the free list from every
slot that is not `reserved`, and a BOOKED slot is not: the index landed on the free list while
`booked` still named it, and the quota `reserve` had charged was refunded by nobody. Two actions,
found exhaustively, on a path no test in the tree walks.

Modeling the fix then found three things about the MODEL rather than the kernel, which is the other
half of what a specification is for:

- `CopyoutFails` refunded every installed handle. The kernel closes them by raw number and `close`
  checks the slot's GENERATION, so one already recycled is refused rather than closed twice - the
  model now counts what was actually closed.
- `Unbook` was enabled while a delivery was in hand. `release_reservation` is reached from two
  places and neither is the middle of a delivery.
- A receive whose capabilities were all dropped - the table closed under it - could not end. The
  payload was still delivered and the count still written, so it ends.

## `handles.cfg`

The spike with ONE lever moved: the minted capability carries `DUPLICATE`, so
`HandleTable::duplicate` is reachable and the rights algebra is exercised rather than merely
defined. A second object type comes with it, because type sealing is vacuous where there is only
one type to ask for.

| | |
| --- | --- |
| Result | model checking completed, no error |
| States generated | 7626643 |
| Distinct states | 1039839 |
| Search depth | 31 |
| Wall clock | 83 s, one worker |
| Peak resident | 2.1 GiB |
| `handles.cfg` | `46077bce3d4d661f…` |

`TransferIsLinear` is NOT checked here, and that is the point of the split: it counts one authority
in one place, which is exact only while nothing duplicates. Two capabilities for one object are two
owners and not a violation. Keeping the invariant while enabling duplication would be a
configuration whose name no longer describes what it checks.

### What its first shape cost, which is why M3's rule exists

Three slots, three generations, two types and a duplicate quantified over EVERY subset of the minted
rights: 37 million states generated in six minutes with three million still queued, and killed
rather than finished. Four dimensions widened at once, none of them measured first. The committed
shape moves one - duplication - keeps the slot and generation bounds where the spike had them, and
offers a duplicate two right sets rather than eight: keep everything, and narrow to one. The middle
cases are the same two rules.

## `revoke-test-only.cfg`

`ObjectHeader::revoke` is a TEST HELPER in this tree: the production authority model has stale
handles after slot reuse and object destruction and no syscall that globally invalidates derived
capabilities. So the action exists only here, and a result from this configuration describes what
the helper does. THE PRODUCTION CLAIM MAY NOT CITE IT. The other configurations set
`RevocationModeled` false, which removes the action rather than leaving it disabled behind a guard -
the spike's state count is identical with and without the constant, which is how that is visible.

| | |
| --- | --- |
| Result | model checking completed, no error |
| States generated | 165843 |
| Distinct states | 30484 |
| Search depth | 26 |
| Wall clock | 3.7 s, one worker |
| Peak resident | 1.0 GiB |
| `revoke-test-only.cfg` | `16f196f7abbeff91…` |

### Watched to fail

`RevokedSnapshotCannotOperate` is stated over the GUARD - "a capability the object has outlived is
not usable" - rather than over an outcome, so that a mutation which drops the generation check fails
it instead of passing quietly. Removing `cap.objgen = objgen` from `Usable` makes TLC report the
violation in two states: `Init`, `Revoke`. Restored, the configuration passes. An invariant that has
not been seen to fail is a sentence, not a check.

## Configurations not yet committed

`propagation.cfg`, `transactions-single.cfg` and `transactions-batch.cfg` are the rest of
P02M0154 M3, and each is one measured change from one of these rather than a
speculative widening in every dimension at once.
