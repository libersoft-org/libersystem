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


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0174 (2026-09-10T20:43:15Z):

Scope: `docs/todo/P02M0174.md`, eight items. This entry records what was built, what was NOT built,
and where the line between them is. The milestone is NOT complete and is not ticked.

## What was built, and where

The pure IPv6 layer lives in `src/user/services/logic` - the crate that exists so this kind of logic
can be run by `cargo test` rather than only by a QEMU boot - and the state it is driven against lives
in `src/user/services/core/src/ipv6_host.rs`, which owns the MAC address, the frame buffers, the
clock and the transmit queue and makes no decisions of its own.

    src/user/services/logic/src/ipv6.rs            M1  addresses, prefixes, scope, the identifier
    src/user/services/logic/src/ipv6_packet.rs     M2  Ethernet/IPv6 parsing and egress
    src/user/services/logic/src/ipv6_icmp.rs       M3  ICMPv6, the rate limit, the path-MTU cache
    src/user/services/logic/src/ipv6_nd.rs         M4  the neighbour-discovery codec
    src/user/services/logic/src/ipv6_neighbour.rs  M4  the five-state NUD machine
    src/user/services/logic/src/ipv6_solicit.rs    M4  RFC 7559 router solicitation
    src/user/services/logic/src/ipv6_slaac.rs      M5  lifetimes, prefix validation, DAD
    src/user/services/logic/src/ipv6_router.rs     M5  the default-router list and its total order
    src/user/services/logic/src/ipv6_mld.rs        M5  the multicast listener
    src/user/services/logic/src/ipv6_events.rs     M6  the two event paths
    src/user/services/logic/src/ipv6_budget.rs     M6  the seventeen capacities and the queues
    src/user/services/logic/src/ipv6_timers.rs     M6  the aggregated deadline
    src/user/services/core/src/ipv6_host.rs        the I/O half, driven by all of the above

### M1 - values that carry their scope

`Address`, `Prefix`, `Interface` and `Scoped`. `Scoped::new` REFUSES a link-local or link-scoped
multicast address with no interface, because `fe80::1` on two links is two hosts, and `Interface`
carries a GENERATION as well as an index so a replaced NIC cannot inherit the previous one's
neighbours, addresses or routes.

The interface identifier is derived, NOT taken from the hardware. Modified EUI-64 embeds the MAC in
every address the host forms, which follows the machine between networks; this derives an opaque
identifier from a per-interface secret, the prefix and the interface identity, clears the
universal/local bit and steps past the reserved forms. The test asserts the negative directly: the
result is not the EUI-64 of a plausible MAC and carries no `fffe` marker.

The digest is a PARAMETER rather than a call into `bootproto`. That was forced by the shared-image
build - a provider library may not import a symbol nothing provides - and it is the better seam
anyway: this module states the policy and the caller supplies the primitive.

### M2 - parsing that is strict and bounded

The L3 packet is SLICED to Payload Length before anything walks it, so Ethernet padding is padding
and an empty payload is still a packet. The extension walk is bounded twice, by header count and by
byte count, and each bound has its own refusal. The unknown-option action bits are implemented as
four distinct actions, and the two that report differ ONLY on a multicast destination - which is the
whole reason a blanket multicast rule is wrong and is asserted as its own case. An atomic fragment is
processed as the complete packet it is; every other fragment is refused with a typed result, which is
the named conformance gap made observable. Neighbour discovery behind a fragment header is refused
whether or not the fragment is atomic, which is RFC 6980.

### M3 - ICMPv6 as part of the host

Echo request and reply, the four errors, and the two things that stop one being sent. The structural
rules are checked before the bucket, so a packet that was never going to be answered does not spend a
token. The bucket refills saturating at a twenty-error burst and its rate is read once from
`net.icmpv6-error-rate`, with any value outside 1..1000 giving the default of ten. A fractional
refill is not lost to repeated polling, which a naive implementation loses on every call.

The path-MTU cache lowers only downward, never below 1280, and an EQUAL report does not refresh the
expiry - so a router repeating the same value cannot keep a record alive for ever. A full table
answers `Capacity` rather than claiming the lowering was recorded.

### M4 - all five NUD states, and a solicitation schedule that survives loss

`INCOMPLETE`, `REACHABLE`, `STALE`, `DELAY` and `PROBE`, with their timers, retry counts and the two
retirement paths. A stale entry is USED and moved to `DELAY`, because refusing to send while
revalidating would stall every flow on a link whose neighbours have been quiet. An unsolicited
advertisement never makes an entry reachable, and one carrying a different link-layer address without
the Override flag leaves the entry stale rather than believing either address.

Router solicitation implements RFC 3315 section 14 as RFC 7559 adopts it, in integer thousandths so
there is no floating point and every interval is reproducible. The jitter multiplies `RTprev` and not
twice it; equality with `MRT` does not trigger the replacement; the capped band is `[3240, 3960)`
seconds. The tests inject the plan's own RAND values and assert 3.6/4.0/4.2 s and 19/20/20.5 s.
Only an INSTALLED default route stops the schedule, and the list becoming empty by ANY route restarts
it at the first interval.

### M5 - lifetimes an unauthenticated advertisement cannot abuse

`Infinite` has one representation, selected by the wire value and by nothing else, and the
transitions between it and a finite lifetime are defined in both directions. A Prefix Information
option with `PreferredLifetime > ValidLifetime` is discarded WHOLE rather than clamped. The `A` and
`L` flags are independent decisions, and `A` on a prefix that is not a /64 forms nothing rather than
truncating. The two-hour rule is implemented with its three branches and tested against a forged
one-second lifetime, a genuine renumbering, and a repeated forgery that must not ratchet an address
down.

The default-router list holds eight and orders them by the three frozen keys. The case the dimension
order exists for is asserted directly: a usable low-preference router precedes an unreachable
high-preference one, while a high-preference STALE router precedes a low-preference REACHABLE one -
there is no rank inside the usable class, and a fourth test proves it by showing four differing
usable states come out in address order.

The multicast listener reports before duplicate-address detection, sourced from `::`, and reports
every joined group AGAIN once a real address exists. All-nodes is never reported. A query is
validated on MLD's terms - link-local source, hop limit ONE, Router Alert - and not on neighbour
discovery's, which would discard every legitimate query.

### M6 - the seam

The invalidation queue is bounded, coalesces on identity, and sets a STICKY resync flag on the
thirty-third distinct identity rather than growing or dropping silently. Draining it never clears the
flag; only a snapshot at least as new as the miss, taken at a generation the tables have not moved
past, does. The quoted-error queue drops the NEWEST under a flood, because the oldest errors are the
ones that arrived before it. Seventeen capacities are named in one enum with saturating per-resource
refusal counters, and the pending-resolution queues charge three budgets before a byte is copied.
The timer aggregation covers all eleven kinds, including the two an earlier version of the contract
omitted while calling its list exhaustive, and bounds due work at sixteen actions per pass without
starving the rest.

### The integration

`Ipv6Host` is carried BY the IPv4 `Stack` rather than threaded through the service's nine blocking
helpers. That is not a shortcut: every one of those helpers pumps frames, and M6 requires that a
router advertisement arriving during a DNS wait is processed rather than discarded. Threading a
second argument through nine call chains would have made it easy to forget in one of them; carrying
it makes the ingress path single. The two stacks share the NIC and the frame channel and nothing
else, so an IPv6-less link behaves exactly as it did.

The service loop's wait is now bounded by the MINIMUM of the DHCP lease deadline and the IPv6
layer's next deadline, which is what stops a blocking client request from starving detection,
solicitation, neighbour retries and listener reports.

## What was NOT built

- **M7, the controllable peer fixture.** The harness constructs only QEMU user-mode networking, and
  the hostile RA/ND/ICMPv6 oracles this milestone's gate requires cannot be built from it. Nothing
  was written for this item.
- **M8's QEMU half.** The host tests below cover the state machines and the codecs; the captured-packet
  oracles - the pre-detection report sourced from `::`, the post-detection re-report, the solicitation
  bands with scheduler-tick tolerance, the hostile-RA cases - need M7 and were not run.
- **The transport migration.** IPv6 UDP and TCP are P02M0175's, and this layer therefore has no
  consumer for what it delivers. Deliveries are retained under a bound and REPORTED rather than
  passed on, which is what lets a booted system be asked whether the layer works at all.
- **A persisted interface-identifier secret.** RFC 7217 wants one that survives a reboot; this
  generates it per boot from `random_get`. The privacy property that matters - no hardware identifier
  is exposed, and the address differs per prefix - holds either way, and the limit is written where
  the secret is made.

## The identifier policy, and why it is not RFC 7217's

The first implementation derived the interface identifier the way RFC 7217 does: SHA-256 over a
per-interface secret, the prefix and the interface identity. It works, it is tested, and it cannot
ship in this service, for a reason worth writing down.

`network_service` is `linkage = "dynamic"`, so it takes its code from the shared image's provider
libraries and EVERY cross-crate symbol it references must have exactly one declared provider. There
is no digest in that library set - checked, not assumed: no `.lslib` in the image exports a
`sha256`, and `bootproto` is a boot-chain crate that stages nothing into userspace. Three ways out
were considered:

- add `bootproto` as a shared-image library. That stages a boot-chain crate into the shipping image,
  which is a product decision about the library set and not this milestone's to take;
- compile the digest into the service by including the boot protocol's source file by path. One
  implementation, no new library - and a cross-crate module included by path is the kind of thing
  that is invisible until it surprises somebody;
- state a different local policy, which is what the milestone actually asks for: "derive the stable
  link-local interface identifier by ONE DOCUMENTED LOCAL POLICY; do not accidentally expose an
  unreviewed hardware identifier as a privacy claim."

The third was taken. The identifier is sixty-four bits of KERNEL randomness drawn once per prefix,
with the universal/local bit cleared and the reserved forms refused so the caller draws again. It
gives the two properties the exposure concern is about - no hardware value is an input, and each
prefix gets its own draw - and it gives up the one RFC 7217 adds, which is stability across a reboot.
That limit was already there when the secret was generated per boot, and it is written at the
function rather than left for a reader to infer.

If the owner wants RFC 7217's construction, what it needs is a digest provider in the shared image;
the policy is one function and one test away from being swapped back.

## Verification

    cargo test --manifest-path user/services/logic/Cargo.toml   PASSED  168 test(s), 135 of them IPv6
    ./build.sh --arch x86_64                                    PASSED  every part, shared image included
    ./test.sh --arch x86_64                                     PASSED  387 test(s), 198 s

THE GUEST SUITE IS THE INTEGRATION'S OWN EVIDENCE, not a regression check that happened to pass. The
booted system's log carries

    ipv6: link-local fe80::caa:560:3561:bba8%if0.1, addresses=1, routers=0, mtu=1500, events=1, errors=0, dropped=0

which is the whole bring-up sequence having run on a real link: the solicited-node group joined and
reported before detection, a detection probe sent from the unspecified address, the retransmission
timer armed through the aggregated deadline, no answer, the address assigned, every joined group
reported again from it, and a router solicitation still outstanding because this harness's link has
no router. `routers=0` is the correct answer on QEMU user-mode networking and is what M7's fixture
exists to change.

ONE EXISTING TEST HAD TO CHANGE, and the change is worth stating. `dhcp_lease_renews_at_t1...` read
"the next frame" from the service's frame channel and asserted it was a DHCP message. The link now
carries IPv6 control traffic too, so that read now takes the next NON-IPv6 frame. What the test
asserts is unchanged; what it skips is another protocol's traffic. It failed first and was fixed
after, rather than being adjusted in anticipation - the failure is what proved the IPv6 host was
really transmitting.

Not performed, and named rather than implied: the aarch64 and riscv64 guest suites, and every
captured-packet oracle. The first is a scoping choice; the second needs M7.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0174 (2026-09-10T21:41:38Z):

## M7 - the controllable peer, and what it caught immediately

`src/harness/ipv6-peer.py` is a deterministic IPv6 Ethernet peer on the far end of QEMU's socket
netdev. `qemu-run.sh` attaches the NIC to it when `NET_PEER_PORT` is set, in the one function that
already built the netdev argument, so a peer run and an ordinary run differ in the far end of the
wire and in nothing else.

The peer emits what its scenario names and nothing it was not asked for: router advertisements with
programmable prefix, RDNSS, MTU, lifetime and preference; neighbour advertisements for its own
address only; Packet Too Big; malformed extension chains; and ARP replies so the IPv4 side keeps
working. Everything it sees goes into a CAPTURE, one line per frame naming the properties a test
asserts on - the hop limit, the source, whether a Router Alert option was present - so a shell gate
reads a line instead of parsing packets a second time.

`src/tools/check-ipv6-peer.sh` runs four rows, each a boot against a scripted far end:

    quiet    nothing answers. The system must still report its solicited-node group BEFORE detection
             and from `::`, probe, re-report from the real address once it is its own, and keep
             soliciting. None of this is observable behind user-mode networking, which answers
             solicitations itself.
    router   one advertisement with an autonomous /64, a recursive server and a smaller MTU. The
             system forms an address, runs detection ON IT, installs the route, takes the MTU, and
             STOPS soliciting - which only an installed default route does.
    hostile  an advertisement whose preferred lifetime exceeds its valid one, an autonomous prefix
             that is not a /64, and a router that withdraws itself. Neither prefix may become an
             address, and the withdrawal must restart solicitation.
    flood    sixty-four packets whose extension chain is a lie, while the host is bringing itself up.
             The host must still come up, and must answer at most the twenty-token burst rather than
             amplifying somebody else's flood.

TWO THINGS THE FIXTURE CAUGHT THAT NOTHING ELSE HAD.

The first is a defect in the shipped code: every emitted listener message was going out WITHOUT the
IPv6 Router Alert hop-by-hop option. The envelope is stated in the milestone and the value type
carried it - `ipv6_mld::envelope` returns `router_alert: true` - and the transmit path ignored the
field. A router or snooping switch discards such a message silently, which is the failure the
envelope exists to prevent, and no host test could see it because the option is added where the
frame is built. The capture line `alert=no` is what made it visible. Fixed: one transmit path now
takes the flag and prepends `ROUTER_ALERT_HEADER`, and the RECEIVE side asks the parser rather than
inferring the option from "some extension header was present" - `ipv6_packet::Parsed` gained
`router_alert`, with its own test that a Destination Options header carrying option 5 is not a
hop-by-hop Router Alert.

The second is not a defect but would have made this gate lie: the UEFI firmware brings the NIC up
before the kernel does and runs ITS OWN IPv6, emitting a version-1 listener report and a detection
probe for the modified-EUI-64 address of the NIC's MAC, seconds before this system exists. A gate
asserting on "an MLD report" would have passed against the firmware's while the system sent none. So
every line names the system's own address, which the guest log states and the firmware cannot
produce.

## M8, and what is still owed

The captured-packet oracles this milestone names are now partly real: the pre-detection report from
`::` with hop limit 1 and the Router Alert, the post-detection re-report, the detection probe at hop
limit 255, the solicitation continuing and then stopping on an installed route, the two malformed
prefixes refused, and the withdrawal restarting solicitation. What M8 still owes is the rest of its
list - the solicitation BANDS measured against an injected clock, the RA MTU cases, the two-hour rule
observed on the wire, the MLD timer quartet and the source-cap trio - and the exact-bound pairs it
asks for over each M6 table, several of which the host tests already carry and which have not been
gone through one by one against its text. M8 is NOT ticked.

## Verification of this half

Every command below was run from ONE tree, after the last source edit.

    ./check.sh --gate ipv6-peer              PASSED  4 row(s), each a boot against a scripted peer
    ./check.sh --gate verify-model           PASSED  the new gate is in the catalog and the frozen
                                                     release list, which the model checks both ways
    ./check.sh --gate verify-model-tests     PASSED
    ./check.sh --gate host-tests             PASSED  77 suite(s)
    ./build.sh --arch x86_64                 PASSED  every part, shared image included
    ./image.sh --format iso --dma-mode enforcing-required   PASSED
    ./test.sh --arch x86_64                  PASSED  387 test(s), 198 s
    cargo test --manifest-path user/services/logic/Cargo.toml   PASSED  169 test(s)

`./run.sh` BOOTS THE ISO, NOT THE BUILD TREE, and that cost one confusing run: the first peer row was
judged against an image from hours earlier, whose network service had no IPv6 at all, and the only
frames on the wire were the firmware's. `./image.sh` is what makes a `run.sh`-driven gate see the
code that was just built, and the gate now runs after it.

Not performed: the aarch64 and riscv64 guest suites, and M8's remaining oracles.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0174 (2026-09-10T22:15:34Z):

## M8 - the fixtures, and the four defects writing them found

M8's list was gone through case by case rather than declared covered. Where a case had no oracle, one
was written; where the code could not answer it, the code changed. `service-logic` now carries 190
tests, 157 of them IPv6.

### What the cases forced into the code

FOUR THINGS WERE MISSING, and each was found by writing the case rather than by reading the code.

- **The listener's query-merge rules did not exist.** `on_query` kept an earlier deadline and unioned
  sources, and that is one of the five rules. The record now carries a `Response` that is either
  address-specific or a source list, and merging obeys all of them: an address-specific query CLEARS
  a pending source list because the broader report is now owed; a source-specific query arriving
  while an address-specific response is pending leaves the list EMPTY and merges only the timer -
  the reverse ordering, and the one where a union into an empty list silently narrows an answer that
  was already owed; two source lists union up to the cap; past the cap the record degrades to
  address-specific, clears the list and keeps the EARLIEST deadline already chosen. Degraded rather
  than dropped, because a querier that floods must not be able to suppress a report.
- **A leave while a join report was pending could transmit out of order.** The two obligations shared
  one deadline field, so which went first depended on which timer happened to fire. A router that saw
  the Done before the Report would believe this host is still a member. The record now holds the
  query response and the state-change retransmission SEPARATELY, and a leave supersedes a pending
  report and resets its counter.
- **The advertised-MTU rule was inline in the service and untestable.** It is now
  `ipv6_packet::accept_link_mtu`, with its own cases: lowering is the only direction, equal is not a
  lowering, above the link's own ceiling is refused, and BELOW 1280 is ignored rather than taken -
  because an option naming less than the minimum does not describe a link this host can run IPv6 on,
  and obeying it would leave an interface that cannot send a legal packet.
- **The recursive-DNS bound was declared and unenforced.** The capacity table names four records
  keyed on the advertising router AND the server, and the host only emitted an event per server. It
  now keeps `RdnssSet`: the pair is the identity, so one router's withdrawal cannot take away a
  server another still offers; expiry reports only the servers no longer offered AT ALL; and the
  export is unique, because a consumer wants a resolver list rather than a record list.

### And one behaviour the per-family clause required

A link whose EFFECTIVE MTU is below 1280 now leaves IPv6 REFUSED. The constructor used to raise the
number it was given to the minimum, which is the one thing it must not do: the frame buffers are the
size the interface reported, and a host that raised the number would write frames the link will not
take. `bring_up` returns false, the service says so once, and IPv4 is untouched.

### The cases now covered

    the five NUD states, their timers, retries and both retirement paths        ipv6_neighbour
    a full ND table refusing a router, and a retired entry releasing its slot    ipv6_neighbour
    the router order's three keys, and the same winner under Delay, Probe,
      reverse insertion order and swapped addresses                              ipv6_router
    routers expiring independently, and a released slot admitting a new one      ipv6_router
    the four MLD timer cases, the reverse ordering, and the source-cap trio
      including the hostile disjoint-list flood                                  ipv6_mld
    a lost report recovered by the last transmission at the default robustness   ipv6_mld
    a join/leave merge that cannot transmit out of order                         ipv6_mld
    the exact-bound pair on every queue: 8 routers, 64 neighbours, 64 PMTU keys,
      32 invalidations, 32 advisory errors, 4/32/65536 pending, 4 RDNSS          each module
    a dropped advisory error never asking for a resync                           ipv6_events
    the ICMP flood bound as a formula: at most 20 + floor(rate * T)              ipv6_icmp
    the two-hour rule, the prefix refusals, DAD conflict, lifetimes              ipv6_slaac
    payload-length slicing, the chain bounds, atomic fragments, RFC 6980,
      the unknown-option action bits, jumbogram refusal, Router Alert            ipv6_packet

### What M8 still owes

- The solicitation BANDS measured in a booted guest against an injected clock. The formulas are
  tested exactly, with the plan's own RAND values; what is not done is observing the intervals on the
  wire with scheduler-tick tolerance.
- Echo traffic and an observed Packet Too Big in a guest. The peer can inject one; the guest has no
  IPv6 transport to provoke it, which is P02M0175's.
- The sub-1280 link as a BOOTED fixture. The refusal is implemented and the service reports it; the
  harness has no knob to configure an effective link MTU below 1280, so nothing exercises it in a
  guest.
- The two-hour rule and the RA MTU cases observed on the wire rather than in a host test.

M8 is therefore NOT ticked, and the milestone stays open on it.

## Verification

Every command below was run from ONE tree, after the last source edit.

    cargo test --manifest-path user/services/logic/Cargo.toml   PASSED  190 test(s), 157 of them IPv6
    ./build.sh --arch x86_64                                    PASSED  every part, shared image included
    ./image.sh --format iso --dma-mode enforcing-required       PASSED
    ./check.sh --gate ipv6-peer                                 PASSED  4 row(s) against a scripted peer
    ./test.sh --arch x86_64                                     PASSED  387 test(s), 198 s

Not performed: the aarch64 and riscv64 guest suites, and the four M8 items listed above as still
owed.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0174 (2026-09-11T02:58:00Z):

M8 - PACKET FIXTURES AND QEMU PROVE BOTH POSITIVE AND HOSTILE PATHS.

WHAT WAS ALREADY THERE WHEN M8 STARTED. M1 to M7 had left 157 host tests across the twelve IPv6
modules of `service-logic`, and most of M8's named list was among them: address classification,
checksums, extension walking, payload-length slicing with Ethernet padding, atomic-fragment
acceptance and non-atomic refusal, the ICMP error rate limit, DAD conflict, all five NUD states and
their timers, MLDv2 join/leave, the pre-DAD report from `::` and the post-DAD re-report, all three
query kinds, a second query while a response is pending, the lost-first-report recovery at the
default robustness of two, `PreferredLifetime > ValidLifetime`, the two-hour rule, independent router
expiry, RDNSS withdrawal and expiry, jumbogram refusal, the router-ordering fixtures with their three
frozen keys, the source cap and its three cases, the five MLD timer cases, the per-table refusals and
the pending-packet budgets. This section records what was MISSING, which is what M8 came down to.

FOUR GAPS, AND THEY WERE REAL.

1. THE QUOTED-ERROR SEAM HAD NO FIXTURES AND NO SHARED IMPLEMENTATION. `transport_of` lived in
   `ipv6_host.rs`, in the `services` binary crate, where a host test cannot reach it - so the rule
   M6 froze (eight quoted TCP bytes, the complete eight-byte Echo header AND its Request type/code, a
   short quote dropped and counted) had no executable oracle, and two parts of it were not
   implemented. A quoted ICMPv6 message of ANY type had its bytes at offsets 4..8 read as an echo
   identifier and sequence, so a quoted listener report would have been handed to a probe as an echo
   identity; and a seven-byte TCP quotation became `Other { next_header: 6 }` rather than being
   dropped and counted.

   `src/user/services/logic/src/ipv6_quote.rs` is new and holds the whole seam: `QuoteRefusal`,
   `transport`, `validate` and `quotation_is_in_flight`. `ipv6_host::on_icmp_error` is now four lines
   that call `validate` and count what it refuses; its private copy is gone.
   `QuotedErrorQueue::note_dropped` counts an unattributable error against the SAME counter as an
   overflow drop - both mean "an error arrived and no consumer will hear about it", and a counter
   split by cause is a counter whose keys a flood chooses.

   `quotation_is_in_flight` is the RFC 5927 section 4.1 bound M6 froze and M0175 will implement
   against: `quoted_seq.wrapping_sub(snd_una) < snd_nxt.wrapping_sub(snd_una)`, with the empty flight
   falling out of the same expression rather than being a special case.

   Fourteen tests in `ipv6_quote/tests.rs` cover: the seven/eight-byte TCP exact-bound pair including
   a zero sequence; a short UDP quote; the Echo header's type and code, with a quoted Echo REPLY and
   a quoted listener report both refused an echo identity; an unknown protocol that is an event at
   any length; a message too short to hold a quoted header, a type that is not an error, and a quoted
   header that is not IPv6; a quotation naming an address this interface does not hold; the responder
   named separately from the quoted destination; the cross-flow negative (an error quoting flow A
   delivered against A and leaving B unterminated and unresized); the FORGED Packet Too Big with a
   well-formed local tuple matching no live flow, which writes nothing, followed by the same message
   against the flow it does name, which writes; the send bounds (below `SND.UNA`, at `SND.NXT`, at
   `SND.UNA`, an empty flight, and a WRAPPED interval); the expiry-replay case (acknowledge the
   quoted data, advance past the 600-second expiry, replay - neither the cache entry nor the
   consumer-local limit moves, and a fresh outstanding quotation moves both); two responders
   attributed by quoted sequence with the errors arriving in reverse order; a retired earlier probe
   whose error reaches nobody and leaves the later probe untouched; and a truncated Echo quote plus an
   interface-generation mismatch.

   The consumers in those fixtures are deliberately tiny stand-ins written in the test file - a
   `Flow` with a tuple, a send interval and a transmit limit, and a `Probe` with an identifier and a
   sequence. That is what the seam has to be usable by, and it keeps M0174's gate independent of
   M0175's TCP.

2. TWO OF M6'S DECLARED CAPACITIES HAD NO TABLE BEHIND THEM. `Resource::Prefixes` (15),
   `Resource::Routes` (32) and `Resource::AdvertisersPerPrefix` (8) were in the budget with their
   limits, and nothing implemented them: `on_prefix` emitted a `Prefix` invalidation event and
   dropped the fact on the floor, there was no route table at all, and `snapshot()` reported `used =
   0` against all three real limits - a false report to the consumer the snapshot exists for. M8
   requires these bounds driven "independently through the pure table API"; there was no API.

   `src/user/services/logic/src/ipv6_route.rs` is new: `PrefixTable` (unique by prefix and length,
   with a bounded advertiser set per prefix) and `RouteTable` (keyed by destination AND next hop,
   with longest-prefix lookup). The advertiser set is what makes a withdrawal a membership change:
   a prefix two routers advertise survives one of them leaving, which a host that dropped the prefix
   would not - it would stop treating its own link as its own link.

   `ipv6_host` now holds both. `bring_up` installs `fe80::/10`; an accepted Prefix Information option
   records the prefix and its on-link route together or not at all; an admitted default router
   installs `::/0` through itself with the router lifetime; `retire_router` takes that router's
   routes and its prefix memberships and nothing else; the prefix-lifetime timer sweeps both tables.
   `snapshot()` reports all three resources truthfully, `AdvertisersPerPrefix` as the fullest set
   currently held - the one that will refuse first.

   That table then fixed a real defect in the send path: `send_unicast_icmp` resolved the PEER at
   layer two, which is only correct while the peer is on-link. An off-link destination would have
   been solicited directly, nothing on the link would have answered, and the packet would have sat in
   the pending queue until it gave up - a failure looking exactly like an unreachable neighbour
   rather than a missing route. It now resolves `next_hop_for`, which is the route's next hop, and
   charges the pending queue against that neighbour.

   Ten tests in `ipv6_route/tests.rs`: uniqueness and update; the two-advertiser survival case; a
   router going away taking only its own memberships; fifteen prefixes and a refused sixteenth with a
   refresh at capacity and a slot reclaimed by expiry; eight advertisers and a refused ninth with a
   membership reclaimed; expiry and on-link status; two routers as two routes; longest-prefix
   matching with link-local, on-link and default all present; thirty-two routes and a refused
   thirty-third; and interface-generation scoping.

3. FOUR TABLES HAD THE REFUSAL HALF OF THEIR EXACT-BOUND PAIR AND NOT THE RECLAIM HALF. M8 asks for
   "refresh and remove existing identities at capacity, reclaim one eligible slot, and prove a new
   entry can then enter"; only the router list and the RDNSS set had it. The address set, the
   neighbour cache, the path-MTU cache and the MLD listener gained it, each in its existing capacity
   test rather than a new one, because the reclaim is only meaningful against a table that is
   genuinely full. The MLD case is the interesting one: a slot comes back only when the LEAVE has
   finished transmitting, not when membership ends, because the record is what carries the remaining
   Done messages.

4. THE SUB-1280 LINK HAD NO FIXTURE AT EITHER LEVEL. `bring_up` compared `self.mtu` with `MIN_MTU`
   inline, so the rule the Definition of done's per-family clause rests on could not be tested, and
   no gate row ever booted such a link. `ipv6_packet` gained `effective_link_mtu` (the smaller of the
   configured knob and the device's report) and `link_carries_ipv6`; `bring_up` and the service's
   start-up both call them, and the `net.mtu` knob is now bounded above by `u16::MAX` so every later
   conversion of that number is total.

VERIFICATION PERFORMED.

`cargo test --quiet --manifest-path src/user/services/logic/Cargo.toml`: 215 passed, 0 failed. 182 of
those are the IPv6 modules' (ipv6 10, ipv6_budget 12, ipv6_events 11, ipv6_icmp 16, ipv6_mld 18,
ipv6_nd 11, ipv6_neighbour 14, ipv6_packet 21, ipv6_quote 14, ipv6_route 10, ipv6_router 13,
ipv6_slaac 16, ipv6_solicit 8, ipv6_timers 8), against 157 before this item.

`./build.sh --arch x86_64`: clean, with `-D warnings` in force.

`./check.sh --gate source-hygiene`: clean.

`./check.sh --gate host-tests`: clean.

`./test.sh --arch x86_64`: 387 passed, 199s. The DHCP lease tests still read the frames they expect
because `next_ipv4!` skips this host's IPv6 emissions, and the IPv4 configuration line's new `mtu=`
suffix breaks nothing: every other reader of that line matches a prefix ending at `DHCP`.

`./check.sh --gate ipv6-peer` after `./image.sh --format iso --dma-mode enforcing-required`:
all six rows passed. quiet (the pre-detection report from `::`, the
detection probe, the re-report, the RFC 7559 bands measured on the wire at 3.58s, 6.99s, 14.35s);
router (the address formed and proven, `prefixes=1, routes=3`, the MTU taken, solicitation stopped,
and the same boot's IPv4 address and answered ping); hostile (both malformed prefixes refused, no
address formed, the withdrawal restarting solicitation); flood (the host came up under 64 malformed
packets and answered at most the burst); echo (the reply sourced from the address that was asked, the
Packet Too Big taken, and `errors-seen=3, errors-dropped=1`); narrow link (`ipv6: refused on this
link`, no address, nothing of this host's on the wire, IPv4 answering, and `mtu=1279` on the IPv4
configuration line).

THE GATE'S SIXTH ROW IS NEW, and it is the one the Definition of done's per-family clause had no
oracle for: `NET_LINK_MTU=1279` makes the virtio device report one byte below what IPv6 requires -
the largest link that still cannot carry it, which is the number an implementation comparing with `>`
instead of `>=` gets wrong. The guest must say `ipv6: refused on this link`, configure no address,
put nothing of its own on the wire (the firmware's own IPv6 is still there, which is why the check
names the system's emissions rather than counting frames), and keep IPv4 working - its address, its
ARP, and an actual ping answered. The frame buffers are asserted to be the size the LINK reported and
not the size a refused family might have left, which is why the IPv4 configuration line now carries
`mtu=`.

THE ECHO ROW GAINED THE QUOTED-ERROR CASES. After the Packet Too Big, the peer sends two errors from
two DIFFERENT hops about successive Echo Requests to the same destination, in REVERSE order - the
second hop's complaint about sequence 2 before the first hop's about sequence 1 - and then a third
that is addressed to this host and quotes an address this interface does not hold. The guest must
count exactly three validated and one refused. The destination and the quoted source are separate
arguments in the peer for that last case: an error that conflated them would be refused a step
earlier, for not being addressed to this host at all, and would prove nothing about the quotation.

AND THE ROUTER ROW NOW ASSERTS THE OTHER FAMILY AND THE TABLES. `prefixes=1, routes=3` - link-local,
the advertised prefix, and the default route through the router that advertised it - and the same
boot's IPv4 address and ping path, because a configured IPv6 host that had quietly taken the
interface away from IPv4 would pass every other assertion in that row.

ONE DEFECT THE GATE FOUND IN ITS OWN EVIDENCE. The IPv6 report was assembled from twenty separate
`print` calls, and every other process on the system writes to the same console; a report with
`userspace: TimeService: online` spliced through the middle of it is not evidence. It is built into
one string and written once.

WHAT WAS NOT DONE, AND WHY. Nothing in M8's list is outstanding. The one thing deliberately left to
its consumer is the ATTRIBUTION of a quoted error to a live flow: this layer validates and delivers,
and the host-test stand-ins prove the seam carries what an attributing consumer needs, but the real
consumer is P02M0175's TCP and its own send state. That split is M6's frozen decision, not a shortcut
taken here - a layer that attributed would need the flow registry M6 forbids it to keep.
