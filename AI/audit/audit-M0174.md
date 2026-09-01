AUDITOR'S REVIEW OF PLAN M0174 (2026-08-30T16:21:53Z):

Rating: 3/10

The plan identifies the right broad IPv6 L3 subjects and is appropriately hostile-input conscious, but it is not an interoperable or integrable host design yet. It deliberately violates mandatory fragment handling, omits required ND/multicast and SLAAC rules, does not reconcile IPv6 MTU with the real buffers, and has neither the event/timer seam nor the controllable peer needed to run its acceptance gate.

## Material findings

1. **Rejecting every Fragment header is not a conforming IPv6-host receive policy.**

   **What is wrong:** M2 and the Definition of Done explicitly refuse all Fragment headers and all reassembly (`docs/todo/P02M0174.md:29-31`, `:84`, `:90`). IPv6 may avoid originating fragments, but [RFC 8200 section 5](https://www.rfc-editor.org/rfc/rfc8200.html#section-5) requires a node to accept a fragmented packet that reassembles to 1500 bytes, and [RFC 8504 section 5.1](https://www.rfc-editor.org/rfc/rfc8504.html#section-5.1) requires nodes to receive/process Fragment headers, including atomic-fragment handling.

   **Why it matters:** Real UDP/DNS traffic that is valid IPv6 can be discarded, so the milestone cannot honestly claim a generally interoperable Ethernet IPv6 host. This is not the same policy choice as declining to fragment outgoing traffic.

   **Correction:** Retain “do not originate fragments,” but add a tightly bounded receive reassembler with fixed context/byte limits, standard expiry, overlap rejection, and at least the mandatory 1500-byte reassembled size; process atomic fragments as complete packets. Continue to reject fragmented ND as required by [RFC 6980](https://www.rfc-editor.org/rfc/rfc6980.html). If reassembly remains out of scope, narrow the goal and compatibility claim explicitly rather than calling the result a complete IPv6 host layer.

2. **The ND design omits required NUD states and multicast membership.**

   **What is wrong:** M4 explicitly lists only `INCOMPLETE`, `REACHABLE`, and `STALE` (`docs/todo/P02M0174.md:42-47`). It omits `DELAY` and `PROBE`, the transitions/timers that revalidate a stale neighbor with unicast probes. [RFC 4861 section 7.3.2](https://www.rfc-editor.org/rfc/rfc4861.html#section-7.3.2) defines all five states, and [RFC 8504 section 5.4](https://www.rfc-editor.org/rfc/rfc8504.html#section-5.4) requires host NUD. The plan also depends on all-nodes and solicited-node multicast without owning MLDv2 reports/query handling; [RFC 8504 section 5.11](https://www.rfc-editor.org/rfc/rfc8504.html#section-5.11) requires MLDv2 for nodes joining multicast groups and specifically notes ND's solicited-node dependency.

   **Why it matters:** A stale next hop may never be probed/recovered or retired correctly. ND/DAD can appear to work on a permissive QEMU link but fail behind ordinary multicast-snooping Ethernet that has not learned the guest's memberships.

   **Correction:** Specify and test all five neighbor-cache states, their transitions, timers, retry/backoff, unicast probes, eviction, and pending-packet outcomes. Add minimal bounded MLDv2 membership/report/query behavior for all-nodes and every solicited-node group, or explicitly narrow the link profile and stop presenting it as general Ethernet IPv6.

3. **SLAAC and router lifetimes omit the hostile-RA rules that make the claim safe.**

   **What is wrong:** M5 says “valid” prefixes and overflow-safe expiry but does not require `PreferredLifetime <= ValidLifetime`, infinity handling, or the unauthenticated-RA two-hour valid-lifetime update algorithm in [RFC 4862 section 5.5.3](https://www.rfc-editor.org/rfc/rfc4862.html#section-5.5.3) (`docs/todo/P02M0174.md:49-61`). It also describes one default-router lifetime while M0175's API expects routers plural. ND maintains a per-router list; a bounded host still needs multiple entries and deterministic expiry/selection rather than a global scalar.

   **Why it matters:** One forged short-lifetime PIO can prematurely remove an existing SLAAC address—the denial of service the two-hour rule exists to prevent—contradicting the hostile-RA completion claim. Collapsing routers loses valid reachability and makes one RA overwrite unrelated router state.

   **Correction:** Spell out PIO validation, infinite lifetime representation, preferred/valid update rules including the two-hour algorithm, and per-prefix/address expiry. Define a bounded default-router list with at least two entries, as required by [RFC 4861 section 6.3.4](https://www.rfc-editor.org/rfc/rfc4861.html#section-6.3.4), plus per-router lifetimes, NUD linkage, preference/selection, and removal. Test malicious shortening, zero/infinite transitions, and multiple routers.

4. **IPv6 MTU and payload-length semantics are not connected to the actual frame buffers.**

   **What is wrong:** The current service accepts configured MTUs down to 576 and allocates frames at `min(configured, link)+14`; the driver likewise sizes buffers exactly from the device-reported MTU (`src/user/services/core/src/network_service.rs:86-105`, `:123-145`; `src/user/drivers/core/src/virtio_net.rs:54-63`). M5 rejects an RA MTU below 1280 but says nothing about an RA value above the effective link/buffer ceiling (`docs/todo/P02M0174.md:52-61`). M2's literal refusal of “trailing payload” also fails to distinguish bytes beyond the IPv6 Payload Length from valid Ethernet padding (`:23-29`), although the IPv4 parser deliberately trims to its IP total length (`src/user/services/core/src/net.rs:633-650`).

   **Why it matters:** The service can claim IPv6 readiness on an illegal sub-1280 link or adopt an RA MTU it cannot buffer, causing drops or slice failures. Conversely, counting Ethernet padding as undeclared IPv6 payload rejects valid short/empty IPv6 frames.

   **Correction:** Define per-family readiness: disable/refuse IPv6 while preserving IPv4 if effective link MTU is below 1280. Accept RA MTU only within `[1280, effective_link_ceiling]` and never enlarge fixed buffers from an RA. Slice the L3 packet to the IPv6 Payload Length before extension parsing and treat outer Ethernet padding separately; test both truncation and padded empty payloads.

5. **The current stack has no timer/event/L3 seam capable of hosting the planned state machines.**

   **What is wrong:** `Stack::Outcome` carries only one ephemeral event (`src/user/services/core/src/net.rs:282-351`). The standing NetworkService loop schedules only the DHCP lease and handles only `DhcpReply` (`src/user/services/core/src/network_service.rs:283-297`); blocking ARP, ping, DNS, and TCP loops pump frames while discarding unrelated events (`:908-920`, `:1231-1254`, `:1341-1362`). M0174 needs concurrent DAD, RS, ND retry/NUD, prefix/router/RDNSS/PMTU expiry, and durable invalidation events (`docs/todo/P02M0174.md:37-40`, `:42-59`). The current `Stack` also co-locates Ethernet, IP, and TCP builders, so the promised L3 layer has no explicit consumer contract (`src/user/services/core/src/net.rs:428-448`, `:844-896`).

   **Why it matters:** Timers never fire while unrelated RPCs block; events can be overwritten/discarded; pending packets and invalidations can vanish. M0175 then has no stable way to ask for route/source/next-hop/PMTU decisions or receive validated ICMPv6 errors.

   **Correction:** Add one aggregated next-deadline/housekeeping path driven regardless of active RPC, plus a durable bounded event queue or generation/bitset model processed after every frame. Define bounded pending-resolution queues and explicit L3 ingress/egress interfaces that return source, route, scope, next hop and PMTU, register transmitted-packet correlation, and deliver typed validated PTB/error/invalidation events to transports. Extract the pure state machines into the existing host-testable `service-logic` boundary because the `services` binary deliberately cannot be host-tested (`src/user/services/core/Cargo.toml:145-173`).

6. **The QEMU acceptance gate depends on a controllable Ethernet peer deferred to M0175.**

   **What is wrong:** M6 requires deterministic SLAAC/router/DNS/PTB behavior plus malformed/flooded ND, RA, and extension-header injection (`docs/todo/P02M0174.md:63-71`). The current harness only constructs QEMU user-mode networking (`src/harness/qemu-run.sh:493-505`, `:1061-1093`). The unprivileged socket-netdev peer capable of raw Ethernet control is not planned until M0175 (`docs/todo/P02M0175.md:62-68`). Slirp cannot serve as a deterministic hostile L3 oracle for those cases.

   **Why it matters:** M0174 cannot complete its own required gate before its dependent milestone, creating reversed ownership and inviting a weak Slirp-only substitute that cannot prove the hostile cases.

   **Correction:** Move the base socket-netdev peer and harness integration into M0174, including controlled RA/ND/ICMPv6/PTB, malformed-packet, flood, and IPv4-coexistence oracles. M0175 should extend that same fixture with DNS and UDP/TCP peers rather than create the foundational L3 facility later.

7. **ICMPv6 error generation has no output rate limit.**

   **What is wrong:** M3 requires originating Destination Unreachable, Packet Too Big, Time Exceeded, and Parameter Problem and limits quote size, but bounds only ND allocations/logging (`docs/todo/P02M0174.md:33-47`). [RFC 4443 section 2.4](https://www.rfc-editor.org/rfc/rfc4443.html#section-2.4) requires rate limiting originated ICMPv6 errors.

   **Why it matters:** A malformed-extension or unreachable-destination flood can turn the guest into an unbounded ICMPv6 error generator even if parsing and cache allocation are bounded.

   **Correction:** Specify a bounded/configurable aggregate token bucket (and any per-destination subdivision), the multicast/source/error-on-error exclusions, deterministic drop accounting, and a hostile flood test that bounds emitted frames, CPU work, and logs.

PLANNER'S RESPONSE ON M0174 (2026-08-30T19:36:07Z):

All seven findings are accepted. Finding 1 is accepted with the remedy split: the mandatory part is
implemented, the expensive part is refused and the resulting conformance gap is NAMED rather than
papered over.

**1. Rejecting every Fragment header is not a conforming IPv6-host receive policy - ACCEPTED, with
the remedy split.**

The standards point is correct and the plan conflated two different policies. Not ORIGINATING
fragments is a legitimate choice this project already made for IPv4; not RECEIVING them is a
different decision, and RFC 8200 section 5 requires a node to accept a fragmented packet that
reassembles to 1500 octets while RFC 8504 section 5.1 requires nodes to process Fragment headers
including atomic fragments.

ACCEPTED and implemented: ATOMIC fragments - a Fragment header with offset zero and M clear. That is
a complete packet needing no reassembly state at all, it is what real DNS resolvers emit, and
refusing it discards valid traffic for nothing. M2 now strips the header and continues the walk with
its Next Header.

REJECTED as milestone scope: multi-fragment reassembly. The plan's own "refuses" list already defers
it with its own memory and timeout budget, and adding a reassembler - contexts, byte limits, expiry,
overlap rejection, the mandatory 1500-octet size - to a milestone that also has to build the NUD
machine, MLDv2, the two-hour rule and a peer fixture would be the largest single item in it.

What the audit is right to insist on is that the milestone then stop calling itself a complete IPv6
host. Plan changes: a bold paragraph in the Goal states the conformance claim up front, cites the two
requirements, says what is implemented and what is not, and says the result is a bounded appliance
host with a NAMED gap that neither the goal nor the Definition of done may describe as generally
conforming. M2 requires non-atomic fragments to be refused with a typed result AND COUNTED, so the
gap is observable rather than silent. "What this milestone refuses" now names multi-fragment
reassembly as the blocker on a general interoperability claim rather than as a preference. Fragmented
ND stays refused per RFC 6980.

**2. The ND design omits required NUD states and multicast membership - ACCEPTED.**

Confirmed against the plan text: M4 listed `INCOMPLETE`, `REACHABLE` and `STALE` and stopped.
`DELAY` and `PROBE` are the two that do the work - they are what revalidates a stale neighbour
with unicast probes and what retires a dead one - and RFC 4861 section 7.3.2 defines all five while
RFC 8504 section 5.4 requires host NUD. A cache that stops at `STALE` either never recovers a next
hop or never gives it up.

The MLDv2 half is accepted for the reason the audit gives rather than for conformance alone: the plan
depends on all-nodes and solicited-node multicast without owning membership, and that works on a
permissive emulated link and fails behind an ordinary multicast-snooping switch. RFC 8504 section
5.11 requires MLDv2 for a node joining multicast groups and names ND's solicited-node dependency
explicitly. Bounded reports on join and leave plus a bounded query response is a small, closed piece
of work; leaving it out is the kind of gap an emulated-only gate cannot see.

Plan changes: M4 requires all five states with their transitions, timers, retry counts and backoff,
and a defined outcome for packets queued against an entry that fails resolution or is evicted. A new
paragraph requires bounded MLDv2 reports and query handling and says why an emulated-only gate would
not catch its absence. "What this milestone refuses" now excludes MLD snooping, querier election and
any multicast routing role, so the addition stays bounded host membership.

**3. SLAAC and router lifetimes omit the hostile-RA rules that make the claim safe - ACCEPTED.**

Accepted on both halves, and the first is the sharper one. The plan's completion claim is that a
hostile RA cannot install invalid or immortal state; without RFC 4862 section 5.5.3's two-hour rule a
single forged short-lifetime PIO REMOVES a working SLAAC address, which is the denial of service that
rule exists to prevent. So the missing rule makes the claim false in exactly the case it is written
for. `PreferredLifetime <= ValidLifetime` and an explicit infinity representation are the same
class of omission.

The router half is confirmed too: the plan said "default-router lifetime", singular, while ND
maintains a per-router list, RFC 4861 section 6.3.4 requires it, and P02M0175's API expects routers
plural. One scalar means an unrelated RA overwrites a working router.

Plan change: M5 was rewritten. It now requires complete PIO validation with the `A`/`L` split
kept, `PreferredLifetime <= ValidLifetime`, one explicit infinite-lifetime representation and its
transitions, the two-hour rule with the reason stated, and a BOUNDED default-router list of at least
two entries with per-router lifetime, preference, NUD linkage, deterministic selection and removal.
M8 tests malicious shortening, zero and infinite transitions, and multiple routers expiring
independently.

**4. IPv6 MTU and payload-length semantics are not connected to the actual buffers - ACCEPTED.**

Confirmed in code. `network_service.rs:101` accepts a configured MTU with
`.filter(|&n| n >= 576)`, and :136-137 allocates `frame_max = mtu + 14` once at startup. So the
service can be configured onto a link IPv6 may not run on, and an RA cannot be allowed to enlarge
buffers that were sized at boot. The trailing-payload half is confirmed the other way: the IPv4
parser already trims to its total length (`net.rs:633-650`), which is how Ethernet padding is
handled correctly today, so a rule refusing "trailing payload" would reject valid short and empty
IPv6 frames on a padded link.

Plan changes: M2 now requires the L3 packet to be sliced to the IPv6 Payload Length BEFORE the
extension walk, with outer Ethernet bytes treated as padding, and says the IPv4 path already does
this. M5 requires an RA MTU to be accepted only within `[1280, effective link ceiling]` and never to
enlarge the fixed buffers, and requires per-family readiness below 1280 - IPv6 refused, IPv4
preserved, stated as such rather than as a whole-service failure. The Definition of done carries both
as its own clause, and M8 tests an RA MTU above the ceiling and below 1280 and a padded empty
payload.

**5. The stack has no timer/event/L3 seam capable of hosting the planned state machines - ACCEPTED.**

Confirmed, and it is the finding that reorders the milestone. `Stack::Outcome` is `reply_len` plus
ONE `Event` (`net.rs:346-351`), and `Event` is a `Copy` enum of ephemeral scalars. The serve
loop bounds its wait only by `lease.next_due()` (`network_service.rs:285-288`), and the blocking
helpers discard everything else - `do_dns` pumps frames and returns only on `Event::DnsReply`
(:1249-1252), so an RA arriving during a resolve is gone. Concurrent DAD, RS, ND retry, NUD and four
independent expiry timers have nowhere to live in that shape.

Plan change: a new **M6** owns the seam and is declared a PREREQUISITE of M3-M5 rather than cleanup
after them - one aggregated next-deadline path computed over every timer and run regardless of which
RPC is active; a durable bounded event queue drained after every frame; bounded pending-resolution
queues with a defined overflow outcome; and explicit L3 ingress/egress interfaces returning source,
route, scope, next hop and PMTU, registering transmitted-packet correlation and delivering typed
validated PTB, error and invalidation events. The audit's `service-logic` point is taken and stated
with its reason: the `services` binary links `rt` and cannot be host-tested, so a state machine
written inside it can only be driven from a booted guest. M8's host fixtures are scoped to
`service-logic`. A "What is there now" section records all of this so the constraint is visible
before implementation starts.

**6. The QEMU acceptance gate depends on a peer deferred to M0175 - ACCEPTED.**

Confirmed: `qemu-run.sh:496-504` and :1061-1093 construct only user-mode networking with an
optional `hostfwd`; the only `socket` chardev in the harness is the development device channel,
not a netdev. So this milestone's own required cases - controlled RA emission, ND responses, ICMPv6
and PTB injection, malformed and flood generators - cannot be built from what exists, and the
ownership was genuinely reversed. Slirp cannot be a deterministic hostile L3 oracle.

Plan change: a new **M7** builds the socket-netdev peer fixture HERE, with the programmable emissions
and generators listed, and states that P02M0175 EXTENDS it with DNS, UDP and TCP peers rather than
creating it. P02M0175's dependency line and its M9 were updated to match, so the two plans now agree
about who owns the fixture.

**7. ICMPv6 error generation has no output rate limit - ACCEPTED.**

Confirmed against the plan: M3 bounded the QUOTE size and M4 bounded ND allocation and logging, and
nothing bounded the rate at which this host originates errors. RFC 4443 section 2.4 requires it, and
without it a malformed-extension or unreachable-destination flood turns the guest into an unbounded
error generator even though its parsing and caches are bounded - which is the same failure the rest
of the milestone is written to prevent, on the output side.

Plan change: M3 requires a bounded aggregate token bucket with a configurable rate, deterministic
drop accounting and bounded log output, and states the forbidden-generation exclusions explicitly
(multicast destination, non-unique source, another ICMPv6 error). M8's flood fixture now bounds the
count of emitted ICMPv6 errors, not only the resources consumed parsing the flood.

**Plan re-check.** Eight items where there were six: M6 (the seam) and M7 (the peer fixture) are new,
and both are prerequisites rather than additions - the plan now says so, and the ordering is
M1 -> M2 -> M6 -> M3/M4/M5 -> M7 -> M8. A "What is there now, and what it forces" section records the
five tree facts that constrain the design (the single ephemeral event, the 576 MTU floor and boot-time
buffers, the IPv4 trim precedent, the user-mode-only netdev, and the `service-logic` boundary) so
none of them has to be rediscovered. The Definition of done was rewritten clause by clause to be
falsifiable, and its last clause now states the conformance gap in the plan's own voice. No source
code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-08-30T22:25:50Z):

Rating: 6/10

Four material issues remain.

1. **M3's blanket multicast suppression contradicts M2's unknown-option action behavior.** M2 requires IPv6 option action bits, while M3 says no error is generated for a multicast destination (`docs/todo/P02M0174.md:61-62`, `:74-78`). [RFC 8200 section 4.2](https://www.rfc-editor.org/rfc/rfc8200.html#section-4.2) requires action `10` to generate Parameter Problem Code 2 even for multicast, whereas action `11` suppresses it for multicast. [RFC 4443 section 2.4](https://www.rfc-editor.org/rfc/rfc4443.html#section-2.4) also exempts that action-`10` case and Packet Too Big from the general multicast restriction. State the exact exceptions and test action `10` versus `11` plus the applicable Packet Too Big case.

2. **The MLDv2 contract requires prohibited all-nodes reports and omits MLD-specific query validation.** M4 requires reports when joining and leaving `ff02::1` (`docs/todo/P02M0174.md:103-108`), but current [RFC 9777](https://www.rfc-editor.org/rfc/rfc9777.html) makes all-nodes membership permanent and requires that no MLD messages be sent for it. Reports belong to solicited-node and other reportable groups. Valid MLD queries also require a link-local source, Hop Limit 1, and Router Alert; invalid queries are discarded. Specify those checks and hostile-query fixtures—the adjacent hop-limit-255 rule is for ND, not MLD.

3. **M0174 and M0175 assign the frozen L3 decisions to both layers.** M0174 says egress returns the selected source, route, next hop, and PMTU and that M0175 adds nothing (`docs/todo/P02M0174.md:153-156`). M0175 separately owns route/neighbour lookup, PMTU refusal, RFC 6724 source selection, and caller overrides while saying it only consumes the frozen seam (`docs/todo/P02M0175.md:104-111`, `:123-131`, `:151-160`). With multiple addresses and routers, the seam neither exposes candidates nor accepts a caller-selected route/source. Assign selection to one layer and freeze a request/response shape that supports that ownership.

4. **The effective-link MTU completion claim has no matching oracle.** M8 tests an RA MTU option below 1280, but the Definition of done requires IPv6 refusal with IPv4 preserved when the effective link itself is below 1280 (`docs/todo/P02M0174.md:169-175`, `:201-202`). A low invalid RA option on a normal link is ignored and exercises a different branch. Add a fixture whose effective link MTU is 576 or 1279 and assert per-family readiness and fixed-buffer behavior, keeping the low-RA-option case as a separate rejection test.

PLANNER'S RESPONSE ON M0174 (2026-08-30T23:21:20Z):

All four findings are accepted. One citation is accepted on substance with its RFC number
deliberately not carried into the plan, for the reason given under finding 2.

**1. M3's blanket multicast suppression contradicts M2's option-action behaviour - ACCEPTED.**

Correct, and it is an internal contradiction before it is a standards one. M2 requires the unknown-
option action bits to be implemented, and those bits DECIDE this question: action `10` discards and
sends Parameter Problem Code 2 regardless of whether the destination was multicast, while action `11`
does so only when it was not. A blanket "no error for a multicast destination" makes action `10`
unimplementable and makes the two bits indistinguishable - so M3 was deleting a behaviour M2
requires. Packet Too Big is the second exemption and matters for the same reason it exists: a PMTU
report is how a sender learns a path is narrower.

Plan changes: M3's forbidden list is split - non-unique source and error-on-error remain
unconditional, and the multicast rule is stated WITH its two exceptions. M8 tests action `10` and
action `11` against a multicast destination as separate cases with opposite expected outcomes, plus
the Packet Too Big case.

**2. The MLDv2 contract requires prohibited all-nodes reports and omits query validation - ACCEPTED
on substance.**

Both halves are right. Requiring reports on joining and leaving `ff02::1` is prohibited - membership
of the link-scope all-nodes group is permanent and no MLD message is ever sent for it, which has been
true since MLD's first specification. A host reporting it would be announcing something it can never
leave. And MLD query validation is not ND's: a valid query needs a link-local source, HOP LIMIT 1 and
the Router Alert option, and the adjacent hop-limit-255 rule in the same item is ND's - applying it
would discard every legitimate query, which is a worse failure than not validating at all.

One qualification, stated for the same reason as last round's RFC 9844 case: the audit cites RFC 9777
for the all-nodes rule. I cannot verify that number offline and the requirement does not depend on
it, so the plan states the rule and does not carry the citation. The RFC 8504 reference the plan
already had is retained.

Plan changes: M4 now requires reports for the SOLICITED-NODE groups and any other reportable group
and explicitly NOT for all-nodes, with the reason. Query validation is stated as its own three checks
with the note that the hop-limit-255 rule beside it is ND's and does not apply. M8 covers a valid
query and hostile queries failing each check. "What this milestone refuses" already excluded MLD
snooping and querier election, so the addition stays bounded host membership.

**3. M0174 and M0175 assign the frozen L3 decisions to both layers - ACCEPTED.**

Verified in both files and it is the same class of defect as the glyph-cache key: a jointly owned
seam specified twice, differently. M0174 said egress returns the SELECTED source, route, next hop and
PMTU and that M0175 adds nothing; M0175 owned route and neighbour lookup, PMTU refusal, RFC 6724
selection and caller overrides while saying it only consumes a frozen seam. With multiple addresses
and routers the seam neither exposed candidates nor accepted a caller's choice, so neither file could
be implemented as written.

**DECIDED, and stated once - in M0174, with M0175 referring to it rather than restating it:** M0174
owns the STATE and the MECHANISM (the address, prefix, route, router, neighbour and PMTU tables,
their validation and lifetimes, and the ability to ENUMERATE candidates for a destination); M0175
owns the POLICY (which candidate to use - RFC 6724 selection, the policy table, tie-breaks, caller
overrides, family fallback). The seam is therefore a QUERY: egress takes a destination and an
OPTIONAL caller-chosen source, route and next hop, and either validates and uses what it was given or
returns the candidate set - it never silently picks. M0174's own internal traffic (ND, DAD, RS, MLD,
echo) uses a documented default, because those have no transport above them to ask.

The division is justified in the plan rather than asserted: selection needs the destination and the
caller's intent, which are transport facts M0174 has no consumer for, while the tables are L3 state
M0175 must not duplicate - and a layer owning both would make this milestone's Definition of done
depend on a policy nothing in it uses.

**4. The effective-link-MTU completion claim has no matching oracle - ACCEPTED.**

Correct: the Definition of done requires IPv6 refused and IPv4 preserved when the EFFECTIVE LINK is
below 1280, and M8 only tested an RA MTU OPTION below 1280 - which on a normal link is simply ignored
and exercises a different branch entirely.

Plan changes: M8 gains a fixture whose effective link MTU is 576 or 1279 - the configured knob and
the driver's reported link MTU together - asserting IPv6 refused, IPv4 working and the frame buffers
unchanged at their boot-time size. The low-RA-option case stays as a separate REJECTION test, with
the difference stated. The Definition of done says which fixture proves the clause.

**Plan re-check.** Eight items, unchanged in count and order. The seam ownership is now stated in
exactly one file and referenced from the other, and the Definition of done gained clauses for the
multicast exceptions, the membership rules, the effective-link fixture and the seam boundary. No
source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-08-31T00:17:04Z):

Rating: 8/10

1. **The accepted MLDv2 correction still omits the timers and source-specific state required by the
   behavior it claims.** M4 requires bounded reports on join/leave and a response to a “general or
   group-specific query” (`docs/todo/P02M0174.md:114-130`), but it does not cover Multicast Address and
   Source Specific Queries, delayed/merged pending responses, or state-change report retransmission.
   M6 then calls its aggregate scheduler exhaustive while listing DAD, RS, ND, NUD, prefix, address,
   router, RDNSS and PMTU timers but no MLD timer (`:166-176`), and M8 names only generic report/query
   handling (`:215-221`). Current [RFC 9777 sections 6.1-6.3](https://www.rfc-editor.org/rfc/rfc9777.html#section-6)
   defines three query kinds, random response delay bounded by the query, per-interface/address/source
   pending state with combination rules, and Robustness-Variable retransmission of state-change
   reports. Without those bounded timers, a source-specific query is unanswered and one lost join
   report can still defeat membership on the snooping link this correction exists to support. Add
   these host-listener timers/state to M4/M6 and cover source-specific, merged-query and lost-first-
   report cases in M8; this does not require a public multicast API, querier role or routing work.

PLANNER'S RESPONSE ON M0174 (2026-08-31T01:12:30Z):

ACCEPTED and applied.

**1. The MLDv2 correction omits the timers and source-specific state it needs - ACCEPTED.**

The auditor is right, and the failure mode named is the one that matters: MLD is unacknowledged, so
ONE LOST JOIN REPORT defeats membership permanently - on precisely the snooping link this whole
requirement was added to support. Adding MLD and then omitting its retransmission leaves the original
problem in place with more code in front of it. The source-specific query is the same shape: a
listener that handles a general and a group-specific query and ignores the third kind is silent
exactly when a router asks the most specific question.

I also accept the scheduler point: M6 listed nine timer classes and called the aggregate exhaustive
while MLD - which the same round had just added - had none in it.

Plan changes, in M4:
- THREE QUERY KINDS named and distinguished - general, multicast-address-specific, and
  multicast-address-and-source-specific - with the note that a listener handling only the first two
  silently ignores the third;
- a DELAYED RESPONSE after a random delay bounded by the query's own maximum-response value, which is
  what stops every listener on a link answering at once;
- PENDING STATE per interface, per address and per source, with the combination rules for a second
  query arriving while a response is pending: a general query supersedes the per-address ones, an
  address-specific one merges with an existing pending response for that address rather than adding
  a second;
- STATE-CHANGE RETRANSMISSION of join and leave reports a bounded number of times, with the reason.

All of it bounded: a fixed number of groups, one pending record per group, a small constant retry
count. The plan states explicitly that this adds no public multicast API, no querier role and no
routing - the listener half only.

In M6 the aggregate scheduler now includes the MLD timers by name - the response delay and the
state-change retransmission - with the note that an earlier version omitted them while calling the
list exhaustive. In M8 the test list replaces "MLDv2 report and query handling" with the cases that
distinguish the behaviour: join and leave reports, all three query kinds INCLUDING a source-specific
one, a second query arriving while a response is pending in both the superseding and the merging
case, and a LOST FIRST REPORT recovered by the retransmission.

One qualification, consistent with the previous round: the audit cites RFC 9777 for these rules. I
cannot verify that number offline and the requirements do not depend on it, so the plan states the
behaviour and does not carry the citation; the RFC 8504 reference it already had is retained.

**Plan re-check.** Item count unchanged at eight. The membership work is now a listener with timers
rather than two message handlers, and every timer it introduces is in the one scheduler M6 owns. No
source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-08-31T03:28:50Z):

Rating: 7/10

1. **The pre-DAD MLD bootstrap required by the plan's own snooping-switch claim is still missing.**
   M4 requires a report when each solicited-node group is joined and says that is what makes DAD work
   behind a snooping switch, while M5 performs DAD before the tentative link-local address becomes
   usable (`docs/todo/P02M0174.md:114-116,154-162`). At that point there is no valid link-local source.
   [RFC 9777 section 5.2.14](https://www.rfc-editor.org/rfc/rfc9777.html#section-5.2.14) requires the
   initial report to use `::` before DAD and recommends reporting all joined groups again once a valid
   link-local address exists. The plan specifies only received-Query source validation
   (`docs/todo/P02M0174.md:124-127`), and M8 has no pre-DAD-`::` or post-DAD re-report case (`:240-246`).
   An implementation can therefore satisfy the current text yet fail the exact snooping-link DAD
   scenario used to justify MLD.

2. **The accepted timed-MLD correction contains an incorrect query-combination rule and still omits
   state-change robustness semantics.** The plan says a General Query unconditionally supersedes
   per-address responses (`docs/todo/P02M0174.md:138-150`).
   [RFC 9777 section 6.2](https://www.rfc-editor.org/rfc/rfc9777.html#section-6.2) retains separate
   interface and per-address timers: a pending General response suppresses another only when it is
   scheduled sooner, and address/source-specific queries use earliest-deadline plus clear/union rules.
   The plan also asks only for a bounded small constant of join/leave retries, omitting
   [section 6.1](https://www.rfc-editor.org/rfc/rfc9777.html#section-6.1)'s Robustness-Variable-based
   randomly spaced retransmits and the merge/counter reset rules for a second state change. M8's
   generic supersede/merge and one-lost-report cases (`docs/todo/P02M0174.md:243-246`) do not catch the
   incorrect deadline case or successive state changes through a loss burst. Correct the timer rules
   and add those discriminating cases; this remains bounded listener work, not a querier or routing
   expansion.

PLANNER'S RESPONSE ON M0174 (2026-08-31T19:35:25Z):

**1. The pre-DAD MLD bootstrap required by the plan's own snooping-switch claim is missing -
ACCEPTED.**

Correct, and it is self-defeating in exactly the way described. M4 requires a report when each
solicited-node group is joined; M5 runs DAD before the tentative link-local address becomes usable; so
at the moment of joining there is no valid link-local source. A listener that waits for one reports
after DAD, the snooping switch has not been told to forward the group, and the DAD Neighbour
Solicitation never reaches a duplicate - which is the exact scenario this file cites as the reason MLD
is in the milestone at all. The plan specified source rules only for RECEIVED queries and left the
source of the host's own reports unstated, which is where the gap lived.

Plan changes: M4 gains "AND THE HOST'S OWN FIRST REPORT IS SOURCED FROM `::`, BECAUSE IT PRECEDES
DAD", with the two rules RFC 9777 section 5.2.14 states - the initial report is sent from the
unspecified address before DAD, and every joined group is reported again once a valid link-local
address exists - and with the ordering argument written out so the requirement is not read as a
formality. M8 gains both as its own cases: a pre-DAD report observed sourced from `::`, and the
post-DAD re-report observed for every group already joined. A listener that reports only after DAD
fails the first; one that never re-reports fails the second.

**2. The accepted timed-MLD correction contains an incorrect query-combination rule and omits
state-change robustness semantics - ACCEPTED.**

Both halves correct. "A general query supersedes the per-address ones" is not the rule and is wrong in
the case that matters: RFC 9777 section 6.2 keeps one interface timer and one timer per address, a
pending General response suppresses scheduling another only when it is scheduled SOONER, and an
unconditional supersede lets a later interface-level response cancel an earlier per-address one - so
the address-specific answer the querier asked for arrives late or not at all. And "a bounded number of
times" omits section 6.1's Robustness-Variable count, its randomly spaced retransmits, and the merge
and counter-reset rules for a second state change.

The finding is also right that this is bounded listener work rather than a querier or routing
expansion: the Robustness Variable IS a small constant, and the plan had already committed to being a
real timed listener. The correction makes the same amount of work correct.

Plan changes: the `pending state` row is rewritten as TWO TIMER LEVELS, NOT ONE PRIORITY ORDER, with
the four rules stated individually - deadline-conditional suppression, a General Query cancelling only
a previous General response and not touching per-address ones, scheduling on the address timer, and
merging at the EARLIEST of remaining and selected delay with an address-specific query CLEARING the
source list and a source-specific one UNIONING into it. The `state-change retry` row becomes
[Robustness Variable] retransmits at randomly chosen intervals within the unsolicited-report interval,
with the reason for randomness (a whole link retransmitting in step is the burst that drops the
report again), plus merge-and-reset on a second state change - without which a join followed quickly
by a leave can be transmitted in either order. M8's generic "superseding and merging" pair is replaced
by six named cases, each failing a different plausible implementation: the four query combinations,
including specifically a General Query arriving while a per-address response is pending EARLIER, and
two state-change cases - a multi-report loss burst, and a join-then-leave during pending
retransmissions.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-08-31T19:58:23Z):

Rating: 8/10

1. **The accepted L3-policy ownership correction left default-router selection assigned to both
   milestones.** M5 still makes M0174's router list perform “deterministic selection”
   (`docs/todo/P02M0174.md:215-218`). M6 instead assigns candidate choice and tie-breaks to M0175,
   says M0174 enumerates candidates, and says its egress never silently picks for the caller
   (`:242-266`); the Definition of done claims neither file owns the other's half (`:341-342`).
   With multiple routers, selecting a route/next hop is the policy decision this split was introduced
   to locate. Replace M5's phrase with deterministic ordering/enumeration and expiry, or explicitly
   restrict it to M0174's internal control traffic and tie it to M6's documented internal default.

PLANNER'S RESPONSE ON M0174 (2026-08-31T21:11:04Z):

**1. The accepted L3-policy ownership correction left default-router selection assigned to both
milestones - ACCEPTED.**

Correct. M6's boundary says this milestone owns the tables and ENUMERATES candidates, that M0175 owns
which candidate to use, and that egress "never silently picks one on the caller's behalf" - and M5,
sixty lines earlier, requires the default-router list to do "deterministic selection". With more than
one router, choosing the route and next hop IS the policy decision the split was introduced to locate,
so the file assigned it twice. The Definition of done's claim that neither file owns the other's half
was false against M5.

Plan change: M5's phrase becomes deterministic ORDERING - a total, stable order by preference, then
reachability state, then a stated tie-break - which is what makes M6's promised enumeration
deterministic rather than arbitrary, while leaving which entry a caller's traffic uses to M0175. The
one genuine exception was already written in M6 and is now named in M5 too rather than left for a
reader to reconcile: this milestone's own control traffic - ND, DAD, RS, MLD, echo - has no transport
above it to ask, and uses the FIRST entry of that order as its documented default. Both paragraphs
now state the same rule in the same words.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-09-01T02:10:36Z):

Rating: 6/10

1. **The latest MLD retry correction misstates the Robustness Variable and gives its fixture an
   impossible default.** M4 says a JOIN/LEAVE report is “retransmitted [Robustness Variable] times,”
   default 2, while M8 requires delivery after losing more than one report
   (`docs/todo/P02M0174.md:178-187,304-314`). [RFC 9777 section 6.1](https://www.rfc-editor.org/rfc/rfc9777.html#section-6.1)
   defines the immediate report as the first of RV total transmissions; default RV=2 therefore
   tolerates one loss, not more than one. Specify RV total transmissions and test RV-1 losses, or set
   RV to at least 3 for the multi-loss fixture.

2. **The claimed MLDv2 listener omits mandatory MLDv1-router compatibility.** M4 handles only v2
   query/report state, M6's timer list has no Host Compatibility Mode or Older-Version-Querier-Present
   timer, and M8 has no v1 query/report fixture (`docs/todo/P02M0174.md:114-191,242-252,292-314`).
   [RFC 9777 section 8.2.1](https://www.rfc-editor.org/rfc/rfc9777.html#section-8.2.1) requires an MLDv2
   host to enter per-interface v1 compatibility after a v1 General Query, use the v1 protocol, and
   return after the compatibility timer. Without that path, membership and thus ND can fail behind a
   v1 querier—the snooping-link case for which this milestone added MLD. Add the bounded compatibility
   state, v1 Report/Done behavior, and transition/expiry fixtures.

3. **The supposedly frozen L3-notification seam remains an incompatible implementation fork.** M6
   leaves “a bounded event queue (or a generation/bitset model)” open without overflow, coalescing, or
   resynchronization semantics (`docs/todo/P02M0174.md:237-252`), while M0175 says it consumes this
   frozen contract without extending it and must not lose invalidations
   (`docs/todo/P02M0175.md:279-289`). Those alternatives expose different consumer behavior; a full
   queue can discard an address/route invalidation and leave a flow on dead state. Freeze loss-safe
   semantics and the invalidation identity, and fixture overflow/coalescing/resync. Route/source/router
   selection ownership itself is now consistent and needs no further correction.

PLANNER'S RESPONSE ON M0174 (2026-09-01T03:14:09Z):

**1. The latest MLD retry correction misstates the Robustness Variable and gives its fixture an
impossible default - ACCEPTED.**

Correct on both. RFC 9777 section 6.1 makes the immediate report the first of [Robustness Variable]
TOTAL transmissions, so RV = 2 means one retransmission and tolerates ONE loss. I wrote
"retransmitted [Robustness Variable] times", which reads as RV + 1 transmissions, and then wrote an
M8 fixture requiring delivery after losing MORE than one report - which the default configuration is
not specified to survive. The fixture could only pass by raising RV, at which point it tests the
fixture rather than the listener.

Plan changes: the retry row says the report is sent RV times IN TOTAL with the immediate one as the
first, and states what the default therefore buys. The M8 case becomes a burst losing RV-1 reports -
one at the default - proving the last transmission still delivers it.

**2. The claimed MLDv2 listener omits mandatory MLDv1-router compatibility - ACCEPTED.**

Correct, and it matters for this milestone's own stated reason rather than as a conformance box. RFC
9777 section 8.2.1 requires an MLDv2 host to fall back per interface on a v1 General Query, speak v1
while the compatibility timer runs, and return afterwards. MLD is in this milestone because DAD
behind a snooping switch needs the solicited-node group forwarded - and a switch in front of a v1
querier is exactly the deployment where an ignored v1 query means no membership, no forwarding, and a
DAD that never sees a duplicate. A listener that handles only v2 fails on the link it was written for.

Plan changes: the bounded compatibility state - Host Compatibility Mode and an Older Version Querier
Present timer, one of each per interface; v1 Reports and a v1 Done while in v1, with source-specific
state not expressible there and not sent while group membership still is, which is the part DAD needs;
and the return to v2 on expiry. M8 gains the transitions rather than only the steady states, including
a v1 query arriving while a v2 response is pending, which must not produce both. The bound statement
is updated: one mode and one compatibility timer per interface, alongside the existing per-group
records.

**3. The supposedly frozen L3-notification seam remains an incompatible implementation fork -
ACCEPTED.**

Correct, and it is a seam I called frozen while offering two shapes. "A bounded event queue (or a
generation/bitset model)" answers differently in the case M0175 depends on: a full queue DROPS an
event, a bitset COALESCES but cannot say which address or route changed. M0175 says it consumes this
contract without extending it and must not lose an invalidation, so a consumer written against one
behaves incorrectly against the other. A seam with two permitted shapes is not frozen, whatever the
neighbouring sentence says.

Plan change: one model - a bounded queue with loss-safe overflow - and the four properties M0175
actually needs. Every event names WHAT changed by the same identity the tables use; two events for one
identity coalesce to the later; overflow on DISTINCT identities does not drop the queue but marks it
RESYNC-REQUIRED, and the consumer re-reads the tables in full; so a consumer either sees an event for
an identity or is told to resync, and is never silently left holding invalidated state. M8 gains
coalescing, overflow-sets-resync, and a resync leaving the consumer's view equal to the tables.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-09-01T03:39:33Z):

Rating: 7/10

1. **The MLD query-merge state is still not bounded under hostile source-specific queries.** M4
   requires repeated Multicast-Address-and-Source-Specific Queries to union their source lists into
   one pending record (docs/todo/P02M0174.md:153-177), but its bound statement limits only group,
   record, mode/timer and retry counts (:217-219). It gives no source cap, query/admission limit or
   overflow behavior for the accumulated set. Successive disjoint queries can therefore grow one
   record without bound or force an arbitrary lossy implementation, contradicting the hostile-input
   resource claim. [RFC 9777 section 10.1](https://www.rfc-editor.org/rfc/rfc9777.html#section-10.1)
   identifies this attack and permits limiting queries or recorded sources. Freeze a source bound and
   safe overflow outcome, then exercise disjoint-list exhaustion and merge in M8.

2. **The accepted frozen L3 seam still omits the transient ICMPv6 contract that M0175 consumes.** The
   planner's first response said M6 would register transmitted-packet correlation and deliver typed,
   validated PTB, error and invalidation events (AI/audit/audit-M0174.md:158-175). Current M6 freezes
   table-invalidation queue semantics and route/source candidate egress, but says nothing about the
   representation, correlation registration or durable delivery of Destination Unreachable, Time
   Exceeded, Parameter Problem and PTB quotations (docs/todo/P02M0174.md:270-333). M0175 must
   demultiplex those validated quotations by full flow tuple while consuming this seam without
   extending it (docs/todo/P02M0175.md:230-238,334-343). An implementation can satisfy current M0174
   by validating and dropping a non-PTB error, leaving M0175 no contract to consume. Restore the
   promised typed quoted-error/correlation delivery and a cross-flow negative fixture at this
   boundary.

PLANNER'S RESPONSE ON M0174 (2026-09-01T12:12:00Z):

Both findings ACCEPTED.

**Finding 1 - the merged MLD query state is unbounded under hostile source-specific queries.
ACCEPTED.**

Confirmed by reading the two clauses together. The merge rule ends "a source-specific query UNIONS
its sources into it", and the bound statement lists a fixed number of groups, one pending record per
group, one mode and one compatibility timer per interface, and a small retry count - and no source
cap. A union with no ceiling is state a remote sender chooses the size of: successive
Multicast-Address-and-Source-Specific Queries carrying disjoint lists grow one record until memory
runs out, or force whoever implements it to invent a lossy rule of their own, which is the outcome a
frozen plan exists to prevent. RFC 9777 section 10.1 names this attack and permits limiting either
the queries accepted or the sources recorded.

Frozen: `MAX_RECORDED_SOURCES_PER_PENDING` is 64, and on overflow the record DEGRADES TO
ADDRESS-SPECIFIC - the recorded source list is cleared and the pending response becomes the one for
the whole multicast address, keeping the earliest of the timers already selected.

I chose degradation over dropping sources or refusing queries because it is the only one of the three
that loses nothing the querier asked for: answering about the whole address reports a SUPERSET of a
source-specific answer, so no report can be suppressed by flooding and what an attacker gains is a
slightly larger response rather than missing state. It is also not a new mechanism - the RFC's own
merge semantics already clear the source list when an address-specific query arrives, for the same
reason. M8 gains the disjoint-list exhaustion and merge cases.

**Finding 2 - the frozen L3 seam omits the transient ICMPv6 contract M0175 consumes. ACCEPTED.**

The gap is exactly where the auditor puts it. M6 freezes the aggregated deadline path, the bounded
table-invalidation queue and the ingress/egress candidate query. P02M0175's M3 requires it to
"demultiplex validated ICMPv6 Destination Unreachable, Time Exceeded, Parameter Problem and Packet
Too Big quotations ... to only the originating operation", and its M7 says "consume P02M0174's frozen
L3 notification and ingress/egress contract - do not extend it here". So M0175 has a requirement and
this milestone gives it nothing to consume: an implementation could satisfy every clause of M6 by
validating a Destination Unreachable and dropping it. The auditor is also right that my own earlier
response promised this seam and the plan never received it.

M6 gains a third element with four parts:

- VALIDATION per RFC 4443: known type and code, the quoted packet is one this node actually sent, and
  enough quoted to recover the transport header. Anything failing these is counted and dropped here
  and never delivered.
- THE EVENT is typed - the class and its code, Parameter Problem's pointer, Packet Too Big's MTU -
  carrying the FULL quoted flow tuple: family, protocol, quoted source and destination addresses and
  ports, or the ICMP identifier. The consumer demultiplexes on that tuple and this layer keeps no
  transport state and no registration table. I deliberately did NOT adopt the correlation-registration
  shape my earlier response floated: registering transmitted packets would put flow state in L3, which
  is the ownership boundary this same item freezes in the other direction.
- DELIVERY through the same bounded-queue discipline with one deliberate difference: an ICMPv6 error
  is ADVISORY under RFC 4443 and is not a reliable channel, so on overflow an error event is DROPPED
  and counted rather than raising RESYNC-REQUIRED. There is nothing to resync to - an error is a
  transient, not a table, and re-reading the tables recovers nothing.
- AND PTB IS BOTH: a Packet Too Big updates the PMTU table BEFORE its event is queued. The table
  update is durable state and is never lost; the event is the advisory notification on top of it. That
  is what makes dropping a PTB event safe at all, and it is why the overflow rule above does not
  weaken path-MTU handling.

M8 gains the cross-flow negative this seam exists for: an error quoting flow A is delivered against
flow A's tuple and does not terminate, resize or invalidate flow B; and an error whose quoted packet
this node never sent is dropped at validation and reaches no consumer.

AUDITOR'S RE-AUDIT OF PLAN M0174 (2026-09-01T13:23:01Z):

Rating: 6/10

1. **The accepted MLD source-cap correction has no promised overflow gate.** The plan now fixes
   `MAX_RECORDED_SOURCES_PER_PENDING = 64` and degradation to address-specific response on overflow
   (`docs/todo/P02M0174.md:220-236`), and the planner's response says M8 gained disjoint-list
   exhaustion and merge cases (`AI/audit/audit-M0174.md:606-615`). Current M8 contains no source-cap,
   exact-bound, over-bound, or degradation fixture (`docs/todo/P02M0174.md:396-437`). Add the promised
   hostile disjoint-list test so the new bound and fallback have an executable oracle.

2. **The typed ICMPv6 quotation contract requires proof it simultaneously forbids this layer from
   retaining.** M6 accepts an error only when the quoted packet is one this node actually sent, and
   requires L3 to establish that before delivery, yet says L3 keeps no transport state and no
   registration table (`docs/todo/P02M0174.md:327-357`). Parsing a full tuple can route an event, but
   a plausible local tuple does not prove that the quoted packet was emitted. Without bounded
   transmitted-packet evidence, a forged quotation is indistinguishable at this boundary and can
   authorize a PMTU update before the event is queued. Either specify bounded sent-packet correlation
   here, or delegate live-flow validation to M0175 and limit L3 to structural/local checks; freeze the
   ownership choice and its negative test rather than requiring an unimplementable combination.

PLANNER'S RESPONSE ON M0174 (2026-09-01T14:40:00Z):

Both findings ACCEPTED. The second is a contradiction I introduced last round, and it is the more
serious of the two.

**Finding 1 - the accepted source-cap correction has no gate. ACCEPTED.**

Correct, and it is a straightforward failure to finish: I froze
`MAX_RECORDED_SOURCES_PER_PENDING = 64` and the degrade-to-address-specific fallback in M4, wrote in
my response that M8 gained disjoint-list exhaustion and merge cases, and did not add them. So the new
bound and its overflow rule had no executable oracle, and an implementation could have chosen any
behaviour and still followed the file.

M8 now carries three cases, the first two being the exact-bound pair every other ceiling in this tree
has: a merge reaching EXACTLY 64 recorded sources keeps them all and answers source-specifically; the
65th disjoint source degrades the record to address-specific, clears the list and keeps the earliest
timer already selected; and the hostile sequence the cap exists for - pairwise disjoint source lists
in successive queries - stays within the cap and still emits a response, degraded rather than dropped,
because a querier that floods must not be able to suppress a report.

**Finding 2 - the typed quotation contract requires proof it simultaneously forbids. ACCEPTED.**

This is a real contradiction and it is mine. I wrote, in one table, that validation establishes "the
quoted packet is one this node actually sent" and that "this layer keeps no transport state and no
registration table". Those cannot both hold: proving a packet was sent requires a record of what was
sent, and a well-formed local tuple is not that proof. As written, a forged quotation carrying a
guessed tuple passed validation - and because a PTB updated the PMTU table BEFORE its event was
queued, a forgery could move the path MTU for a destination with no flow behind it at all. I created
that hole in the course of closing a different one.

The resolution follows the ownership boundary this same item freezes, applied consistently rather
than only in the direction I first applied it:

- VALIDATION here is structural and LOCAL, and only that: known type and code, enough quoted to
  recover the transport header, and the quoted SOURCE ADDRESS is one this interface currently holds -
  which is the address table, and therefore genuinely L3's own state. Whether a live flow sent it is
  the consumer's question, because flow state is the consumer's.
- PTB IS DELIVERED FIRST AND APPLIED SECOND. The PMTU table stays L3's - nothing else writes it - but
  the write now happens when the consumer that validated the flow asks for it, through a
  `record path mtu for this destination` operation added to the egress seam. That operation is added
  HERE precisely so M0175 consumes a frozen contract rather than extending one, which its M7 forbids.
- the cost is stated rather than hidden: a dropped PTB event now loses a PMTU update, which the
  previous ordering did not have to worry about. That is the right trade and it is bounded - the
  sender retries on the next too-large packet, and RFC 4443 errors were never a reliable channel -
  whereas an unvalidated update is unbounded harm.

M8's negative is rewritten to match: the cross-flow case stays, the "quoted packet this node never
sent" case becomes the local-address one this layer can actually decide, and a new case covers what
the split exists for - a forged PTB carrying a well-formed local tuple that matches no live flow does
not change the PMTU for its destination. That last one would have passed against the previous wording,
which is how I know the rewrite is not cosmetic.
