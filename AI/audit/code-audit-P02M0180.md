# P02M0180 implementer notes

## The shared catalogue stage (2026-09-22)

This is the milestone's second contract item and it is deliberately first in the work: it is the one
piece P02M0181 through P02M0187 all need, and none of them should wait for a Bluetooth stack to get
it.

**What the shape was.** `provider-catalogue`'s root answers the reserved CONNECT opcode. That
opcode takes no arguments, so the connection it mints cannot be narrowed - every consumer got the
whole closed vocabulary, and the only thing between that authority and its use was the consumer's
own restraint. Nine manifest rows mint one on an ordinary boot.

**What it is now.** A second serve root, `provider-catalogue-admin`, whose one operation takes the
kind subset. Its root is delivered to ServiceManager alone, because the supervisor is the only
program that knows which service was declared to need which kinds - the manifest row is that
declaration - so the minting authority never reaches a service.

**Three things were worth more than the feature.**

1. THE ENFORCEMENT MUST READ THE SERVER'S RECORD, NOT THE REQUEST. `open` takes a `provider-info`
   whose `kind` field the caller filled in. The lookup already requires that field to match the
   entry at that slot and generation, so checking the caller's field would have been correct today
   and silently wrong the first time that lookup was relaxed. It reads the entry.

2. THE REFUSAL HAS TO NAME THE KIND AND THE SUBSET. The first enforced boot printed one refusal and
   nothing else, and nothing in the tree could say which of nine consumers had produced it - six
   manifest rows were candidates and all six looked right. Adding the kind narrowed it to `usb-bus`
   and adding the subset (`0`, the inventory connection) said it was not a manifest row at all: it
   was a hand-written bootstrap branch minting through the catalogue's own root. A manifest change
   could not have reached it and no test would have noticed, because until the subset was
   enforceable an unrestricted connection behaved exactly like a correct one.

3. A FIXED BUFFER IS A PANIC AND NOT A TRUNCATION. The diagnostic above was written into a 96-byte
   array whose two literals are 113 bytes between them. The index panicked, DeviceManager died
   mid-bring-up, and the boot stopped with every driver bound and no error anywhere - which reads
   exactly like the hang the previous mistake produced, from an entirely different cause. Both were
   found by reading the log rather than by a test, which is worth remembering about this program:
   its failures are silences.

**What is not covered.** The in-guest denial test runs in the development configuration, like the
three tests beside it in that module; the shipping boot image does not run any of them. The
`development-build` gate compiles that configuration and the scenarios harness replays it.
