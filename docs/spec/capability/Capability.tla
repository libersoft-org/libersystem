------------------------------ MODULE Capability ------------------------------
(***************************************************************************)
(* The reusable half: what a handle slot, a capability, a message and the   *)
(* three ledgers ARE, with no variables and no actions.                     *)
(*                                                                         *)
(* It is separate from `Transfer` so the composed specification is only the *)
(* transitions - and so a second configuration (the test-only revocation    *)
(* one, which is not the production claim) can reuse these definitions      *)
(* without inheriting the production transition relation.                   *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Procs,          \* the processes with handle tables
    Objects,        \* the kernel objects a capability can name
    Slots,          \* how many slots one table has
    QueueLimit,     \* the endpoint's queue bound
    MaxGen          \* the abstract generation ceiling; retirement is reachable at it

(*************************************************************************)
(* Rights. A finite set: USE abstracts a type-correct operation on the     *)
(* object, DUPLICATE and TRANSFER are the two the capability rules name.   *)
(*************************************************************************)
Rights == {"USE", "DUPLICATE", "TRANSFER"}
RightSets == SUBSET Rights

(*************************************************************************)
(* A capability names an object, carries a right set, and REMEMBERS the    *)
(* object generation it was made against - which is what makes a handle to *)
(* a destroyed object detectable rather than merely wrong.                 *)
(*************************************************************************)
NoCap == [obj |-> "none", rights |-> {}, objgen |-> 0]
Caps == [obj: Objects, rights: RightSets, objgen: 1..MaxGen] \cup {NoCap}
IsCap(c) == c # NoCap

(*************************************************************************)
(* The six slot states of `HandleTable`. `Booked` is a slot taken out of   *)
(* circulation by `reserve` and not yet installed into; `Reserved` is the  *)
(* transfer's - `take_for_transfer` emptied it and exactly one of commit,  *)
(* restore or abandon will follow.                                         *)
(*************************************************************************)
SlotStates == {"Free", "Live", "Reserved", "Booked", "Retired"}

EmptySlot == [state |-> "Free", cap |-> NoCap, gen |-> 1]

(* A slot holds authority in exactly these two states. Everything the      *)
(* handle ledger charges for is one of them plus a booking.               *)
HoldsCap(s) == s.state = "Live"

(*************************************************************************)
(* The handle ledger: one charge per live handle, per booked slot, and per *)
(* slot whose capability is out on a transfer. A take does not refund and  *)
(* a restore does not charge, which is why `Reserved` counts here.         *)
(*************************************************************************)
ChargedSlots(table) == Cardinality({i \in DOMAIN table : table[i].state \in {"Live", "Booked", "Reserved"}})

(*************************************************************************)
(* A message: an identity, the capabilities it carries, and whether it is  *)
(* still holding the queue slot it was taken from.                         *)
(*************************************************************************)
NoMsg == [id |-> 0, caps |-> <<>>, slotHeld |-> FALSE]
IsMsg(m) == m.id # 0

(*************************************************************************)
(* Where a capability may be. THERE IS NO "IN THE KERNEL SOMEWHERE": the   *)
(* linear-transfer invariant is stated over exactly these places.          *)
(*************************************************************************)
Places == {"slot", "xferlocal", "queued", "delivery", "installed"}
=============================================================================
