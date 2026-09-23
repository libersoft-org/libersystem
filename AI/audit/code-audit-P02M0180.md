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

## The host-testable leaf (2026-09-22)

Six modules, 39 tests. The order they were written in is the order they depend on each other, and
each was checked against a document before the next was started.

**Where the vectors came from.** FIPS-197 for AES, RFC 4493 for CMAC, Core Appendix D for f4/f5/f6/g2
and for the Core's own `e` sample. The owner approved fetching the specification for the Appendix D
sample data; the fetch was a read of a public document and nothing left this machine.

**One vector was written from memory and was wrong.** A second Core `e` sample, typed before the
document was fetched, failed - while FIPS-197 Appendix B, FIPS-197 C.1, the all-zero known answer
and the first Core sample all passed, so the cipher was right and the expectation was not. It was
removed rather than corrected to whatever the code produced. That is the whole argument for the
rule the test files state: an expected value that cannot be sourced proves only that the
implementation agrees with whoever typed it.

**What the document gave that memory could not.**

- The f5 salt could be CONFIRMED rather than asserted, because D.3 prints the intermediate `T`.
  Nothing else in the derivation would have caught a wrong salt - every downstream value would have
  been wrong together and consistently.
- Which of the two f5 counters is the MacKey and which is the LTK. The flattened table is ambiguous
  about which label belongs to which block; D.4 resolves it, because f6's sample is keyed with the
  number counter zero produces. A guess here is the defect that still completes a pairing.
- `keyID` is `62746c65`, which is ASCII `btle` - one letter away from what memory offered.

**What is deliberately absent.** No P-256: the controller owns it, and the milestone requires a
controller that does. No inverse AES: CMAC is forward-only, and code that exists and is never
exercised is code nothing would notice breaking. No constant-time claim: the S-box is a table, the
module says so in its own header, and the threat this service faces is a peer on a radio rather
than a process that can measure a cache.
