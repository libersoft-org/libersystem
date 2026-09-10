IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0174 (2026-09-10T09:50:00Z):

Scope: `docs/todo/P02M0174.md` - a bounded IPv6 host layer beside the existing IPv4 stack. Status
line: "FUTURE TRACK. NETWORKING FOUNDATION. NOT A PHASE-2 COMPLETION GATE."

Assessment, reported rather than half-implemented. The tree read before deciding:

- The L3 stack lives in `src/user/services/core/net.rs` and `network_service.rs` (66 kB), with the
  wire types in `network-proto`. `Stack::Outcome` carries `reply_len` plus ONE ephemeral `Event`,
  and the standing NetworkService loop bounds its wait only by the DHCP lease deadline, pumping
  frames and discarding every event that is not the one a blocking helper waits for.

- The plan's own M6 states this directly: "Nothing above can work inside the current
  one-ephemeral-event-per-frame model, so this item is a prerequisite of M3-M5 rather than a cleanup
  after them." So M1 (typed values) and M2 (strict parsing) do not compose into a working host on
  their own - ICMPv6 (M3), neighbour discovery with the full NUD state machine (M4), and DAD/SLAAC
  with hostile-RA handling (M5) all require the timer/event/interface seam (M6) with the hard
  admission table it specifies (16 unicast addresses, 15 prefixes, 32 routes, 8 routers, 64 ND
  neighbours, and the rest), and M7/M8 add a controllable unprivileged peer fixture and the QEMU
  positive/hostile proofs.

This is a large, self-contained networking milestone - a complete bounded IPv6 host with a named
conformance gap (atomic fragments only, non-atomic fragments refused and counted) - held to this
repository's standard: every rule with a host test that is watched to fail under a mutation, hostile
packet fixtures, and a QEMU proof against a controllable peer. It is roughly 980 lines of
specification across eight interlocking items, and its foundational items are explicitly not
independently functional without the seam rework.

Decision: nothing implemented. Delivering M1+M2 alone would add typed IPv6 addresses and a parser
that nothing calls, inside a service loop the plan says cannot carry the state above them - a
partial, non-functional layer, which is the stub/placeholder outcome the job forbids. Half-building
the NUD or SLAAC state machines without their seam is worse. This milestone needs its own focused
implementation run; it is flagged here rather than partially built inside a batch whose other
members are unrelated to it. Scaling it down or scheduling it is the project owner's call.

What a dedicated run would do, in the plan's own order: M6 (the timer/event/interface seam with the
admission table) first, then M1/M2 (typed values, strict bounded parsing), then M3 (ICMPv6), M4
(neighbour discovery + NUD), M5 (DAD/SLAAC + hostile RA), then M7/M8 (the peer fixture and the
host+QEMU proofs), with IPv4 operational throughout.

Verification: not applicable - no change was made. Commands run: read of `docs/todo/P02M0174.md`,
`src/user/services/core/net.rs` and `network_service.rs` shapes, `network-proto`.
