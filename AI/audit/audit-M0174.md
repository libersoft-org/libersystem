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
