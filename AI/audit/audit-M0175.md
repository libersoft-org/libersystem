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

