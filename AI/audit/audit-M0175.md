AUDITOR'S REVIEW OF PLAN M0175 (2026-08-30T16:21:53Z):

Rating: 3/10

The plan recognizes most of the visible dual-stack surfaces, but it assumes transport and event machinery the current stack does not have. Its TCP PTB promise is impossible without retained transmit data, DNS is neither correlated nor complete over TCP, the public wire contract cannot express the promised listener/scoping behavior, and hostile inbound state remains unbounded. Several scope and standards choices also remain ambiguous enough to produce incompatible implementations.

## Material findings

1. **M2 assumes a transmit-side TCP implementation that does not exist.**

   **What is wrong:** M2 requires PTB-triggered resegmentation/retransmission while preserving current backpressure and retransmission (`docs/todo/P02M0175.md:35-37`). `TcpConn` stores sequence counters and receive bytes only, not unacknowledged payload, peer MSS, or an advertised send window (`src/user/services/core/src/net.rs:368-405`, `:759-762`). `tcp_build_data` advances `snd_nxt` after a one-shot frame and retains no bytes (`:1006-1013`). `socket.send` truncates to one frame, transmits once, closes/discards the caller's buffer, and reports success (`src/user/services/core/src/network_service.rs:851-877`); only SYN has a retransmission loop (`:1337-1370`).

   **Why it matters:** Neither loss nor a smaller PTB can be recovered. There are no “outstanding data” bytes to resegment, so M5's reduced-MTU gate is impossible or will silently lose data.

   **Correction:** Make a bounded transmit design part of M2: owned unacknowledged-byte queues, peer/default MSS and send-window tracking, segmentation and partial-write backpressure, ACK retirement, RTO for data/FIN, retry limits, and PTB resegmentation. Define per-flow and aggregate byte/control-block budgets and exactly when `send` reports bytes accepted versus acknowledged; test partial ACK, loss, wraparound, PTB, close, and budget refusal.

2. **DNS omits both response correlation and required TCP fallback.**

   **What is wrong:** M3 names A/AAAA/CNAME parsing but no UDP correlation fields, TC behavior, or DNS-over-TCP framing (`docs/todo/P02M0175.md:39-45`). Today any UDP packet with source port 53 can become `DnsReply`; UDP length/checksum/destination port/server address are not checked, the DNS parser ignores transaction ID, QR/opcode/rcode/question/owner/CNAME, and it returns the first A record (`src/user/services/core/src/net.rs:668-689`, `:1473-1521`). `do_dns` accepts the first such event, while `resolve` collapses every failure into `NotFound` (`src/user/services/core/src/network_service.rs:679-685`, `:1229-1255`). [RFC 7766 section 5](https://www.rfc-editor.org/rfc/rfc7766.html#section-5) requires general-purpose stub resolvers to support TCP; a truncated UDP response is retried over TCP.

   **Why it matters:** Spoofed or cross-query answers can be accepted, distinct errors cannot be preserved, and legitimate truncated/large answers—more common with AAAA/CNAME sets—fail. DNS/TCP also depends on the missing data-retransmission work in finding 1, which the plan does not order.

   **Correction:** Validate UDP framing/checksum and correlate family, scoped server, local/destination port, transaction ID, normalized qname, qtype, and qclass. Validate QR/opcode/rcode/counts, answer-owner/CNAME chain, TTLs, and compression loops. On TC, use bounded two-byte-length-framed DNS/TCP after M2's TCP base is sound; define timeout/malformed/no-record/truncated error mapping and spoof, cross-query, and TCP-retry tests.

3. **Source and destination selection are reduced to an undefined preference.**

   **What is wrong:** M2/M3 say only “source-address selection” and a deterministic preference/fallback policy, plus avoiding deprecated sources (`docs/todo/P02M0175.md:27-31`, `:43-45`, `:57`). M0174 can produce multiple global/link-local/scoped addresses, prefixes, routes, and routers. [RFC 6724](https://www.rfc-editor.org/rfc/rfc6724.html) defines coupled default source and destination ordering based on route/interface availability, scope, deprecation, policy labels/precedence, and prefix match; “IPv6 first” is not an equivalent algorithm.

   **Why it matters:** A resolver can prefer AAAA for which there is no usable route/source, choose the wrong scope/interface, or produce long deterministic failures before a working IPv4/IPv6 candidate. Different clients can implement different fallback order despite one shared API.

   **Correction:** Pin the applicable RFC 6724 profile and any RFC 8028 route/interface update, including the policy table and tie-breaks. Define candidate filtering, scoped-zone handling, explicit caller overrides, failure/fallback timing, and behavior after address/route invalidation. Test mixed A/AAAA, multiple prefixes/routers, deprecated/tentative addresses, link-local scope, and no-route candidates.

4. **The public IDL migration does not define records or operations that encode the promised semantics.**

   **What is wrong:** M1/M4 name route, router, DNS-server, interface, endpoint, neighbor, and socket records without choosing their fields (`docs/todo/P02M0175.md:16-25`, `:47-59`). The actual contract has one IPv4 address/gateway, an IPv4-only endpoint, `listen(port)`, and socket info with no local address/family (`src/idl/network.lsidl:17-38`, `:108-147`). There is no stable interface-ID type/lifetime, address status, bind request, or accepted-socket identity. In particular, a port alone cannot express M2's IPv4-only, IPv6-only, and dual wildcard listeners or distinguish same-port binds.

   **Why it matters:** Independently reasonable implementations will create incompatible wire contracts. A listener can neither request nor report the semantics the tests are supposed to distinguish, and scoped identities can become stale or ambiguous.

   **Correction:** Enumerate the exact LSIDL records/variants and every changed operation signature before implementation. Include stable interface identity/generation, valid and invalid scope combinations, address state/lifetimes/prefixes, route/router/RDNSS entries, a listener bind mode and local scoped endpoint, conflict/reuse rules, and local plus remote endpoints on accepted/live sockets. Add encode/decode/layout and cross-caller migration fixtures.

5. **“Bounded” has no numeric LSIDL or service-buffer contract.**

   **What is wrong:** M1/M3 call address/router/DNS/answer sets bounded but supply no limits or overflow response (`docs/todo/P02M0175.md:22-23`, `:39-40`). Existing MAC, neighbor, name, TCP request, socket, fetch, and chunk lists/strings are unbounded in the schema (`src/idl/network.lsidl:23-38`, `:92-96`, `:124-164`), while NetworkService uses 1024-byte request and 4096-byte fixed-reply buffers (`src/user/services/core/src/network_service.rs:72-77`, `:223-226`). The LSIDL contract says `@bound(n)` is enforced on decode and enables receive-buffer sizing (`docs/LSIDL.md:139-157`).

   **Why it matters:** Expanded info/resolve replies can exceed the server's framing buffer, and the wire can advertise requests the service cannot receive. A u16 list length is not a resource budget, so the bounded-resources Definition of Done remains untestable.

   **Correction:** Assign explicit `@bound(n)` values to every variable occurrence touched by the migration, derive request/reply maximum sizes, and state per-client/per-flow aggregate budgets. Define typed overflow versus deterministic truncation (including an explicit truncation signal where allowed) and use fallible allocation. Test exact-bound and over-bound encodings and rollback/accounting.

6. **Hostile inbound TCP state bypasses the P02M0080 handle ceiling.**

   **What is wrong:** P02M0080 says elastic pools grow until channel-handle exhaustion (`docs/todo/P02M0080.md:8-15`), but an inbound SYN allocates a TCB before any socket channel exists (`src/user/services/core/src/net.rs:801-832`, `:899-917`). Each TCB immediately allocates 65,535 receive bytes and may grow to 262,140 bytes when window scaling is offered (`:99-111`, `:402-405`, `:824-827`). The pool grows without a hard cap, and neither half-open nor completed-but-unaccepted entries have a timeout/backlog limit. M2's new transmit ledger would add more uncharged memory.

   **Why it matters:** An IPv4 or IPv6 SYN/handshake flood can grow the service heap without consuming the handle resource advertised as its ceiling. This directly contradicts M5's hostile/bounded matrix and can exhaust the network Domain.

   **Correction:** Define hard half-open, established-unaccepted, total TCB, and aggregate RX/TX byte budgets; SYN-ACK retransmission/expiry; accept-backlog policy; eviction/admission behavior; and capacity/drop reporting. Charge allocations before state publication and test SYN plus completed-backlog floods across both families.

7. **Independent family readiness and invalidation require a runtime redesign that the plan does not own.**

   **What is wrong:** M4 promises concurrent SLAAC/DAD/RA versus DHCP, IPv6-only readiness, and live address/route invalidation (`docs/todo/P02M0175.md:54-59`). Current boot blocks on DHCP before entering the serve loop and then installs a hard-coded Slirp IPv4 fallback (`src/user/services/core/src/network_service.rs:31-37`, `:108-163`). The standing loop schedules only DHCP lease deadlines and retains one `DhcpReply` from a single ephemeral event; synchronous DNS/connect helpers pump and discard unrelated events (`:207-297`, `:1229-1255`, `:1337-1362`).

   **Why it matters:** An IPv6-only profile still waits for DHCPv4 and silently acquires fallback v4 state. ND/DAD/RA/PMTU/TCP deadlines and M0174 invalidations can be starved or lost during another RPC, leaving sockets bound to invalid state.

   **Correction:** Require immediate event-driven service startup, explicit per-family enable/profile configuration and readiness criteria, one aggregate deadline scheduler, and bounded durable events/pending operations keyed by flow. Freeze the M0174 notification/L3 API and consume it here. Extract pure network decisions into the existing host-testable `service-logic` boundary—the `services` binary deliberately cannot be host-tested (`src/user/services/core/Cargo.toml:145-173`)—then use the same core in QEMU.

8. **The UDP deliverable is internally ambiguous.**

   **What is wrong:** The goal, M2, and M5 promise UDP plus inbound/outbound UDP peer evidence (`docs/todo/P02M0175.md:7-9`, `:27-37`, `:62-70`), but the public IDL has no datagram socket; UDP exists only internally for DHCP, DNS, and SNTP (`src/idl/network.lsidl:124-174`; `src/user/services/core/src/net.rs:668-689`). M4 names only the current TCP-oriented `connect/fetch/listen/accept` public paths.

   **Why it matters:** One implementer can declare success after IPv6 DNS while another builds a large unplanned public datagram API. The test phrase “UDP/DNS” does not resolve whether general application UDP is a milestone requirement.

   **Correction:** Either explicitly limit the UDP claim to named internal DHCP/DNS/SNTP operations and narrow the goal/tests, or define a capability-scoped bounded datagram bind/send/receive API with scoped endpoints, wildcard/conflict rules, queue budgets, and asynchronous ICMP error semantics. Do not leave this scope decision to implementation.

9. **UI zone syntax and URL authority syntax are incorrectly left as one parser decision.**

   **What is wrong:** M4 groups canonical IPv6 rendering, bracketed address/port and URL authority parsing with one zone syntax (`docs/todo/P02M0175.md:50-52`). [RFC 9844](https://www.rfc-editor.org/rfc/rfc9844.html) now obsoletes RFC 6874: `%zone` is a local UI convention, and the earlier zone-in-URI extension no longer updates RFC 3986. A local zone identifier is not part of an HTTP origin and must not be transmitted.

   **Why it matters:** Following obsolete `%25zone` URI advice or blindly round-tripping a UI-scoped literal into HTTP authority creates non-standard URLs, origin ambiguity, and leakage of local interface names.

   **Correction:** Specify separate grammars: RFC 5952 canonical address rendering; bounded RFC 4007/RFC 9844 UI zone input mapped to a stable interface identity; bracketed RFC 3986 URL literals without a zone. Reject scoped URL authorities unless the project deliberately defines and confines a local-only nonstandard extension.


PLANNER'S RESPONSE ON M0175 (2026-08-30T19:36:07Z):

All nine findings are accepted. Two of them were scope decisions the plan had left open; both are now
decided in the plan rather than described. One citation is accepted on substance with its RFC number
deliberately not carried into the plan, and that is explained under finding 9.

**1. M2 assumes a transmit-side TCP implementation that does not exist - ACCEPTED.**

Confirmed, and it is the finding that resizes the milestone. `TcpConn` (`net.rs:371-400`) holds
`snd_una`, `snd_nxt`, `rcv_nxt`, a receive `Vec` and `rx_len` - no unacknowledged payload, no
peer MSS, no send window. `tcp_build_data` (:1008-1014) advances `snd_nxt` past a one-shot frame
and retains nothing. `socket.send` (`network_service.rs:856-878`) truncates to
`min(data.len, tx.len() - TCP_SEGMENT_OVERHEAD)`, transmits once, closes the caller's buffer and
returns `Ok(n)`. Only SYN has a retransmission loop. So "after a validated Packet Too Big
resegments/retransmits outstanding data" is not an addition to existing code - there is no
outstanding data, and M5's reduced-MTU gate as written was impossible.

The silent truncation is worth calling out separately: today a caller that sends more than one
segment loses the remainder and is told the send succeeded.

Plan change: **M2** is now "TCP acquires a bounded transmit side" and is stated as new implementation
that everything else mentioning retransmission depends on: an owned unacknowledged-byte queue with
per-flow and aggregate budgets, peer MSS and effective MSS from interface and path MTU, a tracked
send window, segmentation with partial-write backpressure and a defined accepted-versus-acknowledged
contract for `send`, ACK retirement with wraparound, an RTO for data and FIN, a retry limit that
closes rather than retries forever, and PTB resegmentation. Its tests name partial ACK, loss,
wraparound, PTB, close with data in flight and budget refusal.

**2. DNS omits both response correlation and required TCP fallback - ACCEPTED.**

Confirmed and worse than "omits". `on_udp` (`net.rs:670-678`) hands ANY datagram whose SOURCE
port is 53 to `parse_dns_response`. The destination port, the server address, the UDP length and
the checksum are unchecked; the parser ignores the transaction ID, QR, opcode, rcode, the question
section, the answer owner and the CNAME chain, and returns the first A record. `do_dns` accepts the
first such event, and `resolve` collapses every failure into `NotFound`. A spoofed or cross-query
answer is indistinguishable from the real one.

The TCP-fallback half is accepted with the ordering the audit itself points out: RFC 7766 section 5
requires a general-purpose stub resolver to support TCP, truncated answers are exactly the AAAA and
CNAME sets this milestone adds, and it depends on finding 1's work.

Plan change: **M6** requires UDP framing, length and checksum validation and correlation by family,
scoped server, local and destination port, transaction ID, normalized qname, qtype and qclass;
validation of QR, opcode, rcode and section counts, the answer owner, the CNAME chain, bounded
compression traversal with loop rejection, and TTLs; preserved distinct timeout, malformed, no-record
and transport errors; and bounded length-framed DNS/TCP on truncation, explicitly ordered after M2.
Spoofed, cross-query and TCP-retry tests are named.

**3. Source and destination selection are reduced to an undefined preference - ACCEPTED.**

Confirmed as a genuine gap: the plan said "source-address selection" and "a deterministic
preference/fallback policy", and P02M0174 can produce multiple global and scoped addresses, prefixes,
routes and routers. "IPv6 first" is not an algorithm and does not survive a candidate with no usable
route, a deprecated source, or two prefixes - it produces long deterministic failures before a
working candidate, and two clients of one API can implement different orders.

Plan change: a new **M5** pins the applicable RFC 6724 profile for BOTH default source-address
selection and destination-address ordering, including the policy table, this appliance's tie-breaks,
and any RFC 8028 route/interface update it adopts. It requires candidate filtering, scoped-zone
handling, explicit caller overrides, failure and fallback TIMING, and behaviour after invalidation,
and names the test cases: mixed A/AAAA, multiple prefixes and routers, deprecated and tentative
addresses, link-local scope, and a candidate with no route.

**4. The public IDL migration does not define records or operations - ACCEPTED.**

Confirmed. `network.lsidl:18-21` is `endpoint { addr: ipv4-addr, port: u16 }`; `net-info`
carries one address and one gateway with an unbounded neighbour list; `@op(7) listen: func(port:
u16)` takes a bare port. A port alone cannot express IPv4-only, IPv6-only and dual wildcard
listeners, which are three of the cases M5's tests are supposed to distinguish, and there is no
interface identity, address status, bind request or accepted-socket identity anywhere.

Plan change: **M1** now requires the exact records, variants and operation signatures to be
enumerated BEFORE implementation, with the reason stated - a list of record names is not a wire
contract and two reasonable implementers will produce incompatible ones. It names each one that must
be decided: stable interface identity with a generation, valid and invalid scope combinations,
address state with prefixes and lifetimes, route/router/DNS-server entries as plural records, a
listener bind mode with conflict and reuse rules, and local plus remote endpoints on accepted and
live sockets. The fixed 16-octet positional record and the atomic first-party migration are kept.

**5. "Bounded" has no numeric LSIDL or service-buffer contract - ACCEPTED.**

Confirmed by count: `network.lsidl` contains ZERO `@bound` annotations, while MAC, neighbour,
name, TCP request, socket, fetch and chunk lists and strings are all variable. `network_service.rs`
uses a 1024-byte request buffer and a 4096-byte fixed reply buffer. So the wire currently advertises
requests the service cannot receive, and a u16 list length is not a resource budget - which leaves
the bounded-resources Definition of done untestable, exactly as the audit says.

Plan change: M1 requires an explicit `@bound(n)` on every variable-length occurrence the migration
touches, maximum request and reply sizes DERIVED from those bounds and reconciled against the framing
buffers, per-client and per-flow aggregate budgets, typed overflow versus deterministic truncation
with an explicit truncation signal where truncation is allowed, fallible allocation, and exact-bound
and over-bound encoding tests with rollback and accounting.

**6. Hostile inbound TCP state bypasses the P02M0080 handle ceiling - ACCEPTED.**

Confirmed, and the pool is not merely uncapped in principle. `tcp_alloc` (`net.rs:902-918`) scans
for a free slot and otherwise does `self.conns.push(fresh); Some(len - 1)` unconditionally - it
never returns `None`, so `passive_open`'s own comment "No reply if the pool is full" describes a
state that cannot occur. Every fresh `TcpConn::closed()` allocates `vec![0; 65535]` and
`passive_open` resizes it to `65535 << 2` = 262,140 bytes when the peer offers window scaling -
all of it before any socket channel exists. P02M0080 states the domain handle budget is the true
ceiling, and a half-open TCB consumes no handle, so that statement does not hold for this path.

Plan change: a new **M4** requires hard half-open, established-unaccepted, total-TCB and aggregate
RX/TX byte budgets; SYN-ACK retransmission with expiry; an accept-backlog policy; an admission or
eviction rule at the budget; allocation charged BEFORE state is published; and capacity and drop
reporting through the existing operation. It requires P02M0080's ceiling statement to be corrected in
the same change, with the reason recorded - that statement is why nobody looked. M9 floods with SYNs
and with completed-but-unaccepted connections in both families.

**7. Independent family readiness and invalidation require a runtime redesign the plan does not own -
ACCEPTED.**

Confirmed. `network_service.rs:31-37` and :108-163 block on DHCP before the serve loop and then
install a hard-coded Slirp IPv4 fallback; the loop schedules only the lease deadline; the synchronous
helpers pump and discard unrelated events. So an IPv6-only profile would still wait for DHCPv4 and
silently acquire v4 state, and ND/DAD/RA/PMTU/TCP deadlines can be starved during any RPC.

Plan change: a new **M7** requires immediate event-driven startup, explicit per-family enable and
readiness criteria, one aggregate deadline scheduler over DHCP/ND/DAD/RA/PMTU/TCP, and a bounded
durable event and pending-operation store keyed by flow. It states that this milestone CONSUMES
P02M0174's frozen L3 notification and ingress/egress contract and does not extend it, which is the
boundary that keeps the two milestones from each waiting for the other. Pure decisions go to
`service-logic` for the reason the audit gives.

**8. The UDP deliverable is internally ambiguous - ACCEPTED, and decided.**

Confirmed: the goal, M2 and M5 all said UDP, while the public IDL has no datagram socket and UDP
exists only internally for DHCP, DNS and SNTP. The audit is right that this is a scope decision and
not an implementation detail.

DECIDED, and written into the Goal as its own paragraph: UDP in this milestone means the named
INTERNAL operations plus their IPv6 equivalents and test peers, and there is NO public datagram API.
The reason is stated - it would be a second unplanned public contract stacked on a transmit-side TCP
implementation this milestone already has to build from nothing, and none of the tools being migrated
need it. If an application ever needs datagrams it is its own reviewed requirement with scoped
endpoints, conflict rules and queue budgets. "What this milestone refuses" carries the same line.

**9. UI zone syntax and URL authority syntax are incorrectly left as one parser decision - ACCEPTED
on substance.**

The substance is correct and independent of any particular document: a zone identifier is
link-local UI scope under RFC 4007, it is not part of an HTTP origin, RFC 3986's host syntax does not
admit it, and round-tripping a UI-scoped literal into an authority produces non-standard URLs, origin
ambiguity and leakage of local interface names. Grouping all three under "one unambiguous
interface-zone syntax", as the plan did, invites exactly the `%25` handling that should not be
implemented.

One qualification, stated because it affects what went into the plan: the audit cites RFC 9844 as
obsoleting RFC 6874. I could not verify that number offline, and the correction does not depend on
it, so the plan does not carry it. M8 instead cites RFC 5952 for canonical rendering, RFC 4007 for
the UI zone convention and RFC 3986 for URL authority, and says that RFC 6874's zone-in-URI extension
is deprecated and is not implemented. That reaches the same behaviour without staking a plan on a
citation I have not confirmed.

Plan change: M8 defines the THREE grammars separately, requires round-tripping within each, rejects
scoped URL authorities, and states that a zone is never transmitted. "What this milestone refuses"
now names zone identifiers in URLs or any transmitted authority.

**Plan re-check.** Nine items where there were five. The growth is the honest cost of findings 1, 3,
4, 6 and 7, each of which was work hiding inside an adjective. Ordering is implementable and stated
by the dependencies between items: M1 (contract and bounds) -> M2 (transmit side) -> M3 (family
invariants) and M4 (inbound budgets) -> M5 (selection) -> M6 (DNS, after M2 for the TCP retry) ->
M7 (event-driven service) -> M8 (clients and text) -> M9 (matrix). A "What is there now, and what it
forces" section records the six tree facts an implementer would otherwise rediscover. The Definition
of done gained clauses for the transmit side, DNS correlation and the inbound budgets so each is
falsifiable on its own. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-08-30T22:25:50Z):

Rating: 5/10

Four material issues remain.

1. **M2 still does not specify a minimally correct TCP sender.** An unacknowledged queue, tracked send window, “an RTO,” and retry limit (`docs/todo/P02M0175.md:85-102`) omit RTT sampling, dynamic RFC 6298/Karn RTO and exponential backoff, basic congestion-window/slow-start/congestion-avoidance behavior, and sender-side zero-window probing. These are basic TCP requirements, not the advanced congestion control refused at `:223-225`; [RFC 9293 sections 3.8.1-3.8.2 and 3.8.6.1](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.8.1) require them. Pin a bounded basic sender profile, distinguish retransmission from persist handling, and test changing RTT, backoff, congestion-window flight limits, a closed/reopened peer window, and a lost window update.

2. **DNS is the only named internal UDP operation whose replies become correlated and validated.** M8 merely says to migrate SNTP, while M7's flow-keyed pending store supplies no request identity (`docs/todo/P02M0175.md:151-167`). Today UDP dispatch accepts SNTP solely by source port; its request transmit timestamp is zero, its event carries only a Unix scalar, and `do_sntp` accepts the first such event (`src/user/services/core/src/net.rs:668-689`, `:1236-1269`, `:1523-1535`; `src/user/services/core/src/network_service.rs:1258-1282`). That value resets the wall clock. Require full tuple/framing/checksum validation, a per-request transmit value matched against the reply's originate timestamp as required by [RFC 5905](https://www.rfc-editor.org/rfc/rfc5905.html), mode/version/leap/stratum/transmit validation, and spoof/replay/cross-request fixtures.

   DHCP has the same class of omission: the current client uses a fixed transaction ID and parses replies without checking `xid` or `chaddr`; the first OFFER/ACK of the expected type advances the exchange (`src/user/services/core/src/net.rs:1318`, `:1392-1434`; `src/user/services/core/src/network_service.rs:1190-1224`). [RFC 2131 section 4.4.1](https://www.rfc-editor.org/rfc/rfc2131.html#section-4.4.1) requires a recorded random transaction ID and discarding nonmatching offers. Include DHCP client/transaction/state correlation and hostile fixtures instead of preserving this behavior merely as “green.”

3. **The stated UDP scope contradicts the explicit DHCPv6 refusal.** The Goal includes DHCP, DNS, and SNTP “plus their IPv6 equivalents” (`docs/todo/P02M0175.md:14-19`), while this plan explicitly refuses DHCPv6 (`:227-228`) and M0174 assigns IPv6 configuration to SLAAC/RDNSS and also refuses DHCPv6 (`docs/todo/P02M0174.md:110-139`, `:207-210`). Replace the phrase with the actual scope, such as DHCPv4 plus DNS and SNTP over both address families, and make the test matrix match it.

4. **The frozen M0174 egress seam contradicts M0175's selection ownership.** M0174 returns the selected source, route, next hop, and PMTU and says M0175 adds nothing (`docs/todo/P02M0174.md:153-156`). M0175 nevertheless owns route/neighbour lookup, PMTU refusal, default source selection, and caller overrides while also saying it only consumes that seam (`docs/todo/P02M0175.md:104-111`, `:123-131`, `:151-154`). Assign selection to one layer and freeze a request/response shape capable of expressing that ownership before M0174 completes.

PLANNER'S RESPONSE ON M0175 (2026-08-30T23:21:20Z):

All four findings are accepted. Two of them are code-verified and the other two are contradictions
between this plan and M0174.

**1. M2 does not specify a minimally correct TCP sender - ACCEPTED.**

The auditor draws the right line. An unacknowledged queue, a tracked window, "an RTO" and a retry
limit is a retransmit loop, not a sender: with no RTT sampling it uses a fixed timeout that is wrong
on any real path, with no backoff it retransmits into a congested one, with no flight limit it has no
notion of how much may be outstanding, and with no persist mechanism a LOST WINDOW UPDATE deadlocks
the connection permanently. That last one is a correctness failure, not a performance one, and it is
the clearest evidence that these are basic requirements rather than the advanced congestion control
this milestone refuses.

Plan changes: M2 gains a BOUNDED BASIC SENDER PROFILE as a labelled block - RTT sampling with Karn's
rule, smoothed RTT and variance with the standard RTO computation and its floor and ceiling,
exponential backoff per segment reset on a new measurement; a congestion window with slow start and
congestion avoidance and the rule that flight never exceeds the smaller of it and the peer's window;
PERSIST as its own mechanism distinct from retransmission, with the deadlock reason stated; and a
bounded retry limit that closes with a typed error. What stays refused is named so the line is
visible: CUBIC, BBR, SACK, ECN, pacing and delayed-ACK tuning. Tests: changing RTT moving the RTO,
backoff across successive retransmissions of one segment, the window limiting bytes in flight, a peer
window closing to zero and reopening, and a lost window update recovered by PERSIST rather than by
timeout.

**2. DNS is the only internal UDP operation whose replies are correlated - ACCEPTED, and the SNTP
case is worse than the audit states.**

Verified in code. The SNTP request is 48 bytes with only the first byte set, so its TRANSMIT
TIMESTAMP IS ZERO - there is no per-request value in existence to match a reply against - and the
reply is accepted on source port 123 alone, parsed for a transmit timestamp, and used to SET THE WALL
CLOCK. A single forged datagram moves system time, and no amount of care elsewhere in the resolver
touches that path. DHCP is the same class: a FIXED transaction ID whose own comment says "SLIRP is the
only DHCP source", with neither `xid` nor `chaddr` checked on the reply.

Plan changes: a new **M8**, "EVERY internal UDP operation is correlated and validated, not only DNS",
owning both. SNTP gets a random non-zero transmit timestamp per request, recorded and matched against
the reply's ORIGINATE timestamp, plus full tuple, length and checksum validation and mode, version,
leap-indicator and stratum checks with alarm and stratum 0/16 refused as unsynchronised. DHCP gets a
random recorded `xid` and `chaddr` matching, with non-matching replies discarded. Fixtures for both:
spoofed, replayed and cross-request. The plan says why they belong here rather than later - this
milestone rebuilds the event and pending-operation model that correlation needs, and carrying an
unauthenticated clock and lease path across that rebuild as "green" is how it would survive.

**3. The stated UDP scope contradicts the explicit DHCPv6 refusal - ACCEPTED.**

Correct and trivially so: "DHCP, DNS and SNTP plus their IPv6 equivalents" while the same file refuses
DHCPv6 by name and M0174 assigns IPv6 configuration to SLAAC and RDNSS. There is no IPv6 equivalent
of the DHCP client and the phrase implied a test row that must not exist.

Plan changes: the Goal now reads "DHCPv4, and DNS and SNTP over BOTH address families", with the
correction and its reason recorded.

**4. The frozen M0174 egress seam contradicts this milestone's selection ownership - ACCEPTED.**

The same finding as M0174's third, answered identically and in one place. The boundary is now stated
ONLY in M0174 - that layer owns the tables and ENUMERATES candidates, this one CHOOSES among them -
and this file refers to it rather than restating it in different words, which is what produced two
incompatible readings.

Plan changes: M5 is retitled "over M0174's candidates" and records that this milestone does not
implement route or neighbour lookup and does not own the PMTU cache; it queries them, and what it
owns is the choice. M3's opening line is corrected the same way - PMTU refusal against the value the
seam returns, lookup left to M0174. The dependency line now describes the split rather than listing
what M0174 "supplies".

**Plan re-check.** Ten items where there were nine (M8 is new, and the matrix item renumbered to
M10). Ordering is unchanged and still runs contract -> transmit side -> family invariants -> inbound
budgets -> selection -> DNS -> event-driven service -> internal-UDP correlation -> clients -> matrix.
The Definition of done gained clauses for the sender profile and for internal-UDP correlation. No
source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-08-31T00:17:04Z):

Rating: 8/10

1. **The accepted “every internal UDP operation” correction remains incomplete for DHCP.** M8 gives
   SNTP full tuple, UDP length/checksum and request/semantic validation, but DHCP receives only a
   random `xid`, `chaddr` matching and three fixtures (`docs/todo/P02M0175.md:206-227`). It does not
   require DHCP UDP length/checksum/destination-port validation or bind replies to the selected
   offer/server/address and the current SELECTING, REQUESTING, RENEWING or REBINDING state. That is
   material because multiple legitimate servers in the named racing fixture share the client's
   `xid` and `chaddr`; those fields alone cannot distinguish the chosen offer. Current dispatch still
   selects DHCP solely by source port (`src/user/services/core/src/net.rs:668-689`), and `parse_dhcp`
   publishes the parsed lease before the caller checks message type or state (`:1392-1434`). The
   blocking handshake and lease clock apply or reject the event only afterwards
   (`src/user/services/core/src/network_service.rs:1190-1224`, `:1063-1080`), so even a supposedly
   ignored late/racing reply can overwrite stored lease data. RFC 2131's
   [client behavior](https://www.rfc-editor.org/rfc/rfc2131.html#section-4.4.1) selects one offer and
   identifies the selected server/requested address. Require a staged, state-specific transaction:
   validate complete UDP framing; freeze the selected server/address; define which OFFER/ACK/NAK is
   admissible in each phase, including renew/rebind rules; and commit lease state atomically only
   after validation. Add hostile late and competing valid OFFER/ACK/NAK cases that prove rejected
   frames cannot mutate the lease.
