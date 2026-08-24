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
| Distinct states | 12660 |
| Search depth | 25 |
| Wall clock | 2.5 s, one worker |
| Peak resident | 650 MiB (the JVM's default heap on this machine, not a measured requirement) |
| `Transfer.tla` | `fa9b382edc90d029…` |
| `Capability.tla` | `2ba66b6734ec973e…` |
| `spike.cfg` | `e7ec796cb7a9b596…` |

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
| Distinct states | 607859 |
| Search depth | 30 |
| Wall clock | 54 s, one worker |
| Peak resident | 2.1 GiB |
| `handles.cfg` | `dc4a6a3cc5c6dd1c…` |

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
| Distinct states | 25320 |
| Search depth | 26 |
| Wall clock | 3.4 s, one worker |
| Peak resident | 1.0 GiB |
| `revoke-test-only.cfg` | `c258c64577626888…` |

### Watched to fail

`RevokedSnapshotCannotOperate` is stated over the GUARD - "a capability the object has outlived is
not usable" - rather than over an outcome, so that a mutation which drops the generation check fails
it instead of passing quietly. Removing `cap.objgen = objgen` from `Usable` makes TLC report the
violation in two states: `Init`, `Revoke`. Restored, the configuration passes. An invariant that has
not been seen to fail is a sentence, not a check.

## `transactions-batch.cfg`

The spike with ONE lever moved: `BatchMax = 2`. It is the smallest configuration in which "a refused
send returns ALL of them" is a different sentence from "a refused send returns it" - one capability
cannot show an all-or-nothing rule - and it is what `FailedSendRestores` is checked against.

| | |
| --- | --- |
| Result | model checking completed, no error |
| Distinct states | 51216 |
| Search depth | 25 |
| Wall clock | 6.1 s, one worker |
| Peak resident | 1.6 GiB |
| `transactions-batch.cfg` | `7b9ffe65d576aca4…` |

### What the batch cost to model, and what that cost bought

Turning one transfer-local capability into a SEQUENCE found two rules that were true of the code and
absent from the model, both of them about when a take may happen:

- A take may not start a second batch after the message is queued. The capabilities have left but
  their slots are still reserved, so appending there produced a capability in hand whose slot nobody
  was holding - and TLC found it by indexing past the end of a sequence rather than by an invariant,
  which is the model being wrong rather than the kernel.
- A take is bounded by the RESERVATIONS outstanding, not by what is in hand. The syscall takes every
  capability it is going to send and then sends them; it never takes another after the send.

## Configurations not yet committed

`propagation.cfg` and `transactions-single.cfg` are the rest of P02M0154 M3, and each is one measured change from one of these rather than a
speculative widening in every dimension at once.
