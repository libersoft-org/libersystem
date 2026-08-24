------------------------------- MODULE Transfer -------------------------------
(***************************************************************************)
(* The composed specification: one capability moving from one process's     *)
(* handle table, through a channel, into another's - and every way that can  *)
(* be interrupted.                                                          *)
(*                                                                         *)
(* The action granularity is the one `MODEL_MAP.md` fixes: ONE LOCK IS ONE  *)
(* ACTION. `sys_channel_send` takes the handle table, releases it, then      *)
(* takes the peer's inbox, so Take, Enqueue and CommitTake are three actions *)
(* with interleaving points between them - which is where a termination, an  *)
(* allocation failure or a user-copy fault can arrive.                       *)
(***************************************************************************)
EXTENDS Capability

VARIABLES
    table,        \* [Procs -> [1..Slots -> slot]]
    closed,       \* [Procs -> BOOLEAN] - close_all has run
    charge,       \* [Procs -> Nat] - the Domain handle ledger
    booked,       \* [Procs -> Seq(1..Slots)] - concrete slots taken out of circulation
    xfer,         \* [Procs -> Caps] - the transfer-local capability
    xferSlot,     \* [Procs -> Nat] - the slot it came from, 0 for none
    queue,        \* Seq of messages on the receiving endpoint
    inflight,     \* messages taken and not yet committed - they still hold their queue slot
    held,         \* the receiver's delivery-local message
    holder,       \* which process holds it, "none" when nobody does
    installed,    \* Seq(1..Slots) - slots installed for `held` whose numbers are not published yet
    committed,    \* TRUE once commit_delivery has run for `held` - the point of no return
    bytes,        \* the in-transit IPC byte ledger, one unit per queued message
    peeked,       \* the message identity the receiver inspected, 0 for none
    nextId,       \* the monotonic message identity `Message::new` hands out
    lastUse,      \* what the last abstract object operation was performed against
    objgen        \* the object's CURRENT generation. A capability captured one when it was made, and
                  \* the two differing is what makes a handle to a destroyed object detectable.

vars == <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

Sender == CHOOSE p \in Procs : TRUE
Receiver == CHOOSE p \in Procs : p # Sender

Depth == Len(queue) + inflight

TypeOK ==
    /\ table \in [Procs -> [1..Slots -> [state: SlotStates, cap: Caps, gen: 1..MaxGen]]]
    /\ closed \in [Procs -> BOOLEAN]
    /\ charge \in [Procs -> 0..(2 * Slots)]
    /\ xfer \in [Procs -> Caps]
    /\ xferSlot \in [Procs -> 0..Slots]
    /\ inflight \in 0..QueueLimit
    /\ committed \in BOOLEAN
    /\ bytes \in 0..QueueLimit
    /\ peeked \in 0..MaxId
    /\ nextId \in 1..(MaxId + 1)
    /\ lastUse \in Caps
    /\ objgen \in 1..MaxGen

(***************************************************************************)
(* The initial state: the sender holds one live, transferable capability;   *)
(* everything else is empty. One object, because the linear-transfer         *)
(* invariant is about ONE authority being in one place.                      *)
(***************************************************************************)
TheObject == CHOOSE o \in Objects : TRUE
TheType == CHOOSE t \in Types : TRUE
TheCap == [obj |-> TheObject, type |-> TheType, rights |-> MintedRights, objgen |-> 1]

Init ==
    /\ table = [p \in Procs |->
                 [i \in 1..Slots |->
                   IF p = Sender /\ i = 1 THEN [state |-> "Live", cap |-> TheCap, gen |-> 1]
                   ELSE EmptySlot]]
    /\ closed = [p \in Procs |-> FALSE]
    /\ charge = [p \in Procs |-> IF p = Sender THEN 1 ELSE 0]
    /\ booked = [p \in Procs |-> <<>>]
    /\ xfer = [p \in Procs |-> NoCap]
    /\ xferSlot = [p \in Procs |-> 0]
    /\ queue = <<>>
    /\ inflight = 0
    /\ held = NoMsg
    /\ holder = "none"
    /\ installed = <<>>
    /\ committed = FALSE
    /\ bytes = 0
    /\ peeked = 0
    /\ nextId = 1
    /\ lastUse = NoCap
    /\ objgen = 1

(***************************************************************************)
(* THE SEND SIDE.                                                           *)
(***************************************************************************)

\* `take_for_transfer`: the slot empties and is RESERVED, so a second thread finds nothing to take.
\* The charge does NOT move - the slot is still spoken for.
Take(p, i) ==
    /\ ~closed[p]
    /\ xfer[p] = NoCap
    /\ table[p][i].state = "Live"
    /\ "TRANSFER" \in table[p][i].cap.rights
    /\ xfer' = [xfer EXCEPT ![p] = table[p][i].cap]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = i]
    /\ table' = [table EXCEPT ![p][i] = [state |-> "Reserved", cap |-> NoCap, gen |-> table[p][i].gen]]
    /\ UNCHANGED <<closed, charge, booked, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* Recycle a slot under the generation rule: at the ceiling it RETIRES rather than wrapping.
Recycle(p, i) ==
    IF table[p][i].gen = MaxGen
    THEN [state |-> "Retired", cap |-> NoCap, gen |-> MaxGen]
    ELSE [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen + 1]

\* `commit_taken`: the handle value dies and the quota is refunded. Only after the message is queued.
CommitTake(p) ==
    /\ xferSlot[p] # 0
    /\ xfer[p] = NoCap          \* the capability has already gone into the queue
    /\ table' = [table EXCEPT ![p][xferSlot[p]] = Recycle(p, xferSlot[p])]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, booked, xfer, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `restore_taken`: the send was refused, and the capability goes back to the SAME handle - unless
\* the table has been closed, in which case there is nobody to give it back to and the charge is
\* refunded here instead.
RestoreTake(p) ==
    /\ IsCap(xfer[p])
    /\ xferSlot[p] # 0
    /\ IF closed[p]
       THEN /\ table' = [table EXCEPT ![p][xferSlot[p]] = Recycle(p, xferSlot[p])]
            /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
       ELSE /\ table' = [table EXCEPT ![p][xferSlot[p]] =
                          [state |-> "Live", cap |-> xfer[p], gen |-> table[p][xferSlot[p]].gen]]
            /\ UNCHANGED charge
    /\ xfer' = [xfer EXCEPT ![p] = NoCap]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, booked, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `abandon_taken`: the transfer can no longer be resolved either way. The capability is GONE and
\* the slot must not hold its place forever.
AbandonTake(p) ==
    /\ xferSlot[p] # 0
    /\ IsCap(xfer[p])
    /\ table' = [table EXCEPT ![p][xferSlot[p]] = Recycle(p, xferSlot[p])]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ xfer' = [xfer EXCEPT ![p] = NoCap]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, booked, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `send_inner`: room in the ring, then the charge, then the message. A refused send charges nothing.
Enqueue(p) ==
    /\ IsCap(xfer[p])
    /\ Depth < QueueLimit
    \* AN EXPLICIT MODEL BOUND, not a property of the kernel: `Message::new` mints identities from a
    \* monotonic counter, which is unbounded and would make the state space infinite. Two is enough
    \* for the property identities exist for - one receiver looking at a message and another taking
    \* it - and the bound is stated here rather than hidden in a type.
    /\ nextId =< MaxId
    /\ queue' = Append(queue, [id |-> nextId, caps |-> <<xfer[p]>>, slotHeld |-> FALSE])
    /\ nextId' = nextId + 1
    /\ bytes' = bytes + 1
    /\ xfer' = [xfer EXCEPT ![p] = NoCap]
    /\ UNCHANGED <<table, closed, charge, booked, xferSlot, inflight, held, holder, installed, committed, peeked, lastUse, objgen>>

(***************************************************************************)
(* THE RECEIVE SIDE.                                                        *)
(***************************************************************************)

\* `HandleTable::reserve`: a CONCRETE slot leaves circulation and the quota is charged now.
Book(p) ==
    /\ ~closed[p]
    /\ Len(booked[p]) = 0
    /\ \E i \in 1..Slots :
         /\ table[p][i].state = "Free"
         /\ table' = [table EXCEPT ![p][i] = [state |-> "Booked", cap |-> NoCap, gen |-> table[p][i].gen]]
         /\ booked' = [booked EXCEPT ![p] = Append(booked[p], i)]
    /\ charge' = [charge EXCEPT ![p] = charge[p] + 1]
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `release_reservation`: the booking goes back, slot and quota together.
Unbook(p) ==
    \* NOT WHILE A MESSAGE IS IN HAND. `release_reservation` is reached from two places and neither
    \* is "in the middle of a delivery": the receive that could not TAKE the message it peeked, and
    \* the payload copy that failed - which gives the booking back as part of putting the message
    \* back. Between the take and the commit the booking is what the install is going to use.
    /\ ~(holder = p /\ IsMsg(held))
    /\ Len(booked[p]) > 0
    /\ LET i == Head(booked[p]) IN
       /\ table' = [table EXCEPT ![p][i] = [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]]
       /\ booked' = [booked EXCEPT ![p] = Tail(booked[p])]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `peek_identified`: the receiver learns the head's identity and shape. It holds no lock afterwards,
\* so anything may happen to the queue before it comes back.
Peek(p) ==
    /\ held = NoMsg
    /\ Len(queue) > 0
    /\ peeked' = queue[1].id
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, nextId, lastUse, objgen>>

\* `recv_identified`: the message leaves the queue AND TAKES ITS SLOT WITH IT. Nothing is announced
\* as free here - the message can still come back.
Dequeue(p) ==
    /\ held = NoMsg
    /\ Len(queue) > 0
    \* NAMED, NOT WHATEVER IS THERE. This is the whole of `recv_identified`: a receiver commits only
    \* the message it inspected, so a second receiver taking the peeked one in between makes this
    \* refuse rather than hand over a message whose shape was never checked.
    /\ peeked = Head(queue).id
    /\ Len(booked[p]) >= Len(Head(queue).caps)
    /\ held' = [Head(queue) EXCEPT !.slotHeld = TRUE]
    /\ holder' = p
    /\ queue' = Tail(queue)
    /\ inflight' = inflight + 1
    /\ committed' = FALSE
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, installed, bytes, peeked, nextId, lastUse, objgen>>

\* The payload copy faulted BEFORE the commit: the message goes back to the head, still charged,
\* still holding the slot it never gave up, and the booking is released.
PayloadCopyFails(p) ==
    /\ holder = p
    /\ IsMsg(held)
    /\ ~committed
    /\ queue' = <<[held EXCEPT !.slotHeld = FALSE]>> \o queue
    /\ inflight' = inflight - 1
    /\ held' = NoMsg
    /\ holder' = "none"
    \* The booking goes back with it - slot and quota together, exactly as `release_reservation`
    \* does, because the caller is going to peek again rather than install anything.
    /\ Len(booked[p]) > 0
    /\ LET i == Head(booked[p]) IN
       /\ table' = [table EXCEPT ![p][i] = [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]]
       /\ booked' = [booked EXCEPT ![p] = Tail(booked[p])]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ UNCHANGED <<closed, xfer, xferSlot, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `commit_delivery`: the payload is in the caller's buffer. THE POINT OF NO RETURN - the queued
\* byte charge is released and the queue slot is really free.
CommitDelivery(p) ==
    /\ holder = p
    /\ IsMsg(held)
    /\ ~committed
    /\ committed' = TRUE
    /\ held' = [held EXCEPT !.slotHeld = FALSE]
    /\ inflight' = inflight - 1
    /\ bytes' = bytes - 1
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, holder, installed, peeked, nextId, lastUse, objgen>>

\* `insert_reserved`: into the slot this booking owns. Charges nothing - `reserve` already paid.
Install(p) ==
    /\ holder = p
    /\ committed
    /\ ~closed[p]
    /\ Len(held.caps) > 0
    /\ Len(booked[p]) > 0
    /\ LET i == Head(booked[p]) IN
       /\ table' = [table EXCEPT ![p][i] = [state |-> "Live", cap |-> Head(held.caps), gen |-> table[p][i].gen]]
       /\ booked' = [booked EXCEPT ![p] = Tail(booked[p])]
       /\ installed' = Append(installed, i)
    /\ held' = [held EXCEPT !.caps = Tail(held.caps)]
    /\ UNCHANGED <<closed, charge, xfer, xferSlot, queue, inflight, holder, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `insert_reserved` INTO A CLOSED TABLE. The same barrier `restore_taken` stands behind: there is
\* nobody to install for, so the capability is dropped and the quota `reserve` charged is refunded -
\* which is what `close_all` would have done to this handle had it existed at the time.
InstallIntoClosed(p) ==
    /\ holder = p
    /\ committed
    /\ closed[p]
    /\ Len(held.caps) > 0
    /\ Len(booked[p]) > 0
    /\ LET i == Head(booked[p]) IN
       /\ table' = [table EXCEPT ![p][i] = [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]]
       /\ booked' = [booked EXCEPT ![p] = Tail(booked[p])]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ held' = [held EXCEPT !.caps = Tail(held.caps)]
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* The handle numbers reached userspace. The capability is PUBLISHED and the receive is over.
Publish(p) ==
    /\ holder = p
    /\ committed
    /\ Len(held.caps) = 0
    \* NOTHING LEFT TO INSTALL ENDS THE RECEIVE TOO. A message whose capabilities were all dropped -
    \* the table closed under it - still has a payload that was delivered and a count to write, so
    \* the syscall returns. A receive that could not end would be a message held forever.
    /\ installed' = <<>>
    /\ held' = NoMsg
    /\ holder' = "none"
    /\ committed' = FALSE
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, bytes, peeked, nextId, lastUse, objgen>>

\* The handle-number copyout faulted AFTER the commit. The message cannot go back - its capabilities
\* have left it - so what is recoverable is recovered: every installed handle is closed.
\* A POST-COMMIT FAILURE NEVER RETURNS TO A QUEUED MESSAGE.
CopyoutFails(p) ==
    /\ holder = p
    /\ committed
    /\ Len(held.caps) = 0
    /\ Len(installed) > 0
    \* WHAT IS STILL THERE TO CLOSE. The kernel closes each installed handle by its raw number, and
    \* `close` checks the slot's GENERATION - so a handle whose slot has already been recycled by
    \* something else is refused rather than closed twice, and the caller's `let _ = table.close(..)`
    \* is right to ignore it. The refund therefore counts what was actually closed, not what was
    \* installed.
    /\ LET mine == {i \in 1..Slots :
                     /\ \E k \in 1..Len(installed) : installed[k] = i
                     /\ table[p][i].state = "Live"} IN
       /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                      IF i \in mine THEN Recycle(p, i) ELSE table[p][i]]]
       /\ charge' = [charge EXCEPT ![p] = charge[p] - Cardinality(mine)]
    /\ installed' = <<>>
    /\ held' = NoMsg
    /\ holder' = "none"
    /\ committed' = FALSE
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, bytes, peeked, nextId, lastUse, objgen>>

(***************************************************************************)
(* CLOSE AND TERMINATION, which may arrive between any two of the above.    *)
(***************************************************************************)

\* `HandleTable::duplicate`: a NEW capability for the same object, carrying rights the caller asked
\* for AND the original already has - the check is `cap.rights.contains(new_rights)`, so a duplicate
\* can only narrow.
\*
\* THROUGH THE QUOTA, like every other user-reachable install. It was the unbounded `insert` once, so
\* a process holding one duplicable handle could pass its handle limit simply by asking, and every
\* other check bounded by "how many handles the caller holds" was bounded by nothing.
Duplicate(p, i, j, r) ==
    /\ ~closed[p]
    /\ i # j
    /\ table[p][i].state = "Live"
    /\ "DUPLICATE" \in table[p][i].cap.rights
    /\ r \subseteq table[p][i].cap.rights
    /\ r # {}
    /\ table[p][j].state = "Free"
    /\ table' = [table EXCEPT ![p][j] =
                  [state |-> "Live", cap |-> [table[p][i].cap EXCEPT !.rights = r], gen |-> table[p][j].gen]]
    /\ charge' = [charge EXCEPT ![p] = charge[p] + 1]
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `lookup_typed(handle, ObjectType, Rights)` followed by a type-correct operation. The abstraction is
\* deliberate: USE stands for "an operation this object supports", not for a claim that every
\* concrete syscall's rights table is complete.
\*
\* THE GUARD IS THE PROPERTY. A use requires a LIVE slot, the type the caller asked for, the right the
\* operation needs, and a capability whose captured object generation is the object's - which is what
\* makes a handle to a destroyed object detectable rather than merely wrong. `lastUse` records what
\* it was permitted against so an invariant can say so afterwards.
Usable(p, i, t) ==
    /\ ~closed[p]
    /\ table[p][i].state = "Live"
    /\ table[p][i].cap.type = t
    /\ "USE" \in table[p][i].cap.rights
    /\ table[p][i].cap.objgen = objgen

Use(p, i, t) ==
    /\ Usable(p, i, t)
    /\ lastUse' = table[p][i].cap
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, objgen>>

\* `ObjectHeader::revoke`, WHICH IS TEST-ONLY IN THE TREE. Bumping the object's generation makes
\* every capability that captured the old one detectably stale. The production authority model has
\* no syscall that does this, so a configuration with `RevocationModeled` false is the one a
\* production claim may cite - and this action does not exist in it at all.
Revoke ==
    /\ RevocationModeled
    /\ objgen < MaxGen
    /\ objgen' = objgen + 1
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse>>

Close(p, i) ==
    /\ ~closed[p]
    /\ table[p][i].state = "Live"
    /\ table' = [table EXCEPT ![p][i] = Recycle(p, i)]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* `close_all`: the table takes nothing new. A slot with a transfer in flight is NOT reclaimed -
\* its capability is elsewhere and one of commit/restore is still to come.
\*
\* A BOOKED SLOT IS NOT THIS FUNCTION'S TO RECLAIM EITHER, for the same reason: `reserve` took it
\* out of circulation and an `insert_reserved` may still be on its way to it.
\*
\* THAT IS THE FIX, AND THE SPIKE IS WHAT FOUND IT NEEDED ONE. The code this models rebuilt the free
\* list from every slot that is not `reserved` - which a booked slot is not - so the index landed on
\* the free list while `booked` still named it, and the quota `reserve` had charged was refunded by
\* nobody. TLC produced it in three states: Init, Book, Terminate, violating `QuotaConserved`.
\* Replace the `Booked` arm below with the `Free` one to reproduce that counterexample.
Terminate(p) ==
    /\ ~closed[p]
    /\ closed' = [closed EXCEPT ![p] = TRUE]
    /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                   IF table[p][i].state \in {"Reserved", "Retired", "Booked"} THEN table[p][i]
                   ELSE IF table[p][i].state = "Live" THEN Recycle(p, i)
                   ELSE [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]]]
    /\ charge' = [charge EXCEPT ![p] =
                   charge[p] - Cardinality({i \in 1..Slots : table[p][i].state = "Live"})]
    /\ UNCHANGED <<booked, xfer, xferSlot, queue, inflight, held, holder, installed, committed, bytes, peeked, nextId, lastUse, objgen>>

\* A SYSTEM WITH NOTHING LEFT TO DO IS NOT A DEADLOCK. Every process has terminated, no transfer is
\* outstanding and no message is in flight - so the only behaviour left is to stay there. Saying that
\* in the specification is better than switching off the check that would otherwise report it, which
\* would also stop it reporting a state that IS stuck.
Done ==
    /\ \A p \in Procs : closed[p]
    /\ \A p \in Procs : ~IsCap(xfer[p])
    /\ ~IsMsg(held)
    /\ Len(queue) = 0
    /\ UNCHANGED vars

Next ==
    \/ Done
    \/ \E p \in Procs, i \in 1..Slots : Take(p, i) \/ Close(p, i)
    \/ \E p \in Procs, i \in 1..Slots, t \in Types : Use(p, i, t)
    \/ \E p \in Procs, i, j \in 1..Slots, r \in DerivedRights : Duplicate(p, i, j, r)
    \/ Revoke
    \/ \E p \in Procs : CommitTake(p) \/ RestoreTake(p) \/ AbandonTake(p) \/ Enqueue(p)
    \/ \E p \in Procs : Book(p) \/ Unbook(p) \/ Peek(p) \/ Dequeue(p) \/ CommitDelivery(p)
    \/ \E p \in Procs : Install(p) \/ InstallIntoClosed(p) \/ Publish(p) \/ CopyoutFails(p) \/ Terminate(p)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE SAFETY INVARIANTS. Every one is a sentence from `MODEL_MAP.md`, and  *)
(* the names are the ones P02M0154 M4 fixes so a configuration cannot       *)
(* quietly check a weaker property under a familiar name.                   *)
(***************************************************************************)

\* Every place a capability may be, counted. ONE AUTHORITY, ONE PLACE.
CopiesInSlots == Cardinality({<<p, i>> \in Procs \X (1..Slots) : HoldsCap(table[p][i])})
CopiesInXfer == Cardinality({p \in Procs : IsCap(xfer[p])})
CopiesInQueue == IF Len(queue) = 0 THEN 0 ELSE Len(queue[1].caps)
CopiesInHeld == IF IsMsg(held) THEN Len(held.caps) ELSE 0

\* TRANSFER IS LINEAR: absent DUPLICATE, one capability has exactly one authority-bearing owner
\* through take, queue, delivery, commit and rollback. Success neither copies it nor loses it.
\*
\* COUNTED OVER THE OBJECT, WHICH IS EXACT ONLY WHILE NOTHING DUPLICATES. The minted capability in
\* this configuration carries USE and TRANSFER and not DUPLICATE, so one authority means one
\* instance. A configuration that models `HandleTable::duplicate` needs an instance identity on the
\* capability - two capabilities for one object are then two owners and not a violation - and that
\* is what `handles.cfg` adds rather than something this weakens.
TransferIsLinear == CopiesInSlots + CopiesInXfer + CopiesInQueue + CopiesInHeld =< 1

\* AUTHORITY NEVER WIDENS: nothing anywhere carries more than the capability it descends from, and
\* a transfer adds nothing at all.
AuthorityNeverWidens ==
    /\ \A p \in Procs, i \in 1..Slots :
         HoldsCap(table[p][i]) => table[p][i].cap.rights \subseteq TheCap.rights
    /\ \A p \in Procs : IsCap(xfer[p]) => xfer[p].rights \subseteq TheCap.rights
    /\ (Len(queue) > 0 /\ Len(queue[1].caps) > 0) => queue[1].caps[1].rights \subseteq TheCap.rights

\* NO FORGERY: every capability that exists names the object that was minted, at the generation it
\* was minted against. Nothing in the transition relation can produce another.
NoForgery ==
    /\ \A p \in Procs, i \in 1..Slots :
         HoldsCap(table[p][i]) => table[p][i].cap.obj = TheObject /\ table[p][i].cap.objgen = TheCap.objgen
    /\ \A p \in Procs : IsCap(xfer[p]) => xfer[p].obj = TheObject

\* QUOTA CONSERVED: the handle ledger equals the slots it represents, at every point of every
\* transfer - including the ones inside a two-lock operation.
QuotaConserved == \A p \in Procs : charge[p] = ChargedSlots(table[p])

\* QUEUE BOUNDED: queued plus delivery-reserved never exceeds the endpoint's depth.
QueueBounded == Depth =< QueueLimit

\* SLOT OWNERSHIP IS UNIQUE: an index is in exactly one state, and a booking names a slot that is
\* actually booked - it cannot be on the free list at the same time.
SlotOwnershipUnique ==
    \A p \in Procs :
      \A k \in 1..Len(booked[p]) : table[p][booked[p][k]].state = "Booked"

\* POST-COMMIT COPYOUT IS TERMINAL: once a receive has ended, nothing is installed and unpublished -
\* either every handle number was published or every installed handle was closed. The delivered
\* payload is never described as rolled back, which is why there is no transition from `committed`
\* back to a queued message.
PostCommitCopyoutIsTerminal ==
    /\ (holder = "none") => (Len(installed) = 0 /\ ~committed)
    /\ committed => IsMsg(held) \/ Len(installed) > 0

\* A CLOSED PROCESS CANNOT RESURRECT: close is a barrier. A rollback into a closed table creates no
\* live handle, and no booking survives it.
ClosedProcessCannotResurrect ==
    \A p \in Procs : closed[p] => \A i \in 1..Slots : table[p][i].state # "Live"

\* STALE HANDLES STAY DEAD: a slot at the generation ceiling is retired rather than recycled, so no
\* raw handle can come back round to name a later capability.
StaleHandlesStayDead ==
    \A p \in Procs, i \in 1..Slots :
      /\ (table[p][i].state = "Retired") => table[p][i].gen = MaxGen
      /\ table[p][i].gen =< MaxGen

\* The same property's other half, as a step: a slot's generation never goes backwards.
GenerationsOnlyAdvance ==
    [][\A p \in Procs, i \in 1..Slots : table'[p][i].gen >= table[p][i].gen]_vars

\* MESSAGE IDENTITY IS STABLE: whatever a receiver holds is the message it inspected. Another
\* receiver cannot substitute a same-shaped one between the look and the take, which is the race
\* `recv_identified` exists for - a receiver that declared room for a hundred bytes being handed a
\* megabyte, and the copy using the RECEIVED length.
MessageIdentityStable == IsMsg(held) => held.id = peeked

\* TYPE SEALING: whatever the last abstract operation ran against had the type it was asked for, the
\* right it needed and the object's live generation. A capability that carried less could not have
\* been used, which is `lookup_typed` refusing rather than a caller remembering to check.
TypeSealing ==
    IsCap(lastUse) => /\ lastUse.type \in Types
                      /\ "USE" \in lastUse.rights

\* A REVOKED SNAPSHOT CANNOT OPERATE: a capability that captured an older object generation is not
\* usable, whatever else it carries. Stated over the GUARD rather than over an outcome, so that
\* removing the generation check from `Usable` fails this rather than passing quietly - which is the
\* mutation this invariant exists to catch.
RevokedSnapshotCannotOperate ==
    \A p \in Procs, i \in 1..Slots, t \in Types :
      Usable(p, i, t) => table[p][i].cap.objgen = objgen

\* RECEIVE IS TRANSACTIONAL: before the commit there is nothing installed, and the booking the
\* receive took is either still held or has been given back.
ReceiveIsTransactional ==
    (IsMsg(held) /\ ~committed) => Len(installed) = 0
=============================================================================
