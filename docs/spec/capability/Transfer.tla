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
    xfer,         \* [Procs -> Seq(Caps)] - the transfer-local batch, in the order it was taken
    xferSlot,     \* [Procs -> Seq(1..Slots)] - the slots it came from, in the same order
    queue,        \* Seq of messages on the receiving endpoint
    inflight,     \* messages taken and not yet committed - they still hold their queue slot
    held,         \* [Procs -> message] - the delivery-local message OF EACH RECEIVER
    installed,    \* [Procs -> Seq(1..Slots)] - slots installed for that receiver's `held`
    committed,    \* [Procs -> BOOLEAN] - commit_delivery has run for that receiver's `held`
    bytes,        \* the in-transit IPC byte ledger, one unit per queued message
    peeked,       \* [Procs -> 0..MaxId] - the identity EACH receiver inspected, 0 for none
    nextId,       \* the monotonic message identity `Message::new` hands out
    lastUse,      \* what the last abstract object operation was performed against
    lastAsk,      \* the type that operation ASKED for - the other half of what type sealing means
    outcome,      \* what the last action that ENDED something was, when `CoversModeled` - see the
                  \* cover properties. A ghost, and a configuration's choice: it exists to show that
                  \* the dangerous transitions are REACHED, which the smallest configuration proves
                  \* as well as the largest and far more cheaply.
    lastBatch,    \* HOW MANY capabilities the action that wrote `outcome` acted on. Written in the
                  \* SAME action as `outcome` and by nothing else, which is the whole point of it:
                  \* a cover reading `outcome` and a separately-evolving variable is satisfied by a
                  \* state belonging to a different receive, and `NoTwoCapsPublished` was exactly
                  \* that - `Publish` emptied `installed` in the act of setting `outcome`, so the
                  \* state it produced could not refute a cover asking for two installed handles.
    objgen        \* the object's CURRENT generation. A capability captured one when it was made, and
                  \* the two differing is what makes a handle to a destroyed object detectable.

vars == <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

Sender == CHOOSE p \in Procs : TRUE
Receiver == CHOOSE p \in Procs : p # Sender

Depth == Len(queue) + inflight

TypeOK ==
    /\ table \in [Procs -> [1..Slots -> [state: SlotStates, cap: Caps, gen: 1..MaxGen]]]
    /\ closed \in [Procs -> BOOLEAN]
    /\ charge \in [Procs -> 0..(2 * Slots)]
    /\ inflight \in 0..QueueLimit
    /\ committed \in [Procs -> BOOLEAN]
    /\ bytes \in 0..QueueLimit
    /\ peeked \in [Procs -> 0..MaxId]
    /\ nextId \in 1..(MaxId + 1)
    /\ lastUse \in Caps
    /\ lastAsk \in Types \cup {"none"}
    /\ outcome \in {"none", "sent", "restored", "abandoned", "payload-failed", "published", "copyout-failed", "dropped-into-closed"}
    /\ lastBatch \in 0..BatchMax
    /\ objgen \in 1..MaxGen

(***************************************************************************)
(* The initial state: the sender holds one live, transferable capability;   *)
(* everything else is empty. One object, because the linear-transfer         *)
(* invariant is about ONE authority being in one place.                      *)
(***************************************************************************)
TheObject == CHOOSE o \in Objects : TRUE
TheType == CHOOSE t \in Types : TRUE
TheCap == [obj |-> TheObject, type |-> TheType, rights |-> MintedRights, from |-> MintedRights, objgen |-> 1]

\* ONE MINTED CAPABILITY PER OBJECT, WHICH IS WHAT MAKES A BATCH OF TWO REACHABLE.
\*
\* `Init` used to mint exactly one capability whatever `Objects` held, so a configuration asking for
\* a two-capability batch could not produce one: `BatchMax = 2` said the model would CARRY two and
\* nothing could ever put a second one anywhere. The rule that batch is there to check - a refused
\* send returns ALL of them, and a duplicate source is refused - was therefore only ever checked over
\* a batch of one, which is the length at which "all of them" and "it" are the same sentence.
\*
\* The order is fixed rather than chosen, so a configuration's node count does not depend on which
\* member of `Objects` TLC happens to pick first.
Sorted(S) == CHOOSE seq \in [1..Cardinality(S) -> S] :
                 /\ \A a, b \in 1..Cardinality(S) : a # b => seq[a] # seq[b]

\* A SET OF SLOT INDICES AS A SEQUENCE, IN INDEX ORDER. `reserve` books a set of slots in one call
\* and the model has to name them in some order to record the reservation; taking them in increasing
\* index order makes the state a function of the set rather than of which enumeration TLC happened to
\* produce, so the node count of a configuration does not move when the tool does.
Ordered(S) == [k \in 1..Cardinality(S) |->
                CHOOSE i \in S : Cardinality({j \in S : j =< i}) = k]

\* The slots a booking sequence names, as a set - what `release_reservation` gives back in one call.
SlotsOf(seq) == {seq[k] : k \in 1..Len(seq)}
ObjectSeq == Sorted(Objects)
\* A MINT DESCENDS FROM THE MINT. Nothing is above it, so its own right set is its source.
MintedFor(o) == [obj |-> o, type |-> TheType, rights |-> MintedRights, from |-> MintedRights, objgen |-> 1]
Minted == Cardinality(Objects)

Init ==
    /\ table = [p \in Procs |->
                 [i \in 1..Slots |->
                   IF p = Sender /\ i =< Minted /\ i =< Slots
                     THEN [state |-> "Live", cap |-> MintedFor(ObjectSeq[i]), gen |-> 1]
                     ELSE EmptySlot]]
    /\ closed = [p \in Procs |-> FALSE]
    /\ charge = [p \in Procs |-> IF p = Sender THEN Minted ELSE 0]
    /\ booked = [p \in Procs |-> <<>>]
    /\ xfer = [p \in Procs |-> <<>>]
    /\ xferSlot = [p \in Procs |-> <<>>]
    /\ queue = <<>>
    /\ inflight = 0
    /\ held = [p \in Procs |-> NoMsg]
    /\ installed = [p \in Procs |-> <<>>]
    /\ committed = [p \in Procs |-> FALSE]
    /\ bytes = 0
    /\ peeked = [p \in Procs |-> 0]
    /\ nextId = 1
    /\ lastUse = NoCap
    /\ lastAsk = "none"
    /\ outcome = "none"
    /\ lastBatch = 0
    /\ objgen = 1

(***************************************************************************)
(* THE SEND SIDE.                                                           *)
(***************************************************************************)

\* `take_for_transfer`: the slot empties and is RESERVED, so a second thread finds nothing to take.
\* The charge does NOT move - the slot is still spoken for.
Take(p, i) ==
    /\ ~closed[p]
    \* BOUNDED BY THE RESERVATIONS, NOT BY WHAT IS IN HAND. After the message is queued the
    \* capabilities have left but their slots are still reserved, and a take that started a second
    \* batch there would leave the two out of step - a capability whose slot nobody is holding. The
    \* syscall commits before it sends again, and this is that.
    /\ Len(xferSlot[p]) < BatchMax
    \* AND ONLY WHILE THE BATCH IS STILL IN HAND. The syscall takes every capability it is going to
    \* send and then sends them; it never takes another after the message is queued. Allowing that
    \* would leave a capability in hand whose slot nobody reserved.
    /\ Len(xfer[p]) = Len(xferSlot[p])
    \* NOT THE SAME HANDLE TWICE. A caller naming one handle twice in a batch would have this take
    \* it once and find nothing the second time - which is what `take_for_transfer` emptying the slot
    \* already guarantees, and this is that guarantee written where the model can check it.
    /\ \A k \in 1..Len(xferSlot[p]) : xferSlot[p][k] # i
    /\ table[p][i].state = "Live"
    /\ "TRANSFER" \in table[p][i].cap.rights
    /\ xfer' = [xfer EXCEPT ![p] = Append(xfer[p], table[p][i].cap)]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = Append(xferSlot[p], i)]
    /\ table' = [table EXCEPT ![p][i] = [state |-> "Reserved", cap |-> NoCap, gen |-> table[p][i].gen]]
    /\ UNCHANGED <<closed, charge, booked, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* Recycle a slot under the generation rule: at the ceiling it RETIRES rather than wrapping.
Recycle(p, i) ==
    IF table[p][i].gen = MaxGen
    THEN [state |-> "Retired", cap |-> NoCap, gen |-> MaxGen]
    ELSE [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen + 1]

\* `commit_taken`, FOR EVERY SLOT THE BATCH CAME FROM. The handle values die and their quota is
\* refunded. Only after the message is queued.
CommitTake(p) ==
    /\ Len(xferSlot[p]) > 0
    /\ Len(xfer[p]) = 0          \* the capabilities have already gone into the queue
    /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                   IF \E k \in 1..Len(xferSlot[p]) : xferSlot[p][k] = i THEN Recycle(p, i) ELSE table[p][i]]]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - Len(xferSlot[p])]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = <<>>]
    /\ UNCHANGED <<closed, booked, xfer, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `restore_taken`, FOR EVERY CAPABILITY THE SEND WAS CARRYING. All or nothing: a refused send costs
\* the caller nothing, not even the handles it named - so a batch comes back whole, to the same
\* slots, still live. Unless the table has been closed, in which case there is nobody to give any of
\* them back to and each one's charge is refunded here instead.
RestoreTake(p) ==
    /\ Len(xfer[p]) > 0
    /\ IF closed[p]
       THEN /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                           IF \E k \in 1..Len(xferSlot[p]) : xferSlot[p][k] = i THEN Recycle(p, i) ELSE table[p][i]]]
            /\ charge' = [charge EXCEPT ![p] = charge[p] - Len(xferSlot[p])]
       ELSE /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                           IF \E k \in 1..Len(xferSlot[p]) : xferSlot[p][k] = i
                           THEN [state |-> "Live",
                                 cap |-> xfer[p][CHOOSE k \in 1..Len(xferSlot[p]) : xferSlot[p][k] = i],
                                 gen |-> table[p][i].gen]
                           ELSE table[p][i]]]
            /\ UNCHANGED charge
    /\ xfer' = [xfer EXCEPT ![p] = <<>>]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = <<>>]
    /\ outcome' = IF CoversModeled THEN "restored" ELSE "none"
    /\ lastBatch' = IF CoversModeled THEN Len(xferSlot[p]) ELSE 0
    /\ UNCHANGED <<closed, booked, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, objgen>>

\* `abandon_taken`: the transfer can no longer be resolved either way. The capabilities are GONE and
\* the slots that were holding their places must not hold them forever.
AbandonTake(p) ==
    /\ Len(xferSlot[p]) > 0
    /\ Len(xfer[p]) > 0
    /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                   IF \E k \in 1..Len(xferSlot[p]) : xferSlot[p][k] = i THEN Recycle(p, i) ELSE table[p][i]]]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - Len(xferSlot[p])]
    /\ xfer' = [xfer EXCEPT ![p] = <<>>]
    /\ xferSlot' = [xferSlot EXCEPT ![p] = <<>>]
    /\ outcome' = IF CoversModeled THEN "abandoned" ELSE "none"
    /\ lastBatch' = IF CoversModeled THEN Len(xferSlot[p]) ELSE 0
    /\ UNCHANGED <<closed, booked, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, objgen>>

\* `send_inner`: room in the ring, then the charge, then the message. A refused send charges nothing.
Enqueue(p) ==
    /\ Len(xfer[p]) > 0
    /\ Depth < QueueLimit
    \* AN EXPLICIT MODEL BOUND, not a property of the kernel: `Message::new` mints identities from a
    \* monotonic counter, which is unbounded and would make the state space infinite. Two is enough
    \* for the property identities exist for - one receiver looking at a message and another taking
    \* it - and the bound is stated here rather than hidden in a type.
    /\ nextId =< MaxId
    /\ queue' = Append(queue, [id |-> nextId, caps |-> xfer[p], slotHeld |-> FALSE])
    /\ nextId' = nextId + 1
    /\ bytes' = bytes + 1
    /\ xfer' = [xfer EXCEPT ![p] = <<>>]
    /\ outcome' = IF CoversModeled THEN "sent" ELSE "none"
    /\ lastBatch' = IF CoversModeled THEN Len(xfer[p]) ELSE 0
    /\ UNCHANGED <<table, closed, charge, booked, xferSlot, inflight, held, installed, committed, peeked, lastUse, lastAsk, objgen>>

(***************************************************************************)
(* THE RECEIVE SIDE.                                                        *)
(***************************************************************************)

\* `HandleTable::reserve`: a CONCRETE slot leaves circulation and the quota is charged now.
\*
\* AS MANY AS THE MESSAGE NEEDS, WHICH IS WHY THE RECEIVE SIDE OF A BATCH IS REACHABLE AT ALL.
\*
\* This was `Len(booked[p]) = 0`, so a receiver could hold at most ONE booking - while `Dequeue`
\* requires `Len(booked[p]) >= Len(Head(queue).caps)`. A two-capability message could therefore be
\* sent and queued and never taken: it sat at the head forever, and every rule about the receive half
\* of a batch - install both, publish both, roll both back - was checked over a batch of one. It is
\* the same defect the send side had, in the half that was not looked at when that one was fixed.
\*
\* AND ALL OF THEM IN ONE ACTION, WHICH IS WHAT THE CODE DOES.
\*
\* Appending one slot per action was the fix for the cap and the wrong shape for the operation.
\* `receive_transactionally` takes the handle table ONCE and calls `reserve(reserved)`, and
\* `HandleTable::reserve` books the entire count before it returns - so there is no state in which a
\* receiver holds one booking of a two-booking reservation, and no interleaving point between them
\* for a termination or a close to arrive at. The model had both, and `MODEL_MAP.md`'s own rule -
\* one lock acquisition is one model action - is what it was breaking. A model that offers MORE
\* interleavings than the code is not conservative here: it is why `NoCloseBetweenTwoInstalls` could
\* be refuted, which made a model-only interleaving look like covered evidence.
Book(p) ==
    /\ ~closed[p]
    \* ONE `reserve` PER RECEIVE. The count is chosen here because the peek that determines it is
    \* not modeled as a separate lock: what matters is that the whole of it lands at once.
    /\ booked[p] = <<>>
    /\ \E S \in SUBSET {i \in 1..Slots : table[p][i].state = "Free"} :
         /\ Cardinality(S) >= 1
         /\ Cardinality(S) =< BatchMax
         /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                        IF i \in S THEN [state |-> "Booked", cap |-> NoCap, gen |-> table[p][i].gen]
                        ELSE table[p][i]]]
         /\ booked' = [booked EXCEPT ![p] = Ordered(S)]
         /\ charge' = [charge EXCEPT ![p] = charge[p] + Cardinality(S)]
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `release_reservation`: the booking goes back, slot and quota together.
\* THE WHOLE RESERVATION, like the `reserve` it undoes: `release_reservation(n)` is called once with
\* the count, and gives back every slot and every unit of quota before it returns.
Unbook(p) ==
    \* NOT WHILE A MESSAGE IS IN HAND. `release_reservation` is reached from two places and neither
    \* is "in the middle of a delivery": the receive that could not TAKE the message it peeked, and
    \* the payload copy that failed - which gives the booking back as part of putting the message
    \* back. Between the take and the commit the booking is what the install is going to use.
    /\ ~IsMsg(held[p])
    /\ Len(booked[p]) > 0
    /\ LET mine == SlotsOf(booked[p]) IN
       /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                      IF i \in mine THEN [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]
                      ELSE table[p][i]]]
       /\ charge' = [charge EXCEPT ![p] = charge[p] - Cardinality(mine)]
    /\ booked' = [booked EXCEPT ![p] = <<>>]
    \* AND THE IDENTITY GOES WITH IT. `peek_identified` hands a number back to ONE caller, which
    \* holds it on its own stack for one receive; it is not a property of the endpoint and it does
    \* not outlive the syscall. Leaving it set kept every identity a process had ever inspected alive
    \* in the state - states that describe nothing, and a great many of them.
    /\ peeked' = [peeked EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `peek_identified`: the receiver learns the head's identity and shape. It holds no lock afterwards,
\* so anything may happen to the queue before it comes back.
\* PER RECEIVER, WHICH IS WHAT `peek_identified` RETURNS TO ITS OWN CALLER. `peeked` was one scalar
\* for the whole system and `Peek(p)` ignored `p` when writing it, so one receiver overwrote
\* another's inspected identity and the first could then take the message the second had looked at -
\* with `MessageIdentityStable` still passing, because it compared the survivor against itself. The
\* identity a receiver checks against is the one IT was handed.
Peek(p) ==
    /\ ~IsMsg(held[p])
    /\ Len(queue) > 0
    /\ peeked' = [peeked EXCEPT ![p] = queue[1].id]
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `recv_identified`: the message leaves the queue AND TAKES ITS SLOT WITH IT. Nothing is announced
\* as free here - the message can still come back.
\* ONE MESSAGE PER RECEIVER, NOT ONE PER SYSTEM. This required the single global `held` to be empty,
\* so two receivers could never hold two deliveries at once - a state the implementation not only
\* permits but counts (`Channel::in_flight`) and has a test for
\* (`receives_in_flight_never_let_the_queue_pass_its_limit`). The two-receiver interleavings this
\* model exists to explore were therefore outside it.
Dequeue(p) ==
    /\ ~IsMsg(held[p])
    /\ Len(queue) > 0
    \* NAMED, NOT WHATEVER IS THERE. This is the whole of `recv_identified`: a receiver commits only
    \* the message it inspected, so a second receiver taking the peeked one in between makes this
    \* refuse rather than hand over a message whose shape was never checked.
    /\ peeked[p] = Head(queue).id
    /\ Len(booked[p]) >= Len(Head(queue).caps)
    /\ held' = [held EXCEPT ![p] = [Head(queue) EXCEPT !.slotHeld = TRUE]]
    \* AND EVERY OTHER RECEIVER THAT HAD INSPECTED THIS MESSAGE IS HOLDING A DEAD NUMBER.
    \*
    \* `recv_identified` answers `Superseded` to a receiver naming a message that is not at the head,
    \* and the caller's stored identity is worthless from that moment: `receive_transactionally`
    \* peeks again rather than retrying with it. Modeling the refusal as "the action is not enabled"
    \* is the same behaviour and keeps a value nothing can ever use out of the state - which is a
    \* great many states, one per stale identity per process, all describing the same system.
    /\ peeked' = [q \in Procs |-> IF q = p \/ peeked[q] # Head(queue).id THEN peeked[q] ELSE 0]
    /\ queue' = Tail(queue)
    /\ inflight' = inflight + 1
    /\ committed' = [committed EXCEPT ![p] = FALSE]
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, installed, bytes, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* The payload copy faulted BEFORE the commit: the message goes back to the head, still charged,
\* still holding the slot it never gave up, and the booking is released.
\*
\* EVERY BOOKING, NOT THE FIRST ONE. This removed `Head(booked[p])` and decremented the charge by
\* one, so a two-capability message reached the advertised `payload-failed` outcome with one slot
\* still booked and one unit of quota still held - the exact opposite of what the transition is named
\* for, and no invariant said so because a later standalone `Unbook` could tidy it up after the
\* syscall had already ended. Rust calls `release_reservation(message.caps.len())` once.
PayloadCopyFails(p) ==
    /\ IsMsg(held[p])
    /\ ~committed[p]
    /\ queue' = <<[held[p] EXCEPT !.slotHeld = FALSE]>> \o queue
    /\ inflight' = inflight - 1
    /\ held' = [held EXCEPT ![p] = NoMsg]
    \* The booking goes back with it - slot and quota together, exactly as `release_reservation`
    \* does, because the caller is going to peek again rather than install anything.
    /\ Len(booked[p]) > 0
    /\ LET mine == SlotsOf(booked[p]) IN
       /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                      IF i \in mine THEN [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]
                      ELSE table[p][i]]]
       /\ charge' = [charge EXCEPT ![p] = charge[p] - Cardinality(mine)]
    /\ booked' = [booked EXCEPT ![p] = <<>>]
    /\ outcome' = IF CoversModeled THEN "payload-failed" ELSE "none"
    /\ lastBatch' = IF CoversModeled THEN Len(held[p].caps) ELSE 0
    \* The receive is over, so the identity it inspected is gone with its stack - see `Unbook`.
    /\ peeked' = [peeked EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, xfer, xferSlot, installed, committed, bytes, nextId, lastUse, lastAsk, objgen>>

\* `commit_delivery`: the payload is in the caller's buffer. THE POINT OF NO RETURN - the queued
\* byte charge is released and the queue slot is really free.
CommitDelivery(p) ==
    /\ IsMsg(held[p])
    /\ ~committed[p]
    /\ committed' = [committed EXCEPT ![p] = TRUE]
    /\ held' = [held EXCEPT ![p] = [held[p] EXCEPT !.slotHeld = FALSE]]
    /\ inflight' = inflight - 1
    /\ bytes' = bytes - 1
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, installed, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `insert_reserved`: into the slots this reservation owns. Charges nothing - `reserve` already paid.
\*
\* THE WHOLE LOOP IS ONE ACTION, because it is one lock. `sys_channel_recv_caps` takes the handle
\* table and installs EVERY capability of the message before releasing it, so nothing - not a close,
\* not a termination, not another install - happens between the first handle and the last. Modeling
\* one action per capability put an interleaving point there that the code does not have, and the
\* cover written to celebrate it (`NoCloseBetweenTwoInstalls`, a close arriving between two
\* installs) was evidence for a behaviour of the specification alone.
Install(p) ==
    /\ IsMsg(held[p])
    /\ committed[p]
    /\ ~closed[p]
    /\ Len(held[p].caps) > 0
    /\ Len(booked[p]) >= Len(held[p].caps)
    /\ LET n == Len(held[p].caps)
           at == [k \in 1..n |-> booked[p][k]]
           mine == {at[k] : k \in 1..n}
       IN /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                          IF i \in mine
                            THEN [state |-> "Live",
                                  cap |-> held[p].caps[CHOOSE k \in 1..n : at[k] = i],
                                  gen |-> table[p][i].gen]
                            ELSE table[p][i]]]
          /\ booked' = [booked EXCEPT ![p] = SubSeq(booked[p], n + 1, Len(booked[p]))]
          /\ installed' = [installed EXCEPT ![p] = installed[p] \o at]
    /\ held' = [held EXCEPT ![p] = [held[p] EXCEPT !.caps = <<>>]]
    /\ UNCHANGED <<closed, charge, xfer, xferSlot, queue, inflight, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* `insert_reserved` INTO A CLOSED TABLE. The same barrier `restore_taken` stands behind: there is
\* nobody to install for, so the capability is dropped and the quota `reserve` charged is refunded -
\* which is what `close_all` would have done to this handle had it existed at the time.
InstallIntoClosed(p) ==
    /\ IsMsg(held[p])
    /\ committed[p]
    /\ closed[p]
    /\ Len(held[p].caps) > 0
    /\ Len(booked[p]) >= Len(held[p].caps)
    /\ LET n == Len(held[p].caps)
           mine == {booked[p][k] : k \in 1..n}
       IN /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                          IF i \in mine THEN [state |-> "Free", cap |-> NoCap, gen |-> table[p][i].gen]
                          ELSE table[p][i]]]
          /\ booked' = [booked EXCEPT ![p] = SubSeq(booked[p], n + 1, Len(booked[p]))]
          /\ charge' = [charge EXCEPT ![p] = charge[p] - Cardinality(mine)]
          /\ lastBatch' = IF CoversModeled THEN n ELSE 0
    /\ held' = [held EXCEPT ![p] = [held[p] EXCEPT !.caps = <<>>]]
    /\ outcome' = IF CoversModeled THEN "dropped-into-closed" ELSE "none"
    /\ UNCHANGED <<closed, xfer, xferSlot, queue, inflight, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, objgen>>

\* The handle numbers reached userspace. The capability is PUBLISHED and the receive is over.
Publish(p) ==
    /\ IsMsg(held[p])
    /\ committed[p]
    /\ Len(held[p].caps) = 0
    \* NOTHING LEFT TO INSTALL ENDS THE RECEIVE TOO. A message whose capabilities were all dropped -
    \* the table closed under it - still has a payload that was delivered and a count to write, so
    \* the syscall returns. A receive that could not end would be a message held forever.
    \*
    \* HOW MANY WENT OUT IS RECORDED WHERE IT HAPPENS. `installed` is emptied in this very action,
    \* so `outcome = "published" /\ Len(installed) = 2` was a state this transition could not
    \* produce; what refuted that cover was a LATER receive reaching two installed handles while
    \* this action's `outcome` still stood. `lastBatch` is written here, beside `outcome`, so the
    \* pair describes one publication and no other.
    /\ lastBatch' = IF CoversModeled THEN Len(installed[p]) ELSE 0
    /\ installed' = [installed EXCEPT ![p] = <<>>]
    /\ held' = [held EXCEPT ![p] = NoMsg]
    /\ committed' = [committed EXCEPT ![p] = FALSE]
    /\ outcome' = IF CoversModeled THEN "published" ELSE "none"
    \* The receive is over, so the identity it inspected is gone with its stack - see `Unbook`.
    /\ peeked' = [peeked EXCEPT ![p] = 0]
    /\ UNCHANGED <<table, closed, charge, booked, xfer, xferSlot, queue, inflight, bytes, nextId, lastUse, lastAsk, objgen>>

\* The handle-number copyout faulted AFTER the commit. The message cannot go back - its capabilities
\* have left it - so what is recoverable is recovered: every installed handle is closed.
\* A POST-COMMIT FAILURE NEVER RETURNS TO A QUEUED MESSAGE.
CopyoutFails(p) ==
    /\ IsMsg(held[p])
    /\ committed[p]
    /\ Len(held[p].caps) = 0
    /\ Len(installed[p]) > 0
    \* WHAT IS STILL THERE TO CLOSE. The kernel closes each installed handle by its raw number, and
    \* `close` checks the slot's GENERATION - so a handle whose slot has already been recycled by
    \* something else is refused rather than closed twice, and the caller's `let _ = table.close(..)`
    \* is right to ignore it. The refund therefore counts what was actually closed, not what was
    \* installed.
    /\ LET mine == {i \in 1..Slots :
                     /\ \E k \in 1..Len(installed[p]) : installed[p][k] = i
                     /\ table[p][i].state = "Live"} IN
       /\ table' = [table EXCEPT ![p] = [i \in 1..Slots |->
                      IF i \in mine THEN Recycle(p, i) ELSE table[p][i]]]
       /\ charge' = [charge EXCEPT ![p] = charge[p] - Cardinality(mine)]
    /\ lastBatch' = IF CoversModeled THEN Len(installed[p]) ELSE 0
    /\ installed' = [installed EXCEPT ![p] = <<>>]
    /\ held' = [held EXCEPT ![p] = NoMsg]
    /\ committed' = [committed EXCEPT ![p] = FALSE]
    /\ outcome' = IF CoversModeled THEN "copyout-failed" ELSE "none"
    \* The receive is over, so the identity it inspected is gone with its stack - see `Unbook`.
    /\ peeked' = [peeked EXCEPT ![p] = 0]
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, bytes, nextId, lastUse, lastAsk, objgen>>

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
    \* THE DERIVED CAPABILITY RECORDS WHERE IT CAME FROM, which is what `AuthorityNeverWidens` reads.
    /\ table' = [table EXCEPT ![p][j] =
                  [state |-> "Live", cap |-> [table[p][i].cap EXCEPT !.rights = r, !.from = table[p][i].cap.rights], gen |-> table[p][j].gen]]
    /\ charge' = [charge EXCEPT ![p] = charge[p] + 1]
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

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
    \* THE TYPE THE OPERATION ASKED FOR, recorded beside the capability it ran against. `lastUse`
    \* held only the capability, so `TypeSealing` could ask what type that capability HAS and not
    \* what type was WANTED - and "the capability's type is one of the types" is true by
    \* construction, so the invariant held whatever `Usable` did. The two are only equal because
    \* `Usable` makes them equal, which is the thing being checked.
    /\ lastAsk' = t
    /\ lastUse' = table[p][i].cap
    /\ UNCHANGED <<outcome, lastBatch, table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, objgen>>

\* `ObjectHeader::revoke`, WHICH IS TEST-ONLY IN THE TREE. Bumping the object's generation makes
\* every capability that captured the old one detectably stale. The production authority model has
\* no syscall that does this, so a configuration with `RevocationModeled` false is the one a
\* production claim may cite - and this action does not exist in it at all.
Revoke ==
    /\ RevocationModeled
    /\ objgen < MaxGen
    /\ objgen' = objgen + 1
    /\ UNCHANGED <<outcome, lastBatch, table, closed, charge, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk>>

Close(p, i) ==
    /\ ~closed[p]
    /\ table[p][i].state = "Live"
    /\ table' = [table EXCEPT ![p][i] = Recycle(p, i)]
    /\ charge' = [charge EXCEPT ![p] = charge[p] - 1]
    /\ UNCHANGED <<closed, booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, peeked, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

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
    \* A closed table receives nothing, so an identity it inspected is one nothing can act on -
    \* UNLESS a delivery is in hand, whose identity is the one `MessageIdentityStable` reads. A close
    \* does not reach inside a receive that is already past its take: `InstallIntoClosed` is what
    \* answers for that message, and it needs the message to still be describable.
    /\ peeked' = [peeked EXCEPT ![p] = IF IsMsg(held[p]) THEN peeked[p] ELSE 0]
    /\ UNCHANGED <<booked, xfer, xferSlot, queue, inflight, held, installed, committed, bytes, nextId, lastUse, lastAsk, outcome, lastBatch, objgen>>

\* A SYSTEM WITH NOTHING LEFT TO DO IS NOT A DEADLOCK. Every process has terminated, no transfer is
\* outstanding and no message is in flight - so the only behaviour left is to stay there. Saying that
\* in the specification is better than switching off the check that would otherwise report it, which
\* would also stop it reporting a state that IS stuck.
Done ==
    /\ \A p \in Procs : closed[p]
    /\ \A p \in Procs : Len(xfer[p]) = 0
    /\ \A p \in Procs : ~IsMsg(held[p])
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
    \* THE PRE-COMMIT FAILURE, WHICH THIS RELATION DID NOT CONTAIN. It was defined, commented and
    \* never taken - so every invariant passed over a model in which a receive could not fail before
    \* its commit, which is one of the two outcomes the whole milestone is about. The cover property
    \* `NoPayloadFailure` is what noticed: it could not be refuted.
    \/ \E p \in Procs : PayloadCopyFails(p)
    \/ \E p \in Procs : Install(p) \/ InstallIntoClosed(p) \/ Publish(p) \/ CopyoutFails(p) \/ Terminate(p)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* THE SAFETY INVARIANTS. Every one is a sentence from `MODEL_MAP.md`, and  *)
(* the names are fixed so that a configuration cannot quietly check a      *)
(* weaker property under a familiar name.                                  *)
(***************************************************************************)

\* EVERY PLACE A CAPABILITY MAY BE, and every capability in it.
\*
\* These counted the FIRST queued message and nothing behind it, and the queue holds more than one
\* message in `transactions-single.cfg` - so a second queued capability was in the state space, was
\* explored, and was looked at by none of the three invariants below. `AllCaps` is the whole set,
\* named once, so an invariant cannot quantify over less than the model holds by accident.
CapsInSlots == {table[p][i].cap : <<p, i>> \in {<<q, j>> \in Procs \X (1..Slots) : HoldsCap(table[q][j])}}
CapsInXfer == UNION {{xfer[p][k] : k \in 1..Len(xfer[p])} : p \in Procs}
CapsInQueue == UNION {{queue[m].caps[k] : k \in 1..Len(queue[m].caps)} : m \in 1..Len(queue)}
CapsInHeld == UNION {{held[p].caps[k] : k \in 1..Len(held[p].caps)} : p \in Procs}
AllCaps == CapsInSlots \cup CapsInXfer \cup CapsInQueue \cup CapsInHeld

\* How many authority-bearing copies of ONE OBJECT's capability exist, wherever they are.
CopiesOf(o) ==
    Cardinality({<<p, i>> \in Procs \X (1..Slots) : HoldsCap(table[p][i]) /\ table[p][i].cap.obj = o})
      + Cardinality({<<p, k>> \in Procs \X (1..BatchMax) : k =< Len(xfer[p]) /\ xfer[p][k].obj = o})
      + Cardinality({<<m, k>> \in (1..QueueLimit) \X (1..BatchMax) :
                       /\ m =< Len(queue)
                       /\ k =< Len(queue[m].caps)
                       /\ queue[m].caps[k].obj = o})
      + Cardinality({<<p, k>> \in Procs \X (1..BatchMax) :
                       /\ k =< Len(held[p].caps)
                       /\ held[p].caps[k].obj = o})

\* TRANSFER IS LINEAR: absent DUPLICATE, one capability has exactly one authority-bearing owner
\* through take, queue, delivery, commit and rollback. Success neither copies it nor loses it.
\*
\* PER OBJECT, NOT OVER THE WHOLE SYSTEM. This used to add every location's total and require the
\* sum to be at most one, which says something stronger than linearity and different from it: that
\* the whole machine holds at most one capability. Two capabilities for two objects is an ordinary
\* state and this reported it as a violation - so a configuration could not model a batch of two and
\* keep this invariant, and the batch configuration was left with one object and an unreachable
\* batch rather than the invariant being stated correctly.
\*
\* EXACT ONLY WHILE NOTHING DUPLICATES, which is why the two configurations that model
\* `HandleTable::duplicate` do not check it: there, two capabilities for one object are two owners
\* and not a violation, and telling them apart needs an instance identity this model does not carry.
TransferIsLinear == \A o \in Objects : CopiesOf(o) =< 1

\* AUTHORITY NEVER WIDENS: nothing anywhere carries more than the capability it descends from, and
\* a transfer adds nothing at all.
\*
\* STATED AT EVERY LINK, NOT ONLY AGAINST THE MINT. This was `c.rights \subseteq MintedRights`, which
\* is a ceiling rather than a chain: a derive that handed out MORE than its source satisfied it as
\* long as the result stayed under the mint, so removing the source-rights guard from `Duplicate` did
\* not violate the invariant named for that guard. `from` carries the source's rights, and a mint's
\* source is itself - so the second conjunct is the old statement, and the first is the one the name
\* has always promised.
AuthorityNeverWidens ==
    \A c \in AllCaps :
      /\ c.rights \subseteq c.from
      /\ c.from \subseteq MintedRights

\* NO FORGERY: every capability that exists names an object that was minted, at the generation it
\* was minted against. Nothing in the transition relation can produce another.
NoForgery == \A c \in AllCaps : c.obj \in Objects /\ c.objgen = 1

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
    \A p \in Procs :
      /\ ~IsMsg(held[p]) => (Len(installed[p]) = 0 /\ ~committed[p])
      /\ committed[p] => (IsMsg(held[p]) \/ Len(installed[p]) > 0)

\* A FAILED SEND RESTORES THE WHOLE BATCH, OR REACHES A DOCUMENTED TERMINAL OWNER. Stated as the
\* contract `take_for_transfer` promises: a slot is `Reserved` only while its process is holding the
\* capability that came out of it. No outstanding batch means no reservation left behind - which is
\* what "exactly one of commit, restore or abandon follows" means, seen as a state rather than as a
\* sequence.
FailedSendRestores ==
    \A p \in Procs :
      \* The capabilities may have gone into the queue while their slots are still reserved - that
      \* is the window between `send` and `commit_taken`, and it is exactly where a termination can
      \* arrive. What may never happen is the other way round: a capability in hand whose slot is
      \* not held for it.
      /\ Len(xfer[p]) =< Len(xferSlot[p])
      /\ (Len(xferSlot[p]) = 0) => \A i \in 1..Slots : table[p][i].state # "Reserved"
      /\ \A k \in 1..Len(xferSlot[p]) : table[p][xferSlot[p][k]].state = "Reserved"

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
\* PER RECEIVER, WHICH IS THE ONLY WAY IT SAYS ANYTHING. Against one global `peeked` this compared a
\* receiver's message with whatever identity was inspected LAST, by anybody - so a second receiver
\* peeking over the top of the first, then the first taking the newly-named message, satisfied it.
MessageIdentityStable == \A p \in Procs : IsMsg(held[p]) => held[p].id = peeked[p]

\* TYPE SEALING: whatever the last abstract operation ran against had the type it was asked for, the
\* right it needed and the object's live generation. A capability that carried less could not have
\* been used, which is `lookup_typed` refusing rather than a caller remembering to check.
TypeSealing ==
    IsCap(lastUse) => /\ lastUse.type = lastAsk
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
    \A p \in Procs : (IsMsg(held[p]) /\ ~committed[p]) => Len(installed[p]) = 0
(***************************************************************************)
(* THE COVER PROPERTIES, WRITTEN AS THEIR OWN NEGATIONS.                    *)
(*                                                                         *)
(* An invariant that passes because its dangerous action is never enabled   *)
(* is a failed gate, and nothing in a passing run says which of the two     *)
(* happened. So each of these says "this never happens" - and the gate      *)
(* requires TLC to REFUTE it. A cover that stops being refuted is a         *)
(* transition that has quietly become unreachable.                          *)
(*                                                                         *)
(* They read the `outcome` ghost, which a configuration turns on: reaching  *)
(* a transition is proved as cheaply in the smallest configuration as in    *)
(* the largest, and the ghost triples a state space wherever it is on.      *)
(***************************************************************************)

NoPublish == outcome # "published"
NoCopyoutFailure == outcome # "copyout-failed"
NoPayloadFailure == outcome # "payload-failed"
NoRestore == outcome # "restored"
NoAbandon == outcome # "abandoned"
NoDropIntoClosed == outcome # "dropped-into-closed"

\* Generation exhaustion, which is the boundary `MaxGen` exists to make reachable.
NoRetirement == \A p \in Procs : \A i \in 1..Slots : table[p][i].state # "Retired"

\* A close arriving while a transfer is outstanding - the interleaving this whole model was written
\* for. If this stops being refuted, the race is no longer being explored.
NoCloseRacingTransfer ==
    \A p \in Procs : ~(closed[p] /\ \E i \in 1..Slots : table[p][i].state = "Reserved")

\* Both ways a receive can end, and both must be reachable: one of them is the point of no return.
NoDeliveredCapability == \A i \in 1..Slots : ~HoldsCap(table[Receiver][i])

\* THE BATCH IS REALLY A BATCH, AND THE QUEUE REALLY HOLDS MORE THAN ONE.
\*
\* These exist because the batch configuration did not model a batch. It set `BatchMax = 2` and one
\* object, and `Init` minted one capability - so no second capability could be put anywhere, the
\* all-or-nothing rule was checked over a batch of length one, and the duplicate-source refusal the
\* configuration was named for was unreachable. Nothing said so: every invariant held, the run
\* finished, and a passing result was published for a model that could not reach its own subject.
\*
\* Each is refuted where its configuration is supposed to reach it, and that refutation is the
\* evidence. Where a configuration is NOT supposed to - a queue of two needs `QueueLimit > 1` - the
\* property is simply not listed in it.
NoBatchOfTwo == \A p \in Procs : Len(xfer[p]) < 2
NoMessageOfTwo == \A m \in 1..Len(queue) : Len(queue[m].caps) < 2
NoTwoQueuedMessages == Len(queue) < 2

\* And the rollback that only means something at length two: a refused send returns ALL of them.
NoBatchOfTwoRestored == ~(outcome = "restored" /\ Cardinality({o \in Objects : CopiesOf(o) = 1}) > 1)

\* AND THE RECEIVE HALF OF THAT BATCH, WHICH NOTHING COVERED.
\*
\* The three above are all about the SENDER: a batch of two taken, a message of two queued, a refused
\* send returning both. The receiver's side was never asked about, and it was unreachable - `Book`
\* capped a receiver at one booking while `Dequeue` demanded one per capability, so a two-capability
\* message could be built and could never be taken. Each of these is refuted where the batch
\* configuration reaches it, and that refutation is what says the whole lifecycle runs.
NoTwoBookings == \A p \in Procs : Len(booked[p]) < 2
NoTwoCapMessageDequeued == \A p \in Procs : ~(IsMsg(held[p]) /\ Len(held[p].caps) = 2)
NoTwoCapsInstalled == \A p \in Procs : Len(installed[p]) < 2

\* PUBLICATION OF TWO, ASKED OF THE ACTION THAT PUBLISHED THEM.
\*
\* This read `Len(installed) = 2` beside `outcome = "published"`, and `Publish` empties `installed`
\* in the same action that sets `outcome` - so the publication of two handles produced a state with
\* ZERO installed and could not refute it. What refuted it was a later receive reaching two installed
\* handles while the earlier `published` outcome still stood: a true statement about two different
\* batches, reported as evidence for one. `lastBatch` is written by `Publish` and by nothing else
\* that leaves `outcome` at "published", so the pair below names one publication.
NoTwoCapsPublished == ~(outcome = "published" /\ lastBatch = 2)
NoTwoCapPayloadFailure == ~(outcome = "payload-failed" /\ Len(queue) > 0 /\ Len(Head(queue).caps) = 2)

\* A BATCH OF TWO DROPPED INTO A CLOSED TABLE, which replaces `NoCloseBetweenTwoInstalls`.
\*
\* That one asked for a close arriving BETWEEN two installs - `outcome = "dropped-into-closed" /\
\* Len(installed) = 1` - and `sys_channel_recv_caps` holds the handle table across the whole install
\* loop, so there is no such moment in the code. The cover was refutable only because the model split
\* the loop into one action per capability, which is to say it was evidence for the specification's
\* own extra interleaving. The real transition is the one the close CAN interrupt: it arrives before
\* the install, and the whole batch is dropped and refunded together.
NoBatchOfTwoDroppedIntoClosed == ~(outcome = "dropped-into-closed" /\ lastBatch = 2)
=============================================================================
