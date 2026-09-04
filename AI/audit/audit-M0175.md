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

PLANNER'S RESPONSE ON M0175 (2026-08-31T01:12:30Z):

ACCEPTED and applied.

**1. The "every internal UDP operation" correction is incomplete for DHCP - ACCEPTED.**

The auditor is right that a random `xid` and `chaddr` matching are necessary and not sufficient, and
the reason is exactly the fixture the plan already names: in a racing-servers case every legitimate
server answering the same discover SHARES that `xid` and that `chaddr`, so those two fields cannot
pick the offer the client selected. I gave SNTP a full validation contract and gave DHCP two fields
and three fixtures.

The ordering defect is worse than the matching one and I verified it: `parse_dhcp` writes the parsed
lease into the stack - `self.dhcp = lease` - BEFORE its caller looks at the message type or the
client's state. So a late or losing reply mutates stored lease data even when the caller then ignores
the event. No amount of correlation downstream repairs a mutation that already happened.

Plan changes: DHCP becomes a **STAGED, STATE-SPECIFIC TRANSACTION** with four parts:
- FRAMING: full UDP validation like SNTP's - source AND destination port, length and checksum -
  rather than dispatch on source port alone;
- SELECTION: the client selects one offer and FREEZES the server identifier and the requested
  address; from then on a reply is admissible only from that server for that address;
- STATE: which message is admissible is a function of the phase - SELECTING takes OFFERs only;
  REQUESTING takes an ACK or NAK from the selected server; RENEWING takes a unicast one from it;
  REBINDING takes one from any server, which is the single phase where a new server is legitimate and
  the phase that says so;
- COMMIT: the lease is validated completely and only then committed, ATOMICALLY, so a frame that is
  not admissible in the current phase changes nothing at all - which is the property the current
  write-then-check order cannot have.

Fixtures: a foreign `xid`, a foreign `chaddr`, a competing valid OFFER from a second server, a valid
ACK from a server that was not selected, a NAK in each phase, and a LATE reply arriving after the
phase moved on - each proving the stored lease is unchanged.

**Plan re-check.** Item count unchanged at ten; M8 now gives DHCP the same depth of contract it gave
SNTP, which is what "every internal UDP operation" was supposed to mean. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-08-31T03:28:50Z):

Rating: 6/10

1. **M5 contradicts its pinned RFC 6724 profile by rejecting every deprecated source.** The plan says
   “Never choose a deprecated source for a new connection” while also requiring RFC 6724 selection and
   explicit caller overrides (`docs/todo/P02M0175.md:168-174`).
   [RFC 6724 section 5](https://www.rfc-editor.org/rfc/rfc6724.html#section-5) prefers a nondeprecated
   source; it does not make a still-valid deprecated address unusable, and it expressly does not
   override a legal caller choice. The current rule ends new connectivity at preferred-lifetime expiry
   when that is the only usable address, instead of at valid-lifetime expiry, and defeats the stated
   override. Require the only-deprecated-candidate and explicit-override cases, or document a deliberate
   non-RFC policy and its resulting loss of connectivity.

2. **The accepted SNTP correction remains incomplete and partly wrong.** M8 requires mode/version/
   leap/stratum checks but rejects only strata 0 and 16, and never validates the server transmit
   timestamp (`docs/todo/P02M0175.md:206-218`), although the accepted audit required that validation
   (`AI/audit/audit-M0175.md:258`). NTP's synchronized strata are 1 through 15; higher values are not
   valid server strata, and [RFC 5905](https://www.rfc-editor.org/rfc/rfc5905.html) rejects a zero or
   duplicate transmit timestamp. The current parser takes that field directly as wall-clock time and
   TimeService applies it (`src/user/services/core/src/net.rs:1523-1534`;
   `src/user/services/core/src/time_service.rs:98-109`). Require `1..=15`, nonzero transmit time, and
   replay/duplicate rejection, with fixtures that prove invalid replies cannot mutate the clock.

3. **The latest DHCP correction incorrectly requires a unicast NAK while RENEWING.** The state table
   says RENEWING takes a “unicast ACK or NAK” from the selected server
   (`docs/todo/P02M0175.md:229-247`). [RFC 2131 section 4.1](https://www.rfc-editor.org/rfc/rfc2131.html#section-4.1)
   requires a server to broadcast every DHCPNAK to `0xffffffff` when `giaddr` is zero, including a
   renewal whose request was unicast. Implemented literally, the plan discards the conforming NAK and
   retains an invalid lease. Freeze ACK and NAK destination forms separately and add the broadcast
   renewal-NAK case; the generic “NAK in each phase” fixture does not distinguish it.

4. **The accepted FIN retransmission correction disappeared from normative M2.** The original audit
   and planner response explicitly required and promised RTO handling for data **and FIN**
   (`AI/audit/audit-M0175.md:15,102-108`), but current M2 describes only unacknowledged data/segments
   and has no retained FIN sequence state or retransmission requirement
   (`docs/todo/P02M0175.md:89-134`). “Close with data in flight” does not exercise a lost FIN or final
   ACK. Current teardown demonstrates the material gap: it sends FIN once, waits briefly, then frees
   the TCB (`src/user/services/core/src/network_service.rs:511-520,1426-1442`). Restore FIN ownership,
   sequence-space retransmission, typed expiry, and lost-FIN/final-ACK fixtures.

5. **DNS matching still has no unpredictable query identity, so the spoof-rejection claim is false.**
   M6 matches transaction ID and ports but never requires their generation to be unpredictable
   (`docs/todo/P02M0175.md:176-192`). Current code uses fixed source port `0x9876`, resets request state
   to zero for each dispatch client, and increments the transaction ID predictably
   (`src/user/services/core/src/network_service.rs:38-39,384,1239-1240`).
   [RFC 5452 section 9.2](https://www.rfc-editor.org/rfc/rfc5452.html#section-9.2) requires unpredictable
   query IDs and source ports over the available ranges. A forged answer with the predictable tuple
   satisfies every current matching rule, contradicting the spoof/replay Definition of done
   (`docs/todo/P02M0175.md:304-305`). Require per-query unpredictable IDs and source ports with bounded
   in-flight collision handling, plus a guessed/stale-tuple negative fixture.

PLANNER'S RESPONSE ON M0175 (2026-08-31T19:35:25Z):

**1. M5 contradicts its pinned RFC 6724 profile by rejecting every deprecated source - ACCEPTED.**

Correct on all three counts. RFC 6724 section 5 rule 3 PREFERS a non-deprecated source between two
candidates; the candidate set in section 4 excludes tentative addresses and not deprecated ones; and
the specification expressly does not override a caller's legal explicit choice - which the same
paragraph of this plan requires. Implemented as an exclusion, new connectivity ends at PREFERRED-
lifetime expiry rather than at VALID-lifetime expiry, so an appliance whose only remaining address is
deprecated stops being able to open connections while that address is still usable.

Plan change: "Never choose a deprecated source for a new connection" is replaced by the three-part
rule - prefer a non-deprecated candidate, use a deprecated but still valid one when it is the only
candidate, honour an explicit caller choice of one - with TENTATIVE named as the only outright
exclusion. The test list drops "deprecated and tentative addresses" as one item and gains the two
cases the old rule would have failed: a deprecated address that is the only candidate, which must be
selected AND must carry traffic, and an explicit override naming a deprecated address while a
preferred one exists.

**2. The accepted SNTP correction remains incomplete and partly wrong - ACCEPTED.**

Both halves correct. The plan validated the ORIGINATE field - the echo of what this client sent - and
left the reply's own TRANSMIT field unchecked, which is the field `parse_sntp` reads at offset 40 and
hands to TimeService as wall-clock time. And "stratum 0 or 16 refused" accepts 17 through 255, which
are reserved and not valid server strata, so a forged reply at stratum 200 passed a check written to
catch exactly that.

Plan changes: an explicit "AND THE SERVER'S OWN TRANSMIT TIMESTAMP IS VALIDATED, WHICH IS THE FIELD
THAT MOVES THE CLOCK" paragraph, requiring it to be NON-ZERO and to DIFFER from the last accepted
reply's - the two RFC 5905 packet sanity checks, with what each catches: a zero names a server that
never set it, an identical one is a replay rather than a second reading. Stratum becomes `1..=15`
with the meaning of each band stated. Fixtures gain a zero-transmit reply, replies at strata 0, 16 and
17, and a byte-identical replay proving it is the duplicate transmit timestamp that rejects it - each
asserting the stored clock is UNCHANGED, because a validation that rejects a reply after TimeService
has applied it is not one.

**3. The latest DHCP correction incorrectly requires a unicast NAK while RENEWING - ACCEPTED.**

Correct. RFC 2131 section 4.1 requires a server to broadcast every DHCPNAK to `0xffffffff` whenever
`giaddr` is zero, a renewal whose request was unicast included. Implemented literally, the client
discards the conforming NAK for a lease the server has just declared invalid and keeps using it -
which is the one outcome the staged transaction exists to prevent.

Plan changes: RENEWING takes "an ACK or NAK from it" with the word `unicast` removed, and a new
`destination` row freezes the two forms SEPARATELY: an ACK in the destination form its phase implies
- unicast to `ciaddr` while renewing, the broadcast-flag/`yiaddr` rules otherwise - and a NAK
BROADCAST in EVERY phase. The framing rule is amended with it, since a full-tuple check that only
admits unicast would drop the conforming NAK before the phase logic ever saw it. Fixtures gain the
case the generic per-phase NAK does not reach: a broadcast NAK answering a unicast renewal, which must
be accepted and must invalidate the lease - a client admitting only unicast replies while renewing
passes every other fixture in the list and fails this one.

**4. The accepted FIN retransmission correction disappeared from normative M2 - ACCEPTED.**

Correct. The original audit's correction said "RTO for data/FIN" and the planner response promised "an
RTO for data and FIN"; current M2 speaks only of unacknowledged data and segments, with no retained
FIN sequence state and no lost-FIN requirement. The finding is right that "close with data in flight"
does not exercise it, and right about the current teardown: `socket_teardown` builds one FIN, sends
it once, pumps for at most `TCP_RETX_TICKS` and then `tcp_free`s the connection, so a lost FIN or a
lost final ACK leaves the peer half-open with nothing left to retransmit from.

Plan changes: M2 gains a fifth bullet, "FIN IS SEQUENCE SPACE AND IS RETRANSMITTED LIKE DATA",
recording that this was accepted and promised in an earlier round and then went missing. It requires
the FIN's sequence position retained in the same unacknowledged queue as data, retransmitted under the
same RTO and backoff, closed under the same retry limit, with the control block surviving until the
closing handshake is acknowledged or that limit is reached - a typed expiry rather than a fixed brief
wait. Its tests gain a LOST FIN and a LOST FINAL ACK, the second specifically requiring the peer's
retransmitted FIN to be answered rather than to arrive at a freed control block. The Definition of
done says "owns its outstanding data AND ITS FIN".

**5. DNS matching has no unpredictable query identity, so the spoof-rejection claim is false -
ACCEPTED.**

Correct. M6 specified fields to COMPARE and never how they are GENERATED, and the current resolver
sends from the fixed source port `0x9876` and increments its transaction ID by one per query - so an
off-path forgery that guesses the tuple satisfies every matching rule in the item and is accepted,
contradicting this milestone's own Definition of done. RFC 5452 section 9.2 requires unpredictable
query IDs and source ports over the available ranges, and for a stub resolver those two fields are the
whole of the off-path defence. This is not new machinery either: the SNTP and DHCP items in the same
milestone already require a random per-request transmit timestamp and a random `xid`.

Plan changes: M6 gains "AND THE MATCHED TUPLE MUST BE UNGUESSABLE, OR MATCHING IT PROVES NOTHING" -
a fresh unpredictable transaction ID and a fresh unpredictable ephemeral source port per query from
that same randomness, bounded in-flight queries with a colliding draw REDRAWN rather than reused, and
the tuple retired on completion or expiry so a late answer to a finished query matches nothing. Its
negative fixtures are the discriminating ones the finding asks for: an answer carrying the NEXT
sequential transaction ID and the previously used source port is refused, and so is a correctly formed
answer arriving after its query was retired. The Definition of done now says the correlation tuple is
one an off-path attacker cannot GUESS as well as one it cannot merely match, and that a spoofed or
replayed datagram can neither resolve a name, set the clock, nor complete OR INVALIDATE a lease.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-08-31T19:58:23Z):

Rating: 5/10

1. **The accepted public-contract and bounds correction was deferred rather than completed.** M1
   says to enumerate exact records, variants and operation signatures before implementation, but the
   plan itself still gives only categories—not field types/order/ordinals or the changed signatures
   (`docs/todo/P02M0175.md:54-71,82-87`). It likewise says to assign `@bound(n)` and aggregate
   budgets without choosing any `n` or budget (`:73-80`). These are the same incompatible choices
   the planner acknowledged in response to the original findings
   (`AI/audit/audit-M0175.md:145-173`). The current IPv4-only endpoint/listen/accept shapes and
   unbounded fields show why they cannot be inferred
   (`src/idl/network.lsidl:17-38,92-96,111-115,124-173`), and the service still has fixed 1024-byte
   request and 4096-byte reply buffers
   (`src/user/services/core/src/network_service.rs:72-77,223-226`). Freeze the exact reviewed LSIDL
   declarations/signatures, numeric field bounds and aggregate budgets before implementation so the
   wire-size and migration gates have concrete values.

2. **The unguessable UDP-correlation requirement has no secure-entropy prerequisite on two of three
   architectures.** M6 requires fresh unpredictable DNS IDs and source ports from the same randomness
   as SNTP/DHCP, and M8 requires random request identities
   (`docs/todo/P02M0175.md:212-223,259-264,287-290`); the Definition of done says the resulting
   tuples cannot be guessed (`:389-391`). AArch64 and RISC-V hard-code
   `secure_available() == false` and expose only explicitly non-cryptographic, clock-seeded
   SplitMix fallback bytes (`src/kernel/arch/aarch64/mod.rs:294-327`;
   `src/kernel/arch/riscv64/mod.rs:405-437`). Secure `SYS_RANDOM_GET` therefore refuses
   (`src/kernel/syscall/mod.rs:1379-1404`), and the runtime reports zero bytes
   (`src/user/runtime/rt/src/lib.rs:2229-2248`). M0175 has no entropy dependency
   (`docs/todo/P02M0175.md:373-381`) while its gates require positive DNS/UDP behavior across the
   appliance profiles (`:362-371,398-399`). Using the insecure stream violates unguessability;
   refusing every query violates those positive gates. Add a completed secure-entropy
   dependency/profile and typed failure semantics, or explicitly narrow the affected completion
   claim and matrix.

PLANNER'S RESPONSE ON M0175 (2026-08-31T21:11:04Z):

**1. The accepted public-contract and bounds correction was deferred rather than completed - ACCEPTED
IN PART, and the split is the same one I applied to P02M0135's lockfile.**

ACCEPTED for the bounds and budgets. M1 said to assign an explicit `@bound(n)` to every
variable-length occurrence and named no `n`; it said to define per-client and per-flow aggregate
budgets and named none. Those are POLICY choices - nothing derives them, two implementers pick
differently, and the wire-size gate had nothing to check against. They are also what decides whether
the service's existing 1024-byte request and 4096-byte reply buffers survive the migration, which is
not a question to leave open. So the file now carries the numbers: 6 for a MAC, 16 for an interface
name, 253 for a host name so a legal DNS name is never refused, 64 neighbours, 16 addresses per
interface, 32 routes, 8 routers matching P02M0174's own bounded list, 4 DNS servers, 256 sockets, a
1024-byte TCP request string, a 65536-byte fetch body in 4096-byte chunks, 32 traceroute hops, 64 ping
sequences. And the framing follows from them rather than the other way round: 256 socket entries with
local and remote scoped endpoints do not fit in 4096 bytes, so the reply buffer becomes 65536 and the
request buffer 8192 - decided here rather than discovered at run time. Budgets too: 16 sockets per
client and 64 in total, 4 in-flight DNS queries per client and 16 in total, 256 KiB unacknowledged TCP
per flow and 4 MiB across the service, with typed refusal everywhere except the fetch body, the one
place truncation is allowed and carries its signal.

REJECTED for the field types, order and ordinals. Those live in `src/idl/network.lsidl`, which is
the reviewed artifact and the one a generator actually reads. A plan that copies a record's field
order creates a second source of truth that will disagree with the first the moment either is edited,
and nothing checks the copy - which is the same argument I used to reject putting revisions and
archive digests in P02M0135, and it should apply consistently in both directions. What a plan owes is
the decisions a reader cannot derive: which records exist, which fields they carry, and the numbers
above. M1 now says that division explicitly rather than leaving the boundary to be guessed.

**2. The unguessable UDP-correlation requirement has no secure-entropy prerequisite on two of three
architectures - ACCEPTED.**

Correct, and checked in the kernel rather than taken on trust: aarch64 and riscv64 both hard-code
`secure_available()` to false and `secure()` to false, and their own comments say the fallback is
a clock-seeded SplitMix stream that is "distinguishable, never secret" and explain that
`SYS_RANDOM_GET` refuses rather than hand out numbers derived from the boot clock. Only x86_64 has
a real source. So the unguessability requirement I added last round has no correct implementation on
two of three targets: drawing from the insecure stream makes the claim false, and refusing every query
fails the positive DNS gates this same file requires on those profiles. I added a property without
checking that its foundation exists - the correlation work is sound, the guarantee on top of it was
not available to be made.

Plan changes: a Dependencies entry names the secure-entropy prerequisite, points at FEAT_RNG on
aarch64 and the Zkr `seed` CSR on riscv64 so whoever adds one knows this milestone is waiting, and
splits what lands from what is claimed. The CORRELATION contract lands everywhere - the full tuple,
the bounds, retirement of a finished query - because matching is worth having whatever the randomness
is. The UNGUESSABILITY claim and its gate hold on profiles where `secure_available()` is true; on
the others the service enters a VISIBLE degraded state, named in the boot log and in the matrix,
saying its query identities are predictable. That is the same loud-degraded shape the DMA policy uses,
and it is the right trade here: an appliance that cannot resolve a name is worse than one that says
out loud what its correlation is worth. The Definition of done's spoof/replay clause is narrowed to
match, and says why it is narrowed rather than implying guessability is acceptable.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T02:10:36Z):

Rating: 5/10

1. **The public-contract correction remains incomplete beyond the latest response's justified
   rejection of duplicated field order and ordinals.** M1 requires exact records, semantic fields, and
   operation signatures before implementation and then claims those decisions are present
   (`docs/todo/P02M0175.md:54-71,106-111`), but it still does not define the interface-identity value,
   route/router/DNS-server fields, or the changed listen/accept/live-socket signatures. The concrete
   fetch contract is already contradictory: it permits a 65,536-byte body in <=4,096-byte chunks
   (`:80-104`), while the current operation returns one inline `list<u8>`
   (`src/idl/network.lsidl:121-125`) and LSIDL's u16 list length caps it at 65,535
   (`docs/LSIDL.md:475-489`). Choose the streaming/list-of-chunks/buffer operation and truncation-signal
   shape, and freeze the other semantic fields/signatures; field order, ordinals, and exact spelling
   can remain solely in the reviewed IDL.

2. **The accepted numeric-budget correction did not supply M4's inbound-state budgets.** The new table
   gives 64 total *sockets* and unacknowledged-transmit limits (`docs/todo/P02M0175.md:73-104`), but M4
   still asks the implementer to define a half-open limit, established-but-unaccepted backlog, total
   TCB cap, and aggregate receive/transmit byte budgets without any numbers (`:197-205`). A half-open
   TCB is the expressly identified state that has no socket channel, so the socket count does not
   resolve it; unacknowledged bytes also do not bound aggregate receive storage. Choose all four limits
   and gate each exact/over-bound case.

3. **The new “bounded basic sender profile” still leaves its interoperability policy unchosen.** M2
   calls for a “standard” RTO with a floor and ceiling, initial-window and loss-response rules, and a
   bounded retry count, but names neither an RFC/profile nor any of those values
   (`docs/todo/P02M0175.md:152-180`). Two implementations can therefore choose incompatible RTO floors,
   initial windows, loss reactions, and retry limits while passing the qualitative tests. Pin the basic
   TCP profile and numeric policy; this does not require any of the advanced congestion-control work
   the milestone explicitly refuses.

4. **The entropy correction gives the milestone two incompatible completion gates.** Dependencies
   calls secure entropy “a prerequisite” and says M0175 is waiting for FEAT_RNG/Zkr, then immediately
   permits a visible predictable-identity mode until those exist (`docs/todo/P02M0175.md:419-440`);
   the Definition of done likewise accepts that degraded mode (`:442-454`). Decide whether secure
   entropy blocks M0175 or only its stronger unguessability gate, and align the dependency, release
   matrix, and Definition of done with that one answer.

PLANNER'S RESPONSE ON M0175 (2026-09-01T03:14:09Z):

**1. The public-contract correction remains incomplete beyond the justified rejection of duplicated
field order and ordinals - ACCEPTED, and the fetch half was a contradiction I introduced.**

The finding concedes the division I drew last round - order, ordinals and spelling stay in the IDL -
and holds the half that division does not cover: WHICH fields exist and what the changed signatures
are. That is right, and a bound on a record nobody has described bounds nothing.

The fetch contract is the sharp case and it is worse than incomplete. I wrote "fetch body 65536 in
chunks of at most 4096" as if it were a bound, and it is not a shape this wire has: `fetch` returns
one inline `list<u8>`, and LSIDL caps a list length prefix at `u16` - so 65536 is not merely large,
it is one more than the encoding can express. I put a number in a table without checking that the
operation could carry it.

Plan changes: `fetch` returns a `stream<chunk>` - the shape `listener` already uses to hand back a
channel - with chunks of at most 4096 read until the stream ends. Nothing inline carries a body, so no
total has to fit a list length, and the per-flow receive budget is what bounds a client's outstanding
data; a body exceeding it ends the stream with the truncation signal rather than refusing. The table
row becomes the chunk size. And the semantic fields are named: the interface identity as an opaque
`u32` index plus a `u64` generation (the generation being what makes a scoped address refuse after
the interface is replaced), the route, router and DNS-server field sets, and the changed `listen`,
`accept` and live-socket signatures.

**2. The accepted numeric-budget correction did not supply M4's inbound-state budgets - ACCEPTED.**

Correct, and it is the same defect one item along from where I fixed it. I extended M1's table with
the migration's bounds and left M4 asking the implementer for four more numbers. The finding is also
right that the socket count does not resolve them: a half-open TCB is expressly the state with no
socket channel, and unacknowledged transmit bytes say nothing about receive storage.

Plan change: five numbers - 64 half-open, 32 established-but-unaccepted per listener and 64 across the
service, 128 total TCBs bounding the three together, 2 MiB aggregate receive and 4 MiB aggregate
transmit - each tested at its exact bound and one past it, with the over-bound case refusing and
leaving existing connections untouched, since a flood that evicts established state is the same denial
of service by another route.

**3. The new "bounded basic sender profile" still leaves its interoperability policy unchosen -
ACCEPTED.**

Correct. I wrote "the standard RTO computation with its floor and ceiling" and named neither an RFC
nor a value, and did the same for the initial window, the loss response and the retry limit. Two
implementations can pass every qualitative test in that item while agreeing on nothing an interoperable
sender needs to agree on. "Standard" is not a specification.

Plan change: RFC 6298 for the RTO and RFC 5681 for the windows and loss response, named, with the
values - initial RTO 1 s until the first measurement, floor 200 ms, ceiling 60 s, doubling backoff;
initial window 10 segments, loss halving the window and setting ssthresh to the halved value, standard
additive increase; 8 retransmissions of a segment and 5 for a SYN before a typed close, which with the
backoff and ceiling is roughly the order RFC 1122's R2 asks for. None of this is the advanced
congestion control the milestone refuses; it is the profile that makes the refusal meaningful.

**4. The entropy correction gives the milestone two incompatible completion gates - ACCEPTED.**

Correct, and my own text does both: Dependencies calls secure entropy "a prerequisite" and says this
milestone is "waiting on it", then permits a visible degraded mode until it exists, and the Definition
of done accepts that mode. A prerequisite that can be shipped without is not one, and a reader could
take either sentence as the release condition.

Plan change: one answer - it gates the UNGUESSABILITY CLAUSE, not the milestone. M0175 does not wait:
everything it owns is implementable and testable today and the correlation contract lands on all three
targets. The dependency heading says so, the "until then" list is rewritten around it, and the release
matrix is made to carry the same answer - x86_64 claims unguessable identities, the other two record
the degraded mode as a known visible limitation, and no row is blocked. When a secure source lands on
a target, that target's row moves and nothing else changes, which is what makes it a gate on one
clause rather than a dependency of the whole.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T03:39:33Z):

Rating: 5/10

1. **The latest sender-profile correction pins standards that its concrete policy contradicts.** M2
   names RFC 6298 for RTO and RFC 5681 for loss/windows, then specifies a 200 ms RTO floor, an
   unconditional ten-segment initial window and a generic loss response that halves the congestion
   window (docs/todo/P02M0175.md:191-205). [RFC 6298 section 2](https://www.rfc-editor.org/rfc/rfc6298.html#section-2)
   recommends rounding a computed RTO below one second up to one second, so 200 ms is an undeclared
   deviation. RFC 5681's initial window is two to four segments; IW10 comes from
   [RFC 6928 section 2](https://www.rfc-editor.org/rfc/rfc6928.html#section-2) and has a byte-capped
   formula. Most materially, [RFC 5681 section 3.1](https://www.rfc-editor.org/rfc/rfc5681.html#section-3.1)
   halves FlightSize into ssthresh after timeout but resets cwnd to one SMSS; “halve the window”
   permits sending too much immediately after an RTO. Choose one coherent profile, distinguish
   timeout from fast-recovery loss, and make the numeric tests assert that policy.

2. **The accepted fetch-stream correction still has neither the budget nor the wire terminal it
   invokes.** M1 says fetch returns stream<chunk>, that a body exceeding “the per-flow receive budget
   below” ends with an explicit truncation signal, and that typed refusals apply elsewhere
   (docs/todo/P02M0175.md:101-116). No per-flow receive budget follows: 256 KiB is unacknowledged
   transmit data, while M4 supplies only a 2 MiB aggregate receive cap (:113-116,247-251). Nor does
   the plan define whether stream opening is guarded or how an element/terminal distinguishes normal
   completion, truncation and midstream failure. The current chunk carries only data
   (src/idl/network.lsidl:158,162-165), and LSIDL requires result<stream<T>, E> when opening must return
   a typed error (docs/LSIDL.md:368-383). Define the numeric per-flow receive threshold and the
   semantic open/terminal status; exact declaration spelling and order can remain in the IDL.

3. **The entropy narrowing still leaves an impossible spoof-rejection completion claim.** M6 says a
   guessed matching tuple is accepted and therefore requires the tuple to be unpredictable
   (docs/todo/P02M0175.md:304-315). Dependencies then explicitly permit predictable identities on
   aarch64 and riscv64 and gate only the stronger unguessability claim (:474-505). The Definition of
   done nevertheless still says absolutely that a spoofed datagram can never resolve a name, set the
   clock or change a lease on any profile (:514-520). On a predictable profile, an off-path forged
   response carrying the predicted tuple is precisely a spoofed datagram and passes correlation.
   Scope off-path spoof rejection to secure-entropy profiles; all profiles can promise rejection of
   mismatched, stale and replayed packets.

4. **The newly frozen route fields cannot represent the on-link routes supplied by M0174.** M1 makes
   every route carry a next-hop ip-address with no absent/direct form or sentinel semantics
   (docs/todo/P02M0175.md:126-127). M0174 explicitly installs an on-link prefix route for PIO L=1 and
   treats a caller-chosen next hop as optional (docs/todo/P02M0174.md:231-234,312-325). A direct route
   has no fixed gateway; its packet's destination is the next hop. Requiring an address forces either
   a fabricated value or an undocumented sentinel in the public contract. Freeze an optional
   next-hop or direct/via route variant and its validation; this is a semantic field choice, not IDL
   spelling that can be deferred.

PLANNER'S RESPONSE ON M0175 (2026-09-01T12:12:00Z):

All four findings ACCEPTED. Three of them are corrections to text the previous round added, which is
worth saying at the top: naming a standard, freezing a stream shape and narrowing an entropy claim
each fixed a real defect and each left a new one behind it.

**Finding 1 - the sender profile pins standards its concrete policy contradicts. ACCEPTED, on all
three counts.**

Checked against the documents rather than against the names:

- RFC 6298 section 2 rule 2.4 says a computed RTO below one second SHOULD be rounded up to one
  second. The row named RFC 6298 and specified a 200 ms floor, silently - which is the one thing
  naming a standard is supposed to prevent.
- RFC 5681 section 3.1 gives an initial window of two to four segments. IW10 is RFC 6928, and it has
  a byte-capped formula rather than a flat segment count.
- Most materially, "a loss response that halves the window" is the FAST-RECOVERY shape. RFC 5681
  section 3.1 sets `ssthresh = max(FlightSize/2, 2*SMSS)` after a timeout and then `cwnd = 1*SMSS`.
  One "loss response" applied to both cases would put half a window back on a path that had just
  stopped delivering, which is the opposite of what a timeout means. The auditor is right that this
  is the one with a behavioural consequence, not just a citation error.

The profile is now one coherent policy with each number attributed:

- the 200 ms floor STAYS and is recorded as a declared deviation, with the RFC's own note for that
  rule - the large minimum is a conservative choice made for coarse-grained clocks, and the document
  anticipates a smaller one being justified - and the reason it applies here: a bounded appliance on
  an emulated link with a millisecond clock, where a one-second minimum turns every ordinary loss
  into a one-second stall;
- the initial window is RFC 6928 section 2, `min(10*SMSS, max(2*SMSS, 14600))`, byte cap included;
- timeout and duplicate-ACK loss are separated: RTO gives `ssthresh = max(FlightSize/2, 2*SMSS)` then
  `cwnd = 1*SMSS` and slow start; three duplicate ACKs give fast retransmit and fast recovery per
  section 3.2. The plan now says explicitly that this is RFC 5681's core and NOT the "advanced
  congestion control" the milestone refuses - that refusal is about CUBIC, BBR, ECN and SACK-based
  recovery - because a reader could otherwise take the refusal as licence to omit fast retransmit;
- and the numeric tests assert the policy: after a timeout `cwnd` is one SMSS and not half of
  anything, after three duplicate ACKs it is `ssthresh + 3*SMSS`, and the initial window is the RFC
  6928 formula at the fixture's SMSS.

**Finding 2 - the fetch stream has neither the budget nor the terminal it invokes. ACCEPTED.**

Both halves confirmed. The stream clause bounds a body by "the per-flow receive budget below" and no
such budget followed: 256 KiB is unacknowledged TRANSMIT data - a different direction and a different
question - and M4's 2 MiB is an aggregate across inbound state. So the one number the truncation rule
depends on did not exist. And the opening and the terminal were both undefined, while `chunk` carries
only data today.

Frozen: `MAX_FETCH_BODY_BYTES` is 256 KiB per fetch, derived rather than picked - it is the same
order as the per-flow unacknowledged TRANSMIT budget, and eight concurrent full-size bodies fit
inside M4's 2 MiB aggregate receive cap, so one flow's bound cannot by itself exhaust the service's.
I set it at a megabyte first and corrected it in the same round: under a 2 MiB aggregate that let two
fetches take the whole thing, which is not a per-flow bound so much as a way to spell the aggregate
twice. A body reaching it ends with the truncation terminal rather than a refusal - the caller keeps what it received, which is what makes this the one
place truncation is allowed. The operation becomes `result<stream<chunk>, error>`, so a refusal before
any body exists is the typed error every other operation uses rather than a zero-length stream the
caller must interpret. Once open the stream ends in exactly one of three ways - COMPLETE, TRUNCATED,
FAILED - because a caller that cannot tell them apart cannot know whether it holds the whole resource.
Which field carries the outcome is left to the IDL, as the auditor allows; the three outcomes and the
guarded open are semantics and are frozen here.

**Finding 3 - the entropy narrowing leaves an impossible spoof-rejection claim. ACCEPTED.**

The Definition of done said absolutely that "a spoofed or replayed datagram can neither resolve a
name, set the clock, nor complete or invalidate a lease", and the sentence immediately after it
carved out the case that makes the first false. On a profile whose transaction IDs and source ports
are predictable - which the dependency section explicitly permits on aarch64 and riscv64 - an
off-path attacker forges a datagram carrying the predicted tuple, and that datagram is spoofed and
does resolve a name.

The clause now separates what every profile can promise from what only a seeded one can: on EVERY
profile a datagram whose tuple does not match a live request is rejected and one arriving after its
request completed or expired matches nothing, so MISMATCHED, STALE and REPLAYED datagrams are
refused; on profiles with a secure entropy source, and only there, an off-path attacker cannot
produce a matching tuple either. A parenthesis records what the clause said and why it could not hold.

**Finding 4 - the frozen route fields cannot represent M0174's on-link routes. ACCEPTED.**

Correct and concrete. M0174 installs an on-link prefix route for every PIO with `L=1`, and an on-link
route has no gateway - the destination is its own next hop. A mandatory next-hop `ip-address` forces
either a fabricated value or an all-zeros sentinel whose meaning is written down nowhere, which is
how two consumers come to disagree about what an unspecified address means in a public contract.

The route now carries one of two forms: DIRECT, which carries no address and whose packet's
destination is its own next hop, with neighbour resolution run against it; or VIA(a), where `a` must
be a unicast address on the same interface identity - a router's link-local in every case M0174
produces - with the unspecified and multicast forms refused at validation rather than at use. The
plan says in the row that this is a semantic field choice decided here and that only the variant's
spelling belongs to the IDL, which is the distinction the finding draws.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T13:23:01Z):

Rating: 5/10

1. **The accepted RFC 6724 correction still delegates the policy it claims to pin.** M5 says to pin
   an "applicable RFC 6724 profile", including its policy table, appliance tie-breaks, fallback
   timing, and whichever RFC 8028 update it adopts, but supplies none of those choices
   (`docs/todo/P02M0175.md:339-373`). The planner's original response claimed that this wording pins
   them (`AI/audit/audit-M0175.md:138-141`); it only instructs the implementer to choose them. The
   mixed-family and deterministic-fallback tests therefore have no exact oracle. Record the actual
   policy-table rows, complete ordering/tie-break rules, RFC 8028 decision, and fallback timing in the
   plan.

2. **The entropy correction leaves M6's normative contract contradicting the release contract.** M6
   still says the matched DNS tuple **must** be unguessable, mandates fresh unpredictable transaction
   IDs and source ports on every query, and requires negatives against predictable reuse
   (`docs/todo/P02M0175.md:375-397`). The dependency and Definition of done now explicitly permit
   predictable query identities on AArch64 and RISC-V until secure entropy exists, limiting
   unguessability to seeded profiles (`:551-605`). Those are incompatible completion rules for two of
   the three target profiles. Make M6 itself conditional in the same terms as the release matrix and
   separate all-profile correlation tests from secure-profile unguessability tests.

3. **The accepted numeric-bounds correction still omits the DNS result and parser bounds.** M6
   returns a "bounded" ordered address list and requires bounded CNAME and name-compression traversal
   (`docs/todo/P02M0175.md:375-409`), while M10 promises exact-bound and over-bound CNAME/answer tests
   (`:525-540`). M1's numeric table contains no maximum returned-address count, CNAME-chain depth,
   answer-record count, or compression traversal/pointer limit (`:54-101`). Consequently the wire
   shape, resource ceiling, overflow behavior, and test oracle remain implementation choices. Add the
   missing numbers and typed over-bound outcomes within the existing DNS item.

PLANNER'S RESPONSE ON M0175 (2026-09-01T14:40:00Z):

All three findings ACCEPTED. All three are places where a previous round's correction stopped one
step short of the thing it claimed to do, which is a pattern in this file worth naming rather than
repeating a fourth time.

**Finding 1 - the RFC 6724 correction still delegates the policy it claims to pin. ACCEPTED.**

The text says to "pin the applicable RFC 6724 profile ... including the policy table, the tie-breaks
this appliance uses, and any RFC 8028 update it adopts", and supplies none of them. That is an
instruction to choose, and I claimed in an earlier response that it pinned them. It did not, and the
mixed-family and deterministic-fallback tests had no oracle: two implementations could disagree about
every ordering decision and both follow the file.

Now actually pinned, in the item:

- THE POLICY TABLE is RFC 6724 section 2.1's default, written out row by row, with no appliance rows
  and NOT configurable in this milestone - an operator-editable table is a way to make two machines
  running one image select differently, and nothing here needs it.
- THE RULES are sections 5 and 6 in full and in order, with the existing deprecated-address
  correction. Ties surviving every rule break on M0174's enumeration order, which is stable - a
  "deterministic fallback" test needs determinism to be a property of something.
- RFC 8028 is ADOPTED: when the chosen source came from a prefix a particular router advertised, that
  router is preferred as next hop. M0174 already enumerates routes with their routers, so the input
  exists, and without it a multihomed appliance can send a packet whose source belongs to one upstream
  through another - which some upstreams drop.
- FALLBACK TIMING is sequential, not raced: candidates in rule order, one at a time, each bounded by
  the existing per-attempt connect timeout. RFC 8305's racing is both the "advanced" behaviour this
  milestone refuses and the thing that would make the fallback nondeterministic.

**Finding 2 - M6's normative contract contradicts the release contract. ACCEPTED.**

Correct. Last round I scoped the Definition of done to secure-entropy profiles and left M6 saying the
tuple MUST be unguessable, with mandatory unpredictable identities on every query. Those are
incompatible completion rules for two of the three targets: an implementer following M6 would have to
fail the milestone on the profiles the release contract accepts. I fixed the surface a reader reaches
last and left the one they reach first.

M6 is now conditional in the same terms, and its tests split the same way. On EVERY profile the
correlation contract is required - the fields compared, the redraw on collision, the retirement on
completion or expiry - because those are about matching, not entropy. On a profile with a secure
source, and only there, identities are additionally drawn unpredictably and the unguessability
negatives run; on one without, the service says so at start-up and those tests are not run rather
than run and failed, because a test that cannot pass on a supported profile is a broken gate rather
than evidence.

**Finding 3 - the numeric correction omits the DNS result and parser bounds. ACCEPTED.**

Also correct, and the same shape: I added the transport and fetch numbers and left M6's "bounded"
result list and "bounded" CNAME and compression traversal as adjectives, while M10 promises
exact-bound and over-bound fixtures against them. Four numbers now sit in M1's table with their
over-bound outcomes:

    MAX_RETURNED_ADDRESSES   8    beyond it the remaining candidates are DROPPED, not refused - the
                                   list is already in M5's order, so what is kept is what would have
                                   been tried first, and a name with nine addresses is not an error
    MAX_ANSWER_RECORDS      32    a response declaring more is a typed refusal, and the query is NOT
                                   retried against the same server
    MAX_CNAME_CHAIN          8    hops before the answer is refused as a loop or an implausible chain
    MAX_COMPRESSION_JUMPS   16    pointer follows per name, and every pointer must target a strictly
                                   EARLIER offset - the backward rule is what makes a loop impossible
                                   rather than merely bounded, and the count bounds the
                                   legal-but-absurd case that is still an attack

Each gets the exact-bound and over-bound pair M10 promises, with the typed refusal named beside it
rather than a truncation.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T15:22:06Z):

Rating: 5/10

1. **The adopted RFC 8028 policy still depends on provenance that M0174's frozen seam does not
   provide.** M5 says M0174 already enumerates routes with both their routers and their advertising
   prefix, and uses that association to prefer the router which advertised the chosen source prefix
   (`docs/todo/P02M0175.md:402-407`). M0174 freezes only usable source addresses, matching routes,
   their routers, and PMTU; its PIO/default-router state never requires retaining or returning which
   router advertised which source prefix (`docs/todo/P02M0174.md:248-277,362-386`). A set of routes
   and routers cannot reconstruct that relation when different routers advertise different prefixes.
   Either add the missing PIO-to-router provenance to M0174's owned state and candidate result, or do
   not claim that this RFC 8028 choice is implementable through the frozen seam.

2. **The accepted public-contract correction still does not freeze the listener or caller-override
   semantics its tests require.** M1 asks for conflict/reuse rules for IPv4-only, IPv6-only and
   dual-stack wildcard listeners but supplies no conflict matrix, reuse decision, or valid mapping
   between a single local scoped endpoint and a dual-family wildcard
   (`docs/todo/P02M0175.md:54-70,203-209`). M5 also requires and tests an explicit caller-selected
   source, including a deprecated one, but M1 names no changed `connect` request or other public
   operation through which a caller can express that source (`:418-437`). The current wire still has
   only `connect(endpoint)` and `listen(port)` (`src/idl/network.lsidl:124-133`). Consequently M10's
   same-port bind cases and M5's explicit-override case have no contract-level oracle or invocation.
   Field order and ordinals can remain solely in the IDL, but these semantic choices and the request
   shapes that carry them must be decided in the plan.

3. **M4's newly numeric admission policy is internally contradictory and its promised reporting is
   absent from M1.** The correction first says every over-bound case is a refusal which leaves all
   existing connections untouched, then immediately asks implementation to choose an "admission or
   eviction rule" (`docs/todo/P02M0175.md:342-359`). Eviction is incompatible with the preceding
   normative refusal and with its exact/one-past test. The same paragraph promises capacity and drop
   reporting through `capacity`, but M1's supposedly complete semantic-field list does not add any
   half-open, unaccepted, TCB, RX/TX-byte, refusal, or drop fields; the existing record exposes only
   four live counts (`src/idl/network.lsidl:40-48`). Choose admission/refusal consistently and freeze
   the observable counters needed to verify each declared budget.

4. **The accepted event-driven-runtime correction still leaves both its bound and its invalidation
   transitions to implementation.** M7 requires a "bounded" durable event/pending-operation store
   without a numeric capacity, per-kind accounting, or full-store outcome
   (`docs/todo/P02M0175.md:490-500`), despite the plan's own corrected rule that resource-policy
   numbers and over-bound behavior cannot be deferred. It also says an invalidated operation will
   receive a defined error or close/reselect "according to" state without defining which live states
   take which transition; M5 points to this as "M7's rule", but no such rule or error mapping exists
   (`:414-416`). This is the exact state-loss/runtime redesign accepted from the original audit, so
   its capacity and state-transition table need concrete oracles rather than another instruction to
   define them during implementation.

PLANNER'S RESPONSE ON M0175 (2026-09-01T17:25:00Z):

All four findings ACCEPTED. Every one of them is a place where a correction of mine named a decision
instead of making it, or claimed an input that did not exist - the pattern this file has now shown
four rounds running, and which I am recording as such rather than as four unrelated slips.

**Finding 1 - the RFC 8028 policy depends on provenance M0174 does not provide. ACCEPTED.**

I wrote that "M0174 already enumerates routes with their routers and their advertising prefix, so the
input exists". It does not. That milestone freezes usable source addresses, matching routes, their
routers and the PMTU; nothing in its PIO or default-router state retains WHICH router advertised
which prefix, and a set of routes plus a set of routers cannot reconstruct the relation once two
routers advertise different prefixes. I asserted an input rather than checking the seam I was
consuming - the same shape as this round's M0136 finding 3.

Fixed on both sides rather than by withdrawing the policy, because the policy is right: M0174's owned
state and candidate result now carry the originating router of each source address's PIO, retained
when SLAAC forms the address and expiring with it, and this file's row says that is where the input
comes from.

**Finding 2 - the public contract does not freeze listener or caller-override semantics. ACCEPTED.**

Both halves confirmed against the wire, which has only `connect(endpoint)` and `listen(port)`. M10
tests same-port binds and M5 tests an explicit caller-selected source including a deprecated one, and
neither had a contract-level oracle or even an invocation.

Frozen: a bind conflict matrix for one port with no reuse flag in this milestone - IPv4-only and
IPv6-only both admitted, dual-stack conflicting with anything either order, same mode twice refused,
wildcard against a specific local address refused while there is no reuse rule - and an IPv4-mapped
IPv6 address refused as a listen address, matching M3's capability rule. `connect` gains an OPTIONAL
caller-selected source, with the interface identity when scoped; absent means "select for me", and a
source that is not a usable candidate is a typed refusal rather than a silent re-selection, because
a caller that names a source has a reason.

**Finding 3 - M4's admission policy is internally contradictory and its reporting is absent.
ACCEPTED.**

Right on both counts. The paragraph says every over-bound case is a refusal that leaves existing
connections untouched, and then asks for "an admission or eviction rule" - eviction being exactly
what the sentence before rules out, and incompatible with the exact/one-past test. There is no choice
to make: a budget that is reached refuses the NEW state and nothing admitted is dropped. The plan now
says that and says why the alternative was struck.

And the reporting it promised did not exist: `capacity` carries four live counts and none of the five
budgets, so every one was unobservable from outside and no test could check a declared bound against
the service's own view. It gains half-open TCBs, established-but-unaccepted, total TCBs, receive bytes
held, transmit bytes unacknowledged, and two monotonic counters - admissions refused for a budget,
and datagrams dropped. Named as semantics; order and ordinals stay the IDL's.

**Finding 4 - M7's bound and invalidation transitions are left to implementation. ACCEPTED.**

The sharpest part of this is that M5 points at "M7's rule" for the invalidation error and no such
rule existed. And "bounded" with no number, in the item that exists BECAUSE state loss was the
original defect, is the deferral this file has already corrected twice elsewhere.

The store is 256 entries, accounted per kind so one cannot starve another - 128 pending
DNS/SNTP/DHCP operations, 64 queued L3 invalidations, 64 timer wakeups - and a full store refuses the
new entry rather than dropping an old one, as a typed error to the operation that caused it. The L3
invalidation queue keeps its own overflow answer from M0174, RESYNC-REQUIRED, because a consumer that
re-reads the tables loses nothing.

The invalidation transitions are now a table by state: not yet sent reselects silently; a sent UDP
request fails with `address-unavailable`, because reselecting would change the tuple its reply is
correlated against; TCP still connecting fails the same way; TCP established CLOSES after flushing
what is acknowledged, and is not reselected because a connection is its tuple and a new source is a
connection the peer knows nothing about; a listener stays, being bound to a port and a mode rather
than to a route.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T17:59:17Z):

Rating: 6/10

1. **The new invalidation table collapses distinctions that M1's public contract explicitly adds.**
   It silently reselects every not-yet-sent operation and keeps every listener
   (`docs/todo/P02M0175.md:539-565`). M1, however, says an explicit caller-selected source must be
   refused rather than silently replaced (`:219-227`), and supports specific-address listeners as
   well as wildcards (`:203-229`). If that explicit source is invalidated before send, silent
   reselection violates the caller's authority; if a specifically bound local address is invalidated,
   the listener cannot remain published on an address the interface no longer owns. Split the table:
   automatically selected unsent work may reselect, explicit-source work must fail, wildcard
   listeners may stay, and specific-address listeners need a defined withdrawal/error transition.

2. **The adopted RFC 8028 selection still consumes one origin for a relation that can contain several
   routers.** M5 prefers “the” router which advertised the chosen source prefix and says M0174 returns
   one originating router (`docs/todo/P02M0175.md:436-447`). RFC 8028 sections 3.1-3.3 explicitly
   covers several routers advertising the same prefix: selection is restricted to all such routers
   and then applies the RFC 4191 criteria. The singular input makes the result depend on which RA was
   retained and loses a still-live alternative when that one expires, so the promised multi-router
   source-aware choice is not implementable deterministically. Consume M0174's bounded set of live
   advertisers and select within it using the already-pinned router order; add the same-prefix,
   multiple-advertiser case to M10.

PLANNER'S RESPONSE ON M0175 (2026-09-02T00:15:00Z):

Two findings, both ACCEPTED. Both are tables I added in the previous round that flattened a
distinction the milestone's own public contract makes.

**Finding 1 - the new invalidation table collapses distinctions M1 explicitly adds. ACCEPTED.**

Both halves check out against M1 and both are behaviour changes rather than wording.

The "not yet sent → RESELECT silently" row covered two cases that M1 separates. M1 freezes `connect`
with an OPTIONAL caller-selected source and says a source that is not a usable candidate is a typed
refusal "rather than a silent re-selection - a caller that names a source has a reason, and quietly
using a different one is the failure this override exists to prevent". An override that survives
until the moment it matters and is then discarded is worse than not having one: the caller is told
the connection succeeded and it went out of the address they were avoiding. The row is now two rows -
automatically chosen sources reselect, caller-named sources fail with `address-unavailable` and the
caller re-asks if it wants automatic selection.

"A listener STAYS" was written as though every listener were a wildcard. M1's `listen` takes a local
SCOPED ENDPOINT and its own bind-conflict matrix has a "wildcard + a specific local address" row, so
a specifically bound listener is expressible - and one left published on an address the interface no
longer owns is a listener that can never accept again while holding a port against the binds that
could. That is now two rows as well: a wildcard listener stays, and a specifically bound one is
withdrawn with `address-unavailable` on the listener channel, which then closes and releases the
port. M5's own one-line summary of the same transition carried the unsplit version too - "it
re-selects if it has not yet sent" - and is corrected to point at the table with the distinction
named, because a rule stated in two places is a rule that can be followed in two ways. Two consequences are stated rather than left to the implementer: sockets already accepted on
that listener follow the TCP row on their own tuples and are not torn down with it, and the released
port may be bound again when the address returns.

**Finding 2 - the adopted RFC 8028 selection consumes one origin for a relation that can hold several
routers. ACCEPTED, and it is the same defect as its sibling in M0174 seen from the consuming end.**

M5 said "the router which advertised the chosen source prefix" and cited M0174 as returning one
originating router. That input makes the result depend on which RA was retained and loses a still-live
alternative when that one expires, so the multi-router source-aware choice this row promises was not
implementable deterministically - which matters here more than in M0174, because this is the milestone
that has to produce a repeatable answer.

M0174's side is corrected in the same round to a bounded set of live advertisers with independent
lifetimes and RFC 4191 preferences, and M5 now consumes that set. The rule is stated once and
completely: restrict the candidate next hops to the advertisers M0174 returns for the chosen source's
prefix, apply RFC 4191 preference among them, and break what survives by the order M0174's
enumeration returned them in - which is the same stable order this section already uses for every
other tie, so "deterministic fallback" stays true of something. The empty case is stated rather than
left undefined: if every advertiser has expired while the address is still valid on the prefix's own
lifetime, the general default-router rules apply and the fallback is RECORDED rather than silent,
because a source-aware choice that quietly stops being source-aware is the failure this adoption
exists to prevent.

M10 gains the cases the singular input could not express: two routers advertising the same prefix
with the higher RFC 4191 preference selected, the equal-preference case run twice to prove the choice
is stable rather than dependent on RA arrival, the selected advertiser expiring while the other is
still advertising, and every advertiser expiring while the address remains valid. The invalidation
matrix gets a row per authority in the same list - automatic versus caller-named source, wildcard
versus specifically bound listener - so finding 1's split is tested and not only written.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-01T23:25:58Z):

Rating: 6/10

1. **The accepted listener-contract correction still omits the valid mapping between its bind mode
   and its one local endpoint.** M1 makes `listen` always take one scoped endpoint plus one of
   IPv4-only, IPv6-only, or dual-stack-wildcard modes, and now gives conflict outcomes
   (`docs/todo/P02M0175.md:203-230`). It still never says how a single-family wildcard versus a
   specific address is represented, which address family is legal with each mode, or what endpoint a
   dual-family wildcard carries. Those distinctions are not optional: M7 separately handles wildcard
   and specific-address listeners, and M10 tests all three modes (`:595-608,713-729`). Two reasonable
   IDLs can therefore use an unspecified-address sentinel, an ignored endpoint, or an optional
   endpoint and all claim to follow the plan, while producing incompatible validation and
   invalidation behavior. Freeze the valid endpoint/mode combinations and wildcard representation;
   field order and ordinals can remain in the IDL as intended.

2. **The required empty-advertiser fallback is impossible under M0174's current provider contract.**
   M5 and M10 require an address to remain valid after every advertising router expires, at which
   point general default-router selection applies and the fallback is recorded
   (`docs/todo/P02M0175.md:436-462,719-726`). M0174's newly corrected seam instead says the prefix
   survives while an advertiser does and its fixture removes the prefix when the last advertisement
   expires (`docs/todo/P02M0174.md:442-463`). The consumer's required state can therefore never be
   supplied, or M0175 must retain source state that its ownership boundary assigns exclusively to
   M0174. Align the prerequisite so advertiser-set membership expires independently of the still-live
   prefix/address, then keep the already-specified empty-set fallback fixture.

PLANNER'S RESPONSE ON M0175 (2026-09-02T04:00:00Z):

Two findings, both ACCEPTED.

**Finding 1 - the listener contract omits the mapping between its bind mode and its one local
endpoint. ACCEPTED.**

The finding is right that this is not optional detail. M1 gives `listen` a bind mode, a local scoped
endpoint and a backlog, and never says how a wildcard is represented in that endpoint, which family
is legal with each mode, or what a dual-stack wildcard's endpoint carries - while M7 gives wildcard
and specific-address listeners different invalidation outcomes and M10 tests all three modes. So the
distinction is load-bearing and unfrozen, and two implementations could use an unspecified-address
sentinel, an ignored field or an optional field, each follow the plan, and behave differently on
validation and invalidation. I added the M7 rows in the previous round and did not notice that they
read a property M1 never defines.

The table is frozen: the port is always present and the address is what varies. IPv4-only takes
either the unspecified IPv4 address, which binds every IPv4 address, or a specific IPv4 address;
IPv6-only the same in its family; and a dual-stack wildcard takes the unspecified IPv6 address and
nothing else. A dual-stack wildcard carrying a SPECIFIC address is a typed refusal - a bind covering
two families cannot name one address in one of them - and so is an address whose family disagrees
with a single-family mode.

THE WILDCARD IS THE UNSPECIFIED ADDRESS AND NOT AN ABSENT ONE, which is the choice among the three
the finding lists. An optional or ignored endpoint would be a second way to say the same thing and a
first way to say something undefined; the unspecified address is what these protocols already mean by
"any", and it makes "is this listener a wildcard" one comparison rather than a convention. M7's two
rows are relabelled against it explicitly: a listener whose address is the unspecified one is the
wildcard row, and any other is the specific-address row.

**Finding 2 - the required empty-advertiser fallback is impossible under M0174's provider contract.
ACCEPTED, and the defect is on M0174's side.**

Correct, and it is the mirror of the M0174 finding I accepted in the same round. My M0174 correction
said the prefix survives while an advertiser does and its fixture removed the prefix when the last
advertisement expired - so the state this milestone requires, every advertiser gone with the address
still valid, could never be supplied. The alternative the finding names is worse and I want to record
that I rejected it: retaining that source state here would mean M0175 holding address lifetime
information the ownership boundary assigns exclusively to M0174, which is the seam this pair of
milestones spent three rounds getting straight.

So the fix is on the prerequisite and it is made there: M0174 now expires the prefix and its
addresses on their own RFC 4862 lifetimes, treats advertiser membership as provenance with a clock of
its own, and adds the empty-set-with-valid-address fixture alongside a separate prefix-expiry one.
M5's fallback clause now says that state is suppliable and was not, so a reader meeting the clause
learns why it is there rather than assuming it always worked. M10's cases are unchanged - they
already listed the empty-set fallback - and they are now reachable.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-02T04:24:30Z):

Rating: 5/10

1. **The RFC 8028 correction reverses the prerequisite's frozen reachability-first router order.**
   M0174 orders routers by reachability first and advertised preference second, expressly to keep an
   unreachable high-preference router behind a reachable lower-preference one
   (`docs/todo/P02M0174.md:276-297`). M0175 instead says to apply RFC 4191 advertised preference
   within the selected source prefix's advertiser set and use M0174's enumeration only to break what
   survives (`docs/todo/P02M0175.md:466-479`); its fixture likewise requires the higher-preference
   advertiser without constraining reachability (`docs/todo/P02M0175.md:756-760`). An unreachable
   high-preference advertiser can therefore win before the reachability order is consulted, reopening
   the exact selection defect M0174 corrected and giving the two sides of the frozen seam different
   policies.

2. **The timer-store bound cannot cover the already-admitted TCP state and has no safe overflow
   outcome.** M7 promises that no deadline is starved, but reserves only 64 durable timer-wakeup
   entries and says a full partition refuses the new entry with a typed error to the operation that
   caused it (`docs/todo/P02M0175.md:588-607`). M4 permits 128 live TCBs, each of which may own
   retransmission, persist, handshake, or closing timers (`docs/todo/P02M0175.md:285-425`). A timer
   becoming due is autonomous work for an already-admitted connection, not a new
   caller operation that can receive that generic refusal. With more than 64 due TCP timers, the plan
   specifies neither coalescing/rescan nor a connection-level failure transition, so it cannot uphold
   its no-starvation guarantee within its own declared capacities.

PLANNER'S RESPONSE ON M0175 (2026-09-02T14:05:00Z):

Two findings, both ACCEPTED. The first is a defect in the correction I made last round.

**Finding 1 - the RFC 8028 correction reverses the prerequisite's frozen reachability-first order.
ACCEPTED.**

Correct, and it is precise about the damage. M0174 orders routers by REACHABILITY first and
advertised preference second, deliberately and with its own reasoning written down: preference-first
would put an unreachable high-preference router ahead of a reachable lower-preference one. My
correction then said to restrict the candidates to the advertiser set, "among them apply RFC 4191
preference", and use M0174's enumeration only to break what survived - which rebuilds that order with
the two keys swapped. An unreachable high-preference advertiser wins before reachability is
consulted, and the two sides of a seam this pair of milestones spent several rounds freezing end up
with different policies.

The mistake is a specific one and I would rather name it than describe it: I treated M0174's
enumeration as a TIE-BREAK because that is how the surrounding RFC 6724 text uses its other inputs,
without noticing that this particular input is not a list to be re-sorted - it is already the answer
to "which of these routers first", including the RFC 4191 key I was re-applying on top of it.

The rule is now that the order is consumed WHOLE: restrict the candidate next hops to the advertisers
M0174 returns for the chosen source's prefix, and take the FIRST of them in M0174's own order.
Restricting is this milestone's decision, because that is the RFC 8028 policy; ordering is not, and
saying so in one sentence removes the divergence rather than aligning two orderings that could drift
again.

M10's fixture is corrected with it, because it asserted the same inverted rule: it required the
higher-preference advertiser to win without constraining reachability. It now runs twice - once where
the two differ only in preference, so the higher wins, and once where the higher-preference advertiser
is UNREACHABLE and the reachable lower-preference one must win anyway. The second case is the one
that tells the new rule apart from the one it replaced, which the old fixture could not have done.

**Finding 2 - the timer-store bound cannot cover the admitted TCP state and has no safe overflow
outcome. ACCEPTED, and the second half is the sharper of the two.**

Both halves check out. M4 permits 128 live TCBs and one connection can owe a retransmission, a
persist probe, a handshake deadline and a closing timer at once, against a partition of 64 wakeups -
so the bound is smaller than the state the same file has already admitted. And a timer coming due is
AUTONOMOUS work for a connection admitted long ago: there is no caller in flight to hand a typed
error to, so "a full partition refuses the new entry and the refusal reaches the operation that
caused it" answers a question the timer partition never gets asked. The no-starvation promise in the
sentence above it could not hold inside the milestone's own capacities.

The fix is not a bigger number or an overflow rule. The timers stop being entries: ONE SCHEDULER
DEADLINE PER OWNER, and what a connection owes is computed rather than queued - a TCB holds one entry
carrying the EARLIEST of its deadlines, and when that fires it recomputes which of its timers were due
and re-arms for the next. That is the standard shape and it is the one that makes the bound
structural: at most one entry per TCB plus the fixed protocol owners - DHCP, ND, DAD, RA, PMTU - so
128 plus a small constant, sized as 160.

A partition that cannot overflow needs no overflow rule, which is why this replaces the refusal
rather than adding a case to it, and the enforcement point moves to where a caller exists: a
connection that cannot be given its one entry is a connection that is not admitted, which is M4's TCB
budget refusing at admission. The store's total is corrected to 352 - 128 pending operations, 64 L3
invalidations, 160 deadlines - in both places it is stated, and the item's headline figure with it.
Its gates: 128 established connections owing several timers each occupy 128 entries and no more with
every deadline firing, and a connection re-arming while another of its own timers is already due is
served for both without a second entry.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-02T13:02:07Z):

Rating: 4/10

1. **The five-retransmission SYN limit contradicts the plan's own RTO schedule and the base TCP open
   requirement.** M2 starts an unmeasured connection at a 1-second RTO, doubles on each
   retransmission, and closes a SYN after five retransmissions
   (`docs/todo/P02M0175.md:321-368`). Even including the wait after the fifth retry, that schedule
   lasts at most 63 seconds, whereas RFC 9293 requires the SYN retransmission threshold to permit at
   least three minutes ([RFC 9293 section 3.8.3](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.8.3)).
   This is not a performance refinement: a conforming slow or lossy peer is abandoned under one
   third of the required open interval. The numeric profile and its handshake oracle therefore need
   to agree on a limit that reaches the required duration.

2. **The promised lost-final-ACK recovery has no TIME-WAIT state or lifetime and cannot be
   implemented by retaining FIN in the retransmission queue.** M2 says the control block survives
   until the closing handshake is acknowledged or the FIN retry limit expires, then requires a lost
   final ACK to be answered when the peer retransmits FIN
   (`docs/todo/P02M0175.md:300-310,381-385`). The final ACK is not itself acknowledged; after active
   close, TCP retains the tuple in TIME-WAIT for 2 MSL, acknowledges a retransmitted remote FIN, and
   restarts that timer ([RFC 9293 sections 3.6 and 3.10.7](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.6)).
   The plan names only an unspecified "closing timer" in its scheduler discussion and gives no
   TIME-WAIT transition, duration, timer restart, tuple-retention/accounting rule, or exact expiry
   fixture (`docs/todo/P02M0175.md:621-643`). An implementation can therefore free the TCB as soon as
   its own FIN is acknowledged and still claim to follow the stated queue rule, making the required
   lost-final-ACK behavior impossible.

3. **The DHCPNAK fixtures still require opposite outcomes for the same accepted message.** M8 says
   REQUESTING, RENEWING and REBINDING admit an ACK or NAK in their stated server scope, but the
   fixture list then says "a NAK in each phase" leaves the stored lease unchanged
   (`docs/todo/P02M0175.md:727-758`). The immediately following special case correctly requires a
   broadcast NAK answering a unicast renewal to be accepted and invalidate the lease
   (`docs/todo/P02M0175.md:759-762`). The plan never separates invalid/foreign NAK fixtures from an
   accepted NAK's state transition, so ignoring a valid NAK can satisfy the generic oracle while
   retaining a lease the server rejected. State explicitly which rejected NAKs leave state unchanged
   and that an accepted NAK clears any live lease and returns the client to the appropriate discovery
   state.

4. **The original accepted correction still defers `send`'s public completion semantics instead of
   deciding them.** The first audit required the plan to choose exactly when `socket.send` reports
   bytes accepted versus bytes acknowledged. M2 still repeats those alternatives as something the
   implementation must define (`docs/todo/P02M0175.md:290-296`), while the existing public operation
   already returns a byte count (`src/idl/network.lsidl:150-158`). With a newly queued,
   backpressured sender, returning accepted bytes permits completion before delivery and returning
   acknowledged bytes can keep the RPC open across retransmission; those are observably different
   client contracts, buffer lifetimes and failure results. Leaving the choice to implementation is
   an incomplete correction at the milestone's central transport seam.

5. **M7's no-starvation scheduler omits timers from the frozen M0174 contract it claims to
   consume.** M0174 supplies one aggregate L3 deadline over DAD, RS, ND retry, NUD, prefix, address,
   router, RDNSS, PMTU and MLD timers (`docs/todo/P02M0174.md:335-342`). M0175 instead enumerates
   only DHCP, ND, DAD, RA, PMTU and TCP, and sizes its fixed non-TCB owners from the same shortened
   list (`docs/todo/P02M0175.md:597-635`). `ND` cannot implicitly cover the omissions when DAD and RA
   are already listed separately and prefix/address/router/RDNSS/MLD expiry are independent state.
   The plan can therefore satisfy its 160-entry arithmetic while starving state that its prerequisite
   requires to advance. Treating M0174's already-aggregated next deadline as one explicit L3 owner
   would preserve the frozen boundary and make the structural bound and no-starvation gate
   implementable.

PLANNER'S RESPONSE ON M0175 (2026-09-03T09:30:00Z):

All five ACCEPTED. This was the weakest of the nine plans this round and the rating was fair.

FINDING 1 - the five-retransmission SYN limit contradicts the plan's own RTO schedule and the base
open requirement: ACCEPTED. The arithmetic is the plan's own: a 1-second initial RTO doubling over
five retransmissions is 1+2+4+8+16 seconds of retries and 32 more waiting on the last - 63 seconds -
against RFC 9293 section 3.8.3, which requires the SYN retransmission threshold to permit at least 3
minutes. A conforming peer behind a slow or lossy path is abandoned inside a third of the interval
the base specification reserves for opening a connection.

PLAN CHANGE: the SYN limit is stated as the thing the RFC states - the schedule MUST span at least 180
seconds before the open fails with a typed timeout - and the count is DERIVED from the RTO parameters
rather than written beside them where the two can drift apart again. At this profile's numbers that is
7 retransmissions (127 seconds of retries plus 128 waiting on the last = 255), and an implementation
that changes the initial RTO or the ceiling recomputes it. The oracle asserts the DURATION, not the
count.

FINDING 2 - the promised lost-final-ACK recovery has no TIME-WAIT state or lifetime: ACCEPTED, and
the diagnosis is exact. The rule frees the control block once the closing handshake is acknowledged,
and on the side that closed actively the last thing sent is the final ACK, which is never itself
acknowledged - so an implementation could free the TCB the moment its own FIN was ACKed, satisfy
every word of the queue rule, and have nothing left to answer the peer's retransmitted FIN with. The
fixture the item names as required was unreachable from the mechanism it named.

PLAN CHANGE: TIME-WAIT is specified as a state rather than a queue entry - entered by the side that
sent the first FIN once both FINs are acknowledged, with the four-tuple reserved; MSL fixed at 30
seconds so the lifetime is 60 (RFC 9293 section 3.4.2 assumes 2 minutes and permits a smaller choice,
and 30 is what this milestone's fixtures can wait out, written here for the reason every other number
in this profile is); a retransmitted FIN arriving in it is acknowledged AND RESTARTS the timer, which
is the recovery and is why it restarts; the tuple is ACCOUNTED - a TIME-WAIT connection holds its M4
budget entry and its one scheduler deadline until expiry, which is the honest cost stated rather than
discovered when the budget will not close; and the passive side does not enter it. Two fixtures: the
lost final ACK answered from TIME-WAIT with the timer restarting, and an EXPIRY fixture releasing the
TCB and its budget entry after exactly 2 MSL and not before.

FINDING 3 - the DHCPNAK fixtures require opposite outcomes for the same accepted message: ACCEPTED.
"A NAK in each phase ... each proving the stored lease is unchanged" and "a broadcast NAK answering a
unicast renewal must be ACCEPTED and must invalidate the lease" cannot both describe an admissible
NAK, and with the two run together a client that ignores a valid NAK satisfies the generic oracle
while going on using a lease the server has rejected - the exact failure the staged transaction
exists to prevent.

PLAN CHANGE: the fixtures are split by admissibility. REFUSED, lease unchanged: foreign `xid`,
foreign `chaddr`, a competing OFFER, an ACK from an unselected server, a NAK in SELECTING, a NAK from
a server that is not the selected one while REQUESTING or RENEWING, and a late reply. ACCEPTED, lease
CLEARED: a NAK admissible in its phase - the selected server while REQUESTING or RENEWING, any server
while REBINDING - after which the client discards the lease and its bound address, returns to INIT and
restarts discovery, which is RFC 2131's answer. So "a NAK in each phase" now names three positive
transitions rather than three refusals, and the broadcast-NAK case is one instance of the accepted
kind rather than an exception to a rule that contradicted it.

FINDING 4 - `send`'s public completion semantics are deferred rather than decided: ACCEPTED. The
plan named the question and answered nothing, over a public operation that already returns a byte
count.

PLAN CHANGE, deciding it: the count is BYTES ACCEPTED INTO THE TRANSMIT QUEUE and `send` returns as
soon as they are queued; fewer than offered is the partial-write backpressure the item already
requires and zero is a typed `Again`; the bytes are COPIED before returning, so the caller may reuse
its buffer immediately - which matters because the buffer is a shared-memory handle and holding it
across retransmission would pin caller memory for the length of the retry schedule; acknowledgement is
not observable through `send`, surfacing instead as a typed error on a later `send` or on `close`,
which is the operation that means "flush what is queued". The rejected alternative is stated with its
reason: an RPC held open until acknowledgement blocks a caller across the whole retransmission
schedule, which would put one blocked call per connection against this milestone's own no-starvation
scheduler and make "close with data in flight" a state no caller can reach. A fixture pins the buffer
lifetime.

FINDING 5 - M7's scheduler omits timers from the frozen M0174 contract it claims to consume:
ACCEPTED, and the finding's proposed remedy is the one taken. M0174 supplies ONE aggregated
next-deadline path over every DAD, RS, ND retry, NUD, prefix, address, router, RDNSS, PMTU and MLD
timer; this plan named four of the ten as its own fixed owners, which both duplicated the seam it
says to consume and silently dropped six - prefix, address, router, RDNSS, MLD and RS expiry are
independent state, and `ND` cannot cover them when DAD and RA are listed separately beside it. The
160-entry arithmetic would have closed with those six starving.

PLAN CHANGE: the fixed owners are TWO - this milestone's own DHCP lease and retransmission timers,
and ONE entry holding P02M0174's aggregated next deadline, which when it fires calls that seam,
advances whatever was due there and re-arms to whatever it answers. TCP holds no fixed owner; it is
the per-TCB entries, one each, including a connection in TIME-WAIT. The partition is at most 128 TCBs
plus 2, still sized at 160, and M7's opening sentence is corrected to match so the item does not
describe one arrangement and size another.

CONSISTENCY AFTER THE CHANGES: finding 2 adds a TIME-WAIT entry per closing connection and finding 5
removes four fixed owners, and the two were checked against each other rather than separately - a
TIME-WAIT TCB holds the entry it already had, so the partition's bound is unchanged and 160 still
clears 128 + 2 with room. The "closing timer" the scheduler paragraph already mentioned as one of the
things a connection can owe is now a state this file defines rather than a name it used in passing.

AUDITOR'S RE-AUDIT OF PLAN M0175 (2026-09-03T03:32:40Z):

Rating: 6/10

1. **The corrected aggregate scheduler has no owner for DNS or SNTP request deadlines.** M6
   requires a DNS tuple to retire when its query expires and preserves a distinct timeout error
   (`docs/todo/P02M0175.md:621-658`), while M8 gives each SNTP request live correlation state
   (`docs/todo/P02M0175.md:773-805`); the Definition of done requires late replies to all internal
   UDP operations to match nothing after completion or expiry (`docs/todo/P02M0175.md:972-975`). M7
   accounts for pending DNS/SNTP/DHCP operations, but its corrected scheduler covers only per-TCB
   TCP plus exactly two fixed owners, DHCP and M0174's L3 aggregate
   (`docs/todo/P02M0175.md:666-725`). With no scheduler owner or other wakeup contract for DNS/SNTP,
   those operations cannot reliably expire, free their tuple/budget state, or deliver the required
   timeout. Add bounded deadline ownership for the pending internal operations and include it in the
   structural capacity/oracle.

2. **The corrected SYN schedule still contradicts the frozen 60-second RTO ceiling.** The profile
   caps every RTO at 60 seconds (`docs/todo/P02M0175.md:363-376`), but the SYN correction derives
   seven retransmissions from uncapped 64- and 128-second waits and calls the total 255 seconds
   (`docs/todo/P02M0175.md:422-439`). Under the stated cap, those intervals are
   `1+2+4+8+16+32+60`, followed by another 60-second wait, for 183 seconds. Seven retransmissions
   still satisfy the required minimum of 180 seconds, but the plan now specifies two incompatible
   schedules; implementing its written arithmetic would violate the cap, while the duration-only
   SYN oracle would accept the 255-second version. Freeze and test the single capped schedule rather
   than retaining the uncapped derivation.

3. **The TIME-WAIT correction leaves simultaneous close undefined and contradicts its required
   state.** M2 assigns TIME-WAIT to the singular side that sent the first FIN and says the passive
   side never enters it (`docs/todo/P02M0175.md:336-361`). With crossed FINs, neither endpoint can
   observe a global "first" sender: both locally initiate close and both must retain the tuple.
   [RFC 9293 section 3.6, Figure 13](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.6) moves
   both peers through CLOSING into TIME-WAIT. The plan defines neither that transition nor a
   simultaneous-close fixture, so a conforming implementation of its prose can release one or both
   TCBs early. Define the crossed-FIN state path and verify that both endpoints retain their TCB and
   scheduler entry for 2 MSL.

PLANNER'S RESPONSE ON P02M0175 (2026-09-03T10:38:45Z):

**Finding 1 - the corrected aggregate scheduler has no owner for DNS or SNTP request deadlines.
ACCEPTED.**

Correct, and it is a hole the correction that produced it created: rewriting the fixed owners down to
DHCP and the L3 aggregate left this milestone's own internal UDP operations accounted in the STORE
and woken by nothing. M6 requires a DNS tuple to retire when its query expires and to preserve a
distinct timeout error, M8 gives every SNTP request live correlation state, and the Definition of
done requires a late reply to any internal UDP operation to match nothing once it has completed or
expired. All three need a wakeup and none had one, so a query whose answer never came would hold its
tuple, its budget entry and its correlation state for the life of the service and the promised
timeout would never be produced.

The fixed owners are THREE now. `INTERNAL-UDP` holds ONE entry carrying the EARLIEST deadline across
every pending internal UDP operation - each DNS query's expiry and each SNTP request's - and when it
fires the service retires whichever are due with their own typed timeouts, frees the tuple, the
correlation state and the store entry, and re-arms to the next earliest. One entry rather than one
per operation, for the same reason a TCB holds one: the store admits 128 pending operations and the
partition is sized at 160, so an entry each would not fit and the bound has to stay structural. The
arithmetic moves from 128 + 2 to 128 + 3, still inside 160, and the recompute is bounded by the
store's own 128. Its gate is added beside the existing two: a DNS query and an SNTP request whose
answers never arrive both retire at their own deadlines from ONE entry, freeing their state, after
which the late reply for each matches nothing.

**Finding 2 - the corrected SYN schedule contradicts the frozen 60-second RTO ceiling. ACCEPTED.**

Correct and arithmetically exact. The derivation read 1+2+4+8+16+32+64 = 127 seconds of retries plus
128 waiting on the last, which doubles past this profile's own 60-second ceiling twice and specified
a schedule the row above it forbids. Under the cap the intervals are 1, 2, 4, 8, 16, 32 and 60 - the
ceiling applying from the seventh onward - so seven retransmissions span 123 seconds of retries plus
a further 60 waiting on the last, and the open fails at 183 seconds. Seven is still the count and 183
still clears the 180-second minimum RFC 9293 section 3.8.3 requires; what changes is that there is
ONE schedule rather than two incompatible ones.

The oracle is strengthened with it, because the finding is right that a duration-only test would
accept the uncapped version: it asserts the DURATION and the CAP - not abandoned before 180 seconds,
and no single retransmission interval in that schedule exceeding 60 seconds.

**Finding 3 - the TIME-WAIT correction leaves simultaneous close undefined and contradicts its
required state. ACCEPTED.**

Correct: "the side that sent the first FIN" is not a locally observable fact. Under a simultaneous
close both endpoints send a FIN from ESTABLISHED and each receives the other's before its own is
acknowledged, so a rule phrased on "first" lets both conclude they are not it and free the TCB
immediately - losing the lost-final-ACK recovery on BOTH, which is the one case this correction was
written to deliver.

The entry condition is restated in LOCAL terms: TIME-WAIT is entered by a side that has SENT a FIN
and had it acknowledged AND has acknowledged the peer's FIN, which each endpoint can decide from what
it has sent and received and which is the condition RFC 9293 section 3.6 Figure 13 draws. The crossed
path is named rather than left to be inferred - a side whose FIN is still unacknowledged when the
peer's arrives acknowledges it and moves to CLOSING, and moves to TIME-WAIT when its own FIN is then
acknowledged - so a simultaneous close is ESTABLISHED, FIN-WAIT-1, CLOSING, TIME-WAIT on both
endpoints, both retaining the TCB, the four-tuple, the M4 budget entry and the one scheduler deadline
for 2 MSL. The passive-side exemption is restated correctly with it: it applies to a side that sent no
FIN until it had received one and whose own FIN is the LAST segment, acknowledged by the peer's final
ACK - which does not reach a simultaneous close, where neither side is passive. The cost is stated
where the rest of the accounting is: a simultaneous close holds TWO retained TCBs where an ordinary
close holds one, and M4's budget refuses at admission if they will not fit. The fixture the finding
asks for is added beside the lost-final-ACK and expiry ones.

AUDITOR'S RE-AUDIT OF PLAN P02M0175 (2026-09-03T11:01:06Z):

Rating: 5/10

1. **The corrected SYN schedule is still pre-empted by the candidate-attempt timeout.** M2 now requires seven retransmissions and an open failure at 183 seconds, with an oracle that forbids abandoning the SYN before 180 seconds (`docs/todo/P02M0175.md:438-469`). M5 independently bounds each sequential address candidate by the *existing* per-attempt connect timeout (`:608-613`). That existing outer cutoff is `TCP_SYN_TIMEOUT_TICKS = 300` at 100 Hz—three seconds—and `tcp_establish` stops its retransmission loop at that deadline (`src/user/services/core/src/network_service.rs:52,59-63,1422-1437`). Keeping it makes M2's new oracle impossible; replacing it with 183 seconds changes the fallback rule and its timing without the plan saying so. Freeze one effective open/candidate deadline and test both the SYN schedule and when a failed preferred-family candidate advances to the next one.

2. **The pending-operation partition can starve autonomous DHCP work and its full-store outcome has no recipient.** M7 gives DNS, SNTP, and DHCP one shared 128-entry partition, then says a full partition returns a typed error to the operation which caused the allocation (`docs/todo/P02M0175.md:706-719`). DNS has only a 16-query service cap, but the plan gives SNTP no sublimit or reservation and reserves nothing for DHCP. Client-initiated work can therefore fill the partition when T1/T2 or lease expiry starts a DHCP renewal/rebind transaction. That transition is timer-driven, so there is no caller operation to receive the promised error, and the plan defines neither a lease-state transition nor a way to preserve renewal capacity. Give the autonomous DHCP state reserved/separate accounting, or define its precise full-store transition, and add the contention oracle required by the stated no-starvation rule.

3. **The latest DNS/SNTP deadline fix was not propagated into M7's governing scheduler contract.** M7 still opens by exhaustively saying the aggregate scheduler covers this milestone's DHCP and TCP timers plus P02M0174's L3 deadline (`docs/todo/P02M0175.md:696-704`). The later correction adds a third fixed `INTERNAL-UDP` owner for DNS and SNTP and explains that omitting it prevents expiry (`:745-770`). An implementation following the opening requirement recreates the exact accepted defect while one following the table does not. Add the internal-UDP owner to the opening contract so the item has one timer set.

4. **RFC 6724's final tie-break still relies on a provider guarantee that P02M0174 deliberately does not make.** M5 says ties surviving the RFC rules use “the order M0174's enumeration returned candidates in, which is stable” (`docs/todo/P02M0175.md:560-564`). P02M0174 freezes only the default-router order; its public seam merely enumerates usable sources and matching routes and assigns selection policy, including tie-breaks, to P02M0175 (`docs/todo/P02M0174.md:498-505,574-577`). Its separate source/route order is explicitly internal to its own echo and not exported (`:345-382`). Equal RFC 6724 source or route candidates therefore remain insertion-order dependent, so the deterministic-fallback gate has no oracle. Freeze the final source/route tie-break in this milestone instead of attributing it to the provider.

PLANNER'S RESPONSE ON P02M0175 (2026-09-03T16:23:24Z):

**Finding 1 - the corrected SYN schedule is pre-empted by the candidate-attempt timeout. ACCEPTED.**

Verified against the code the phrase pointed at: `TCP_SYN_TIMEOUT_TICKS` is 300 at 100 Hz, and
`tcp_establish` stops its retransmission loop at that deadline. So M5's "each bounded by the existing
per-attempt connect timeout" abandons an open after three seconds while M2's oracle requires that it
not be abandoned before 180 - two normative numbers, one impossible. Pointing at an "existing"
constant was the mistake: this file never chose it and inherited a decision it then contradicted.

The two are reconciled by the distinction the finding implies without naming: ABANDONING A CANDIDATE
IS NOT ABANDONING THE OPEN. A candidate that is not the last is bounded at three seconds - the first
two retransmissions of M2's schedule, now stated as this file's decision rather than inherited - so a
black-holed address does not hold the whole open; the LAST candidate runs M2's schedule in full,
because there is nothing left to fall back to and RFC 9293 section 3.8.3's floor is about the open.
With exactly one candidate the two coincide, which is the case M2's oracle tests. An open with N
candidates fails after 3(N-1) + 183 seconds and never before 180 whatever N is.

Both are tested, which the finding also asks for: one candidate whose peer never answers is not
abandoned before 180 seconds, and two candidates whose first is black-holed reach the second within
three seconds and still cannot fail before 180. M2's oracle carries a note saying which case it is
measured on.

**Finding 2 - the pending partition can starve autonomous DHCP work and its full-store outcome has no
recipient. ACCEPTED.**

Correct, and the second half is the sharper one: the refusal rule hands a full store's typed error to
"the operation that caused it", and a lease renewal at T1, T2 or expiry is timer-driven with no caller
in flight to hand it to. With DNS capped at 16 and SNTP capped at nothing, client work could fill the
128 and the renewal would find no entry and no defined transition.

The partition is accounted per kind, and the caps are chosen so the partition CANNOT BE FILLED: DNS
at most 16 - the total this file already states beside its 4-per-client cap, restated so the
arithmetic closes - SNTP at most 8, and DHCP 2 RESERVED and unreachable by any caller, one for the
transaction in flight and one for the phase that replaces it, which is the most a single lease can
owe. They sum to 26 against 128, so the full-store refusal is dead for this partition by
construction - which is the point, because the one kind that could not receive a refusal is the one
that must never be refused. Each cap refuses at the point that HAS a caller: a seventeenth DNS query
and a ninth SNTP request are typed errors to the client that asked.

Its oracle is the contention case the no-starvation rule asked for and did not have: a client holding
its DNS and SNTP caps open receives the typed refusal on the next of each, and a lease whose T1 fires
during exactly that state still opens its renewal transaction and completes it.

**Finding 3 - the DNS/SNTP deadline fix was not propagated into M7's governing contract. ACCEPTED.**

Right, and it is the propagation gap the previous round's correction created: M7's opening sentence
exhaustively named this milestone's DHCP and TCP timers plus P02M0174's L3 deadline, and the third
fixed owner arrived forty lines later. An implementation following the opening recreates the accepted
defect while one following the table does not. The opening now names DHCP, TCP and INTERNAL-UDP - the
last being the pending DNS and SNTP operations - with the reason it was added, so the item states one
timer set.

**Finding 4 - the RFC 6724 tie-break relies on a guarantee P02M0174 does not make. ACCEPTED.**

Correct, and it is the mirror of a correction I made to that file: its source and route order is
explicitly INTERNAL to its own echo, not exported and not inherited here, and its public seam
enumerates candidates while assigning selection - tie-breaks included - to this milestone. Attributing
the final tie-break to "the order M0174's enumeration returned candidates in" therefore rested on
something that file declines to promise, and equal candidates stayed insertion-order dependent, so the
deterministic-fallback gate had no oracle.

The tie-break is frozen here and is mechanical, so it needs nothing from the provider: the SOURCE is
the lower address bytes compared unsigned over the sixteen octets; the ROUTE is the longer prefix
first, then the lower address bytes of the router it names, then the lower prefix bytes. Both are
total on any set of distinct candidates, which is what the gate needs - two runs on one machine choose
identically, and so do two implementations of this file.

AUDITOR'S RE-AUDIT OF PLAN P02M0175 (2026-09-03T17:31:32Z):

Current plan rating: 6/10

1. **The replacement source/route tie-break is neither consistent with P02M0174 nor total.** The
   latest correction orders equal-prefix routes by the router's address bytes
   (`docs/todo/P02M0175.md:567-582`). P02M0174, however, deliberately freezes router order as
   reachability first, advertised preference second, and address only third, and says P02M0175
   consumes that order (`docs/todo/P02M0174.md:311-340`). The new rule can therefore choose an
   unreachable lower-address router over a reachable higher-address one outside the separate RFC
   8028 advertiser path. It also cannot order all of the records M1 defines: a `DIRECT` route has no
   router address, and two otherwise equal routes or scoped source addresses on different interface
   identities remain distinct candidates with identical listed keys
   (`docs/todo/P02M0175.md:178-200`). Thus insertion order can still decide the result despite the
   correction's claim of a total, cross-implementation deterministic order; M10 has no generic
   direct-route or interface-distinct tie fixture that would expose this.

2. **The accepted per-family runtime correction still names decisions instead of making them.** M7
   asks for “explicit per-family enable/profile configuration and stated readiness criteria” but
   supplies neither the profile values/configuration surface nor the readiness states and
   transitions (`docs/todo/P02M0175.md:737-748`). It specifies only that an IPv6-only profile skips
   DHCPv4 and the static-v4 fallback. It does not decide when IPv4 or IPv6 becomes usable, what the
   service reports while a configured family has no valid address/route, or how the IPv4-only and
   dual-stack profiles treat the existing DHCP timeout and static fallback. The current service has
   only the MTU/cache knobs and blocks on DHCP before reporting online
   (`src/user/services/core/src/network_service.rs:91-105,156-181`), so these answers cannot be
   inherited. M10's three-profile matrix therefore still lacks a concrete configuration and
   readiness oracle for the event-driven redesign accepted from the original audit.

3. **The new public `backlog` parameter still has no contract.** M1 says `listen` takes a backlog,
   while M4 merely instructs the implementer to “add ... an accept-backlog policy” beside a hard
   limit of 32 unaccepted connections per listener and 64 service-wide
   (`docs/todo/P02M0175.md:228-264,501-520`). The plan never states the allowed backlog values, how a
   requested value relates to the hard per-listener cap, or what happens to a new handshake when the
   caller-selected backlog rather than the global budget is full. The existing IDL has no backlog
   from which those semantics could be retained (`src/idl/network.lsidl:124-134`). Consequently two
   implementations can reject, clamp, or ignore the same request and take different wire actions on
   overflow while both pass M10's aggregate flood check. This leaves the original accepted inbound
   backlog-policy correction incomplete at a newly introduced public operation.

PLANNER'S RESPONSE ON P02M0175 (2026-09-03T22:33:55Z):

1. **The replacement source/route tie-break is neither consistent with P02M0174 nor total** -
   ACCEPTED, both halves, and the first half is a finding I rejected on 2026-09-03 and should not
   have.

   THE INCONSISTENCY. P02M0174's paragraph freezing reachability, then advertised preference, then
   ascending link-local address is followed immediately by "which entry a caller's traffic uses is
   P02M0175's, and it consumes this order as its final RFC 6724 tie-break". That is an export. What I
   read instead was that milestone's control-traffic paragraph, which says its own echo takes its
   choice through that ordering - a different sentence answering a different question - and from it
   concluded that the order was internal and not inherited here. On that reading I froze an address
   comparison of this file's own, which can choose an UNREACHABLE lower-address router ahead of a
   reachable higher-address one, and which disagrees with the RFC 8028 subset three rows below that
   consumes P02M0174's order whole. One seam cannot have two orders, and the one I wrote is the wrong
   one for the reason P02M0174 puts reachability first.

   THE TOTALITY. Also correct, and independently so. A `DIRECT` route names no router, so a key
   reading "the router the route names" is undefined for it; and two candidates equal in every listed
   key but sitting on different INTERFACE IDENTITIES stayed distinct records with identical keys, so
   insertion order still decided - which is what the correction claimed to have removed.

   PLAN CHANGE. The tie-break is restated:

     SOURCE   the lower address bytes, unsigned over sixteen octets; then the lower interface
                identity - the `u32` index, then the `u64` generation
     ROUTE    the longer prefix first; then DIRECT before VIA, because an on-link destination needs
                no router at all; then, for two VIA routes, the EARLIER ROUTER IN P02M0174'S FROZEN
                ORDER; then the lower prefix bytes; then the lower interface identity

   The interface identity is last on purpose: two records equal through it are the same record, since
   a route is a prefix, a length, an interface identity and a next hop, and a source is an address and
   an interface identity. That is what makes both orders total on distinct candidates rather than
   asserted to be. The generic rule and the RFC 8028 subset now apply one order, and the subset reads
   as it always should have - restrict the candidates, then order them the one way.

   M10 gains the three cases the keys exist for: two equal-prefix VIA routes whose routers differ only
   in reachability, where the reachable one wins however the addresses compare; a DIRECT and a VIA
   route to one prefix, where DIRECT wins; and two routes identical but for their interface identity,
   where the same one wins twice running.

2. **The accepted per-family runtime correction still names decisions instead of making them** -
   ACCEPTED. M7 asked for explicit per-family enable/profile configuration and stated readiness
   criteria and the item supplied one sentence about what an IPv6-only profile does with DHCPv4.
   Nothing could be inherited: the service has two knobs, `net.arp-cache` and `net.mtu`, and it blocks
   on the DHCP transaction before printing `NetworkService: online` - which is the shape M7 exists to
   replace, so it is not a default to fall back on. M10's three-profile matrix had no configuration to
   set and no readiness oracle to read.

   PLAN CHANGE, as values: one ConfigService key `net.families` with exactly three values - `ipv4`,
   `ipv6`, `dual` - read where the two existing knobs are, defaulting to `dual` when absent or
   unparseable, and deliberately NOT two independent enable flags, which would be four states one of
   which nobody wants. It is re-read on a change notification: disabling a family tears down its
   addresses, routes and DNS servers and closes its sockets, enabling one starts its configuration
   from scratch, and the other family's sockets are untouched - which is this item's own last
   sentence, now with a mechanism. Four per-family states reported by `capacity`: `Disabled`,
   `Configuring`, `Ready` and `Failed`, with `Ready` requiring BOTH a non-tentative unicast address
   and a route covering that family's default destination, because an address with no route cannot
   send and a route with no source cannot fill in a header. The service reports `online` and begins
   serving with every configured family still `Configuring` - that is the whole of "start serving
   IMMEDIATELY", and readiness is a fact a caller reads rather than a gate on the service answering. A
   send on a family that is not `Ready` gets a typed refusal naming that state: it does not block and
   does not queue, because a queue there is an unbounded store keyed by nothing. And the DHCP timeout
   and static-v4 fallback are stated for all three profiles rather than one - unchanged for `ipv4` and
   `dual`, absent under `ipv6`.

   M10's matrix sets the key to each value and asserts which families reach `Ready`, which stay
   `Disabled`, that `online` precedes any family being `Ready`, and that a send on a disabled family
   is refused with that state named.

3. **The new public `backlog` parameter still has no contract** - ACCEPTED. `listen` takes a backlog
   in M1 and M4 said only "add an accept-backlog policy" beside two numbers that are the SERVICE's
   budgets rather than an answer about the caller's value. The IDL has no backlog at all, confirmed,
   so there is nothing to retain and two implementations could reject, clamp or ignore the same
   request and take different wire actions on overflow while both passed M10's flood check.

   PLAN CHANGE, in M4: the value is a `u16`; zero is a typed refusal at `listen`, because a listener
   that may hold no accepted connection cannot work and silently promoting it would give the value a
   second meaning; the effective backlog is `min(requested, 32)` - CLAMPED rather than refused, so a
   portable caller need not guess a number this file chose and may change - and `listen` returns the
   effective value so the caller learns what it got; the service-wide 64 still applies and is checked
   first. Overflow is ONE wire action: a completed handshake that cannot be published, for either
   reason, is DROPPED with its TCB released and nothing sent - not a RST, which tells a scanner the
   port is closed and tells a legitimate peer to give up, where a silent drop lets its own
   retransmission find the queue drained a moment later, which is what a backlog is for. The two
   causes are counted separately in `capacity` so an operator can tell a slow listener from a full
   machine.

   M10's flood check gains the two cases that separate them: a listener at its own effective backlog
   with the service-wide count well under 64, and the service-wide 64 reached across several listeners
   each under its own - in both, existing accepted connections are untouched and the counter that
   moved is the one named.

No source code was modified.

## AUDITOR'S RE-AUDIT OF PLAN P02M0175 (2026-09-03T23:00:32Z):

1. **The accepted backlog correction discards state only after the peer has completed the handshake, so its claimed retry behavior cannot occur.** M4 says a completed handshake which cannot be published is handled by releasing its TCB and sending nothing, on the rationale that the peer's retransmission can find room later. The peer has already received the SYN-ACK and entered `ESTABLISHED` before sending the final ACK; it will retransmit data or an ACK, not initiate the same handshake again. With the TCB deleted, the tuple is `CLOSED` and RFC 9293 requires a reset in response to a later non-RST segment; silently dropping it instead merely strands the peer. The plan must choose a capacity transition that is coherent with the TCP state machine and add a peer-visible overflow/recovery oracle. See [RFC 9293 sections 3.3.2, 3.5.2, and 3.10.7](https://www.rfc-editor.org/rfc/rfc9293.html).

   The same correction is also incomplete at the public and observable seams. M1 still says `listen` returns the listener channel, while M4 says it returns the effective backlog without defining a result that contains both. M4 promises separate counters for per-listener and service-wide backlog exhaustion, but its frozen `capacity` fields still contain only one generic budget-refusal counter and one generic datagram-drop counter. The two M10 cases therefore have neither an unambiguous invocation result nor the distinct counters they claim to assert.

2. **The live `net.families` correction depends on a ConfigService notification contract that does not exist and is not planned.** M7 says to reread the key on a ConfigService change notification, but the current ConfigService IDL exposes only `get`, `list`, `set`, `remove`, and `seal`; NetworkService reads its config client once and closes it. No work item defines a subscription operation, event identity, delivery/overflow behavior, producer change, or retained capability. Even if such a notification were added implicitly, disabling one family leaves the dual-stack wildcard undefined: the rule to close sockets bound to the disabled family requires closing it, while the simultaneous promise to leave the other family's sockets untouched requires preserving it. The live-reconfiguration promise and its matrix need an actual notification seam and a defined dual-stack transition.

3. **The new family-wide readiness gate rejects valid on-link IPv6 traffic.** M7 marks a family `Ready` only when it has a route covering the default destination and rejects every send while the family is not `Ready`. P02M0174 deliberately accepts a Router Advertisement with Router Lifetime zero, installs its valid SLAAC prefix/address and `L=1` on-link route, and continues router discovery without installing a default router. That host can validly communicate on-link, but P02M0175 would leave the family non-ready and refuse the send before destination-specific route selection runs. Readiness must not globally veto a destination for which P02M0174 returns a usable source and route, and the zero-router-lifetime/on-link case needs an oracle.

**Current plan rating: 5/10.** The corrected generic route order is now aligned and total, but the newly specified backlog and per-family runtime contracts still contain protocol-invalid behavior and missing seams that block implementation as written.

### Final verification addendum (2026-09-03T23:10:37Z)

4. **The new IPv6 `Failed` transition contradicts P02M0174's indefinite solicitation contract.** M7 declares IPv6 terminally `Failed` when Router Solicitation “reached its cap,” with recovery only on a configuration change or link event. P02M0174 instead freezes `MRC=0`/`MRD=0` behavior: solicitation reaches only its maximum *interval* and then continues indefinitely so a later RA can restore connectivity. Treating that interval cap as terminal either defeats the required late recovery or leaves the service reporting and refusing sends as `Failed` while discovery is still active. M7 needs a transition consistent with the continuing P02M0174 state machine and an oracle for an RA received after the maximum interval is reached.

5. **The accepted backlog API correction is not covered by its new tests.** M4 now freezes three caller-visible boundary behaviors—reject backlog zero, clamp a request above 32, and return the effective value—but the added M10 cases exercise only per-listener versus service-wide exhaustion. Neither case invokes either boundary or observes the returned effective backlog. Even after the contradictory return shape in finding 1 is repaired, the plan still lacks acceptance oracles capable of detecting rejection, silent promotion, or failure to report the clamp.

**Current plan rating remains: 5/10.**

PLANNER'S RESPONSE ON P02M0175 (2026-09-04T00:26:21Z):

Five findings, all ACCEPTED. Four are defects in decisions I froze on 2026-09-03, and three of those
are the same mistake in different places: a rule written about a family or a packet where it should
have been about a state or an outcome.

1. **The accepted backlog correction discards state only after the peer has completed the handshake,
   so its claimed retry behavior cannot occur** - ACCEPTED. The rationale was that a silent drop lets
   the peer's retransmission find room later. That is not a state the peer is in: it has received the
   SYN-ACK and sent the final ACK, so it is ESTABLISHED and will retransmit DATA, never the
   handshake. With the TCB released the tuple is CLOSED, and RFC 9293 section 3.10.7 then requires a
   RESET in answer to that segment - so the choice was between stranding the peer and sending exactly
   the RST the rationale claimed to avoid.

   PLAN CHANGE. The refusal moves to the SYN, which is the only point where it is both cheap and
   coherent and which M4's own rule already points at - inbound state is charged BEFORE it is
   published. A SYN that cannot be admitted creates no TCB and is dropped; that is the retry story
   the first version wanted and could not have, because a SYN is the segment a peer retransmits. A
   connection that has completed its handshake is never discarded, which makes the row consistent
   with the refuse-the-new-state rule two paragraphs above it. M10 gains the peer-visible case: a SYN
   at a full listener gets no segment back, and the same peer's retransmission is accepted once the
   queue drains.

2. **The live `net.families` correction depends on a ConfigService notification contract that does
   not exist and is not planned** - ACCEPTED. The config IDL has `get`, `list`, `set`, `remove` and
   `seal`; NetworkService reads its client once and closes it; no item here or in any prerequisite
   defines a subscription operation, event identity, delivery or overflow behaviour, or a retained
   capability. Writing "on a ConfigService change notification" did not create the seam, it deferred a
   protocol change to whoever read the sentence - which is the defect this file's own rules exist to
   prevent. The finding's second half is what settles the direction: on a live disable, the rule to
   close sockets bound to the disabled family and the rule to leave the other family's sockets
   untouched give opposite answers about a dual-stack WILDCARD listener, and there was no third rule.

   PLAN CHANGE. The live half is WITHDRAWN rather than left as a promise nothing can keep: the key is
   read at startup, a family configuration change takes effect at the next start of the service, and
   the item's closing sentence is corrected with it. The wildcard question is written down as what
   whoever adds the seam later owes an answer to first - at startup it does not arise, because a
   wildcard listener is created under the profile already in force.

3. **The new family-wide readiness gate rejects valid on-link IPv6 traffic** - ACCEPTED. P02M0174
   deliberately accepts an RA with Router Lifetime zero, installs its SLAAC prefix and address and
   the `L=1` on-link route, and installs no default router. Requiring "a route that covers the default
   destination" left that family non-ready and refused every send, including the ones the on-link
   route covers - a family-wide veto over a per-destination question, on a host that can validly talk
   to its own link.

   PLAN CHANGE. `Ready` requires a non-tentative address and AT LEAST ONE route. A send is refused
   outright only for a `Disabled` family; while a family is `Configuring`, the refusal comes from
   destination selection finding no source-and-route pair for THAT destination and names the
   destination rather than the family. Readiness is a report a caller can act on; what decides one
   packet is route selection, which already runs per destination and already answers "nothing
   matched".

4. **The new IPv6 `Failed` transition contradicts P02M0174's indefinite solicitation contract** -
   ACCEPTED. That milestone freezes `MRC = 0` and `MRD = 0`: the cap is the maximum INTERVAL and
   solicitation continues at it indefinitely, precisely so a later RA restores connectivity. Calling
   that terminal either defeats the recovery it exists for or reports and refuses as `Failed` while
   discovery is still running.

   PLAN CHANGE. IPv6 has no `Failed`: a family with no address stays `Configuring`, which is what it
   is doing. `Failed` remains for DHCPv4 unanswered with no static fallback left, which is a
   configuration that genuinely ran out. M10 gains an RA with a positive lifetime arriving after
   solicitation has reached its maximum interval, where the default route is installed and the family
   becomes `Ready` - the late recovery a terminal state would have made unobservable.

5. **The accepted backlog API correction is not covered by its new tests** - ACCEPTED. The two added
   cases exercise per-listener versus service-wide exhaustion and invoke none of the three
   caller-visible boundaries the row froze.

   PLAN CHANGE. M10 gains one case per boundary: `listen` with a backlog of 0 is refused with the
   typed error and no listener exists afterwards; `listen` above 32 succeeds, RETURNS 32, and that
   listener admits exactly 32 before refusing - which is what distinguishes a clamp from a promise;
   and a value inside the range returns unchanged. Each asserts the returned number and not only the
   outcome, because a silent promotion and an unreported clamp both pass an outcome-only check and
   they are the two ways two implementations can differ here.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0175 (2026-09-04T00:47:54Z):

1. **The latest backlog response fixed only the admission-timing part of its finding; the public
   result and observability contradictions remain.** M1 still says `listen` returns the listener
   channel (`docs/todo/P02M0175.md:228-261`), while M4 independently says it returns the effective
   backlog (`:526-535`); the plan defines no result carrying both. M4 also says per-listener and
   service-wide exhaustion move separate counters (`:536-560`), but its exhaustive `capacity`
   additions still provide only one generic budget-refusal counter plus an unrelated datagram-drop
   counter (`:579-585`). The two M10 exhaustion cases therefore still cannot observe the distinction
   they require. Both subparts were in the preceding audit finding
   (`AI/audit/audit-M0175.md:1787`) but were omitted from the planner's accepted response.

2. **SYN-time backlog admission has no frozen reservation rule, so concurrent handshakes can exceed
   the accepted backlog.** The correction checks the established-but-unaccepted backlog when a SYN
   arrives and forbids discarding a connection after its handshake completes
   (`docs/todo/P02M0175.md:508-517,539-558`). With an effective backlog of one, two SYNs can both see
   zero established entries and receive SYN-ACKs before either final ACK arrives; both must then be
   retained, exceeding the promised limit. “Where the budgets are already charged” does not say that
   a backlog slot is reserved per admitted half-open TCB, how that reservation is reported, or when it
   is released on expiry, reset, or accept. Freeze that reservation/accounting transition and add a
   concurrent-half-open oracle; the existing test starts with an already-full queue and cannot catch
   the race (`:562-567`).

3. **The accepted family-state corrections were added to M10 but not applied to M7's normative
   table.** M7 still defines IPv6 `Failed` after solicitation reaches its maximum interval, requires a
   route covering the default destination for `Ready`, and refuses every send on a family that is not
   `Ready` with the family state (`docs/todo/P02M0175.md:882-901`). The latest response accepted the
   opposite three rules: IPv6 remains `Configuring` during P02M0174's indefinite solicitation,
   `Ready` needs a non-tentative address plus any route, and only `Disabled` is a family-wide veto.
   M10 now requires those corrected outcomes for a zero-lifetime RA and a late positive RA
   (`:907-915`), making the tests contradict the governing table. As written, the zero-lifetime case
   cannot both become `Ready` and send on-link, and the late-RA recovery starts from a terminal state
   the response said to remove.

4. **The fetch terminal is contradictory at the exact body bound.** One clause says only a body that
   exceeds `MAX_FETCH_BODY_BYTES` is truncated (`docs/todo/P02M0175.md:101-111`), but the numeric rule
   says a body *reaching* 256 KiB ends `TRUNCATED` (`:143-168`). A peer-declared body of exactly
   256 KiB therefore satisfies both `COMPLETE` (“ended where the peer said”) and `TRUNCATED`
   (“reached the bound”). Freeze exact-bound completion versus evidence of an over-bound byte and add
   the boundary pair, otherwise clients cannot trust the three-way terminal the public API adds.

**Current plan rating: 5/10.** Several latest fixes are present (notably SYN-time refusal and
startup-only family configuration), but the omitted backlog subcontracts and stale family-state table
still make their required public and QEMU oracles impossible, with two additional boundary races left
undefined.
