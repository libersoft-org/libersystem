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
    Types,          \* the object types, distinct atoms so type sealing is checked rather than assumed
    Slots,          \* how many slots one table has
    QueueLimit,     \* the endpoint's queue bound
    MaxGen,         \* the abstract generation ceiling; retirement is reachable at it
    MaxId,          \* how many message identities one behaviour may mint - see `Transfer`'s Enqueue
    MintedRights,   \* the rights the one minted capability carries; a configuration's lever
    RevocationModeled, \* whether `ObjectHeader::revoke` exists in this configuration. It is TEST-ONLY
                       \* in the tree, so the production result may not be cited from a run with it
                       \* enabled - which is why it is a constant and not an always-available action.
    CoversModeled,  \* whether the `outcome` ghost records anything. It exists for the COVER
                    \* properties - which need the smallest configuration to reach them, not the
                    \* largest - and it triples a state space wherever it is on. Off, the variable is
                    \* constant and costs nothing, which is what keeps `propagation` affordable.
    BatchMax,       \* how many capabilities one send may carry. `sys_channel_send_caps` takes a
                    \* batch and returns ALL of them when it is refused, so one is not enough to see
                    \* the rule that matters: two is.
    DerivedRights   \* the right sets a duplicate may be asked for. A SET OF SETS, and small on
                    \* purpose: quantifying a duplicate over every subset of the minted rights is
                    \* the Cartesian widening M3 refuses, and it buys nothing the two interesting
                    \* cases - keep everything, narrow to one - do not already say.

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
\* THE TYPE TRAVELS WITH THE CAPABILITY, because a lookup checks it: `lookup_typed` refuses a handle
\* whose object is not the type the caller asked for, and that refusal is authority rather than
\* convenience - a channel handle used as a memory object would be a type confusion inside the
\* kernel, reachable from ring 3.
NoCap == [obj |-> "none", type |-> "none", rights |-> {}, objgen |-> 0]
Caps == [obj: Objects, type: Types, rights: RightSets, objgen: 1..MaxGen] \cup {NoCap}
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

=============================================================================
