# The capability transfer model, and the code it is a model of

This is the state and atomic-action map, frozen BEFORE the specification is written.
Its purpose is to make the later TLA+ specification checkable against something other than memory -
every model variable and every model action names the Rust item it abstracts, and every place the
implementation can be interrupted between two of them is written down here rather than discovered
later as a missing interleaving.

Measured against the tree on 2026-08-24.

## What is modeled

The authority-bearing and transactional state only. The allocator, the scheduler, page tables and
the byte-copy implementation are outcomes rather than mechanisms: an allocation either succeeds or
does not, a user copy either lands or does not, and the model takes both nondeterministically.

## The handle slot, and its six states

`HandleTable` (`src/kernel/object/handle/mod.rs`) is a `Vec<Slot>` plus a free list, a booking list
and a closed flag. A slot's state is not one field - it is the combination the table maintains, and
naming the combinations is what makes them checkable:

| Model state | How the implementation represents it | Who may leave it |
| --- | --- | --- |
| `Free` | index on `free`, `cap: None`, `reserved: false` | `insert`, `try_insert`, `reserve` |
| `Live` | `cap: Some(_)`, index not on `free` | `close`, `take`, `take_for_transfer`, `close_all` |
| `TransferReserved` | `cap: None`, `reserved: true`, index NOT on `free` | `commit_taken`, `restore_taken`, `abandon_taken` |
| `Booked` | index on `booked`, `cap: None` | `insert_reserved`, `release_reservation` (via `unbook`) |
| `Retired` | `generation == u32::MAX`, index never returned to `free` | nothing - terminal |
| `Closed` (table-wide) | `closed: true` after `close_all` | nothing - terminal |

`TransferReserved` is the state that used to be unrepresentable: `cap: None` and absent
from the free list, which no caller could test and `close_all` therefore could not respect. The
model keeps it distinct because the two ways out of it - commit and restore - have different
ownership outcomes, and because a process termination may arrive while a slot is in it.

`Retired` is generation exhaustion treated as retirement rather than wraparound
(`retire_or_recycle`). The model needs a small abstract maximum generation so that boundary is
reachable inside the bounds; the implementation's is `u32::MAX`.

## The capability, and who owns it at each phase

A capability is `(object identity, object type, rights, captured object generation)` -
`Capability` plus `ObjectHeader::koid`/`generation`. There is no "in the kernel somewhere" phase:
at every point of a transfer exactly one of these owns it.

| Phase | Where the value physically is | Reached by | Left by |
| --- | --- | --- | --- |
| source slot | `slots[i].cap` | the sender's earlier `insert` | `take_for_transfer` |
| transfer-local | the syscall's `Vec<Capability>` | `take_for_transfer` | `Message::new`, or `restore_taken` |
| queued message | `Message.caps`, inside `Channel.inbox` | `send_inner`'s `push_back` | `recv_identified`, or the endpoint's drop |
| delivery-local | the receiver syscall's `Message` | `recv_identified` | `insert_reserved`, or `return_to_head` |
| destination booking | nothing yet - `booked` holds an INDEX | `HandleTable::reserve` | `insert_reserved`, `release_reservation` |
| committed, unpublished | `slots[i].cap`, raw handle not yet in userspace | `insert_reserved` | `write_user` succeeding, or `close` on the copyout failure path |
| published | `slots[i].cap`, and userspace holds the number | the last `write_user` | `close`, `close_all` |

THE PHASE THAT MATTERS MOST IS "COMMITTED, UNPUBLISHED", because it is the one the implementation
reaches after the point of no return. `sys_channel_recv_caps` commits the delivery, installs every
capability into its pre-booked slots, and only then copies the raw handle numbers out. A copyout
failure there cannot return the message to the queue - the capabilities have left it - so what it
does instead is close every handle it installed. The model must represent that as its own
transition, not as a rollback.

## The charge ledger

Three ledgers, and every commit or rollback names which entry moves:

| Ledger | Charged by | Refunded by |
| --- | --- | --- |
| Domain handle quota | `HandleTable::reserve` (one per booked slot), `insert`/`try_insert`/`place` | `close`, `release_reservation`, `commit_taken`, `abandon_taken`, `restore_taken` into a CLOSED table, `Drop for HandleTable` |
| Domain in-transit IPC bytes | `Message::charge_queue`, inside `send_inner` after ring space is assured | `release_queue_charge` at the commit point, and structurally by `Drop for QueueCharge` |
| Endpoint queue depth | `inbox.len()`, plus `in_flight` for a message taken and not committed | `commit_delivery` (really free) or `return_to_head` (back on the queue) |

A TAKE DOES NOT REFUND AND A RESTORE DOES NOT CHARGE. `take_for_transfer` empties the slot and
leaves the charge standing, because the slot is still spoken for; the refund happens at
`commit_taken` (the handle value died) or at `abandon_taken` (the transfer can no longer be resolved
and the capability is gone). `restore_taken` puts the capability back under the SAME charge - except
into a closed table, where there is nobody to give it back to and the refund happens there instead.
Those four are the whole of the handle ledger on the transfer path, and each is a different model
transition.

`insert_reserved` charges NOTHING: the quota for that handle was paid when the room was booked.
A model that charges at install would disagree with the implementation about the ledger for the
whole window between booking and install, which is exactly the window a termination can land in.

The queued-bytes charge travels WITH the message rather than being released at the dequeue. The
refund is structural - it lives on `QueueCharge`'s `Drop` - so a path that takes a message and
returns early cannot leave a domain charged for bytes queued nowhere.

## The actions, and what makes each one atomic

Each row is one model action. The lock column is what makes it a single step; anything not in a row
is a point where the syscall may yield, allocate, copy user memory, or have its process terminated.

| Model action | Rust | Atomic under |
| --- | --- | --- |
| `Take` | `HandleTable::take_for_transfer` | the process's handle-table lock |
| `CommitTake` | `HandleTable::commit_taken` | same |
| `RestoreTake` | `HandleTable::restore_taken` | same |
| `AbandonTake` | `HandleTable::abandon_taken` | same |
| `Book` | `HandleTable::reserve` | same |
| `Unbook` | `HandleTable::release_reservation` | same |
| `Install` | `HandleTable::insert_reserved` | same |
| `Close` | `HandleTable::close` | same |
| `Terminate` | `HandleTable::close_all` | same |
| `Enqueue` | `Channel::send_inner` | the PEER endpoint's inbox lock |
| `Peek` | `Channel::peek_identified` | this endpoint's inbox lock |
| `Dequeue` | `Channel::recv_identified` | same |
| `ReturnToHead` | `Channel::return_to_head` | same |
| `CommitDelivery` | `Channel::commit_delivery` | same, plus the charge release before it |
| `Init`'s live slot | `HandleTable::try_place` (every `insert`/`try_insert` path) | the process's handle-table lock |

THE LAST ROW IS NOT AN ACTION OF THE MODEL, and saying so is the point. `Transfer.tla` follows ONE
capability - `TheCap`, live in the sender's slot 1 at `Init` - through the queue to the receiver, and
`TransferIsLinear` counts the copies of that one. A process creating a second object is not a step of
that behaviour; it is the start of another. The trace sink records it as `SEED` anyway, because a
checker that saw a `Take` from a slot it never saw filled would have nothing to check the take
against.

What the host checker holds a `SEED` to is what the model's `NoForgery` is about: a capability may
not appear where the accounting says none should be. It may not displace a live one, it may not land
in a slot a transfer is holding, and it may not appear in a table that has closed. Those three are
refused; the mere fact of a new capability is not.

TWO LOCKS ARE TWO ACTIONS. `sys_channel_send` takes the handle-table lock to take the capability,
RELEASES it, then takes the peer's inbox lock to enqueue - so "send" is at least three model actions
with interleaving points between them, however transactional its helper names sound. The same is
true of `receive_transactionally`: peek, book, dequeue are three lock acquisitions, and the message
it finally takes is the one it NAMED rather than whatever is at the head, which is the only reason
the sequence is safe at all.

`commit_delivery` releases the sender's byte charge before taking the inbox lock. The model treats
the charge release and the slot release as one action because nothing observes the state between
them: the message is off the queue and in the receiver's hand either way.

## Where a syscall can be interrupted, by path

- **`sys_channel_send`**: after `lookup_typed` and before `read_bytes` (a user copy that can fault);
  after `Vec::try_reserve` and before `take_for_transfer` (an allocation that can fail); between
  `take_for_transfer` and `send_charged_or_return` (the capability is transfer-local and its slot is
  `TransferReserved`); between the send's outcome and `commit_taken`/`restore_taken`.
- **`sys_channel_recv_caps`**: between `peek_identified` and `reserve`; between `reserve` and
  `recv_identified` (`Booked` slots held, message still queued); between `recv_identified` and the
  payload copy (message `delivery-local`, queue slot held by `in_flight`); at the payload copy,
  which fails BEFORE the commit and returns the message to the head; between `commit_delivery` and
  `insert_reserved` (the point of no return has passed and no handle exists yet); between
  `insert_reserved` and the handle-number copyout (`committed, unpublished`); at the copyout, which
  fails AFTER the commit and closes what it installed.
- **Every one of those points is a place `close_all` may arrive**, which is why `close_all` skips
  `TransferReserved` slots and why `restore_taken` into a `Closed` table drops the capability rather
  than installing it.

## What the model does not claim

Recorded here so the specification cannot quietly acquire any of it:

- **No selective derivation-tree revoke.** `ObjectHeader::revoke` is test-only. Production authority
  has stale handles after slot reuse and object destruction and no syscall that globally invalidates
  derived capabilities. The revocation configuration is separate and its result may not be cited for
  the production claim.
- **No ambient lookup.** Every authority is reached through a handle in the calling process's table;
  there is no name that resolves without one.
- **No transfer without `TRANSFER`** and **no duplication without `DUPLICATE`**: `take_for_transfer`
  and `duplicate` both take the required right as an argument and refuse without it.
- **`USE` is an abstraction.** It stands for a type-correct object operation. It is not a claim that
  every concrete syscall's rights table is complete; that is what the rights gates in `syscall`
  answer, one call at a time.
