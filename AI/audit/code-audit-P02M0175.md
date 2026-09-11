IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-10T09:52:00Z):

Scope: `docs/todo/P02M0175.md` - dual-stack transports, DNS and NetworkService. Status line:
"PLANNED AFTER P02M0174. FUTURE-TRACK NETWORKING. NOT A PHASE-2 COMPLETION GATE."

Assessment, reported rather than half-implemented:

- It DEPENDS ON P02M0174, which is not implemented (see `code-audit-P02M0174.md`, flagged as a large
  self-contained milestone needing its own run): M5's source/destination selection is "OVER
  M0174's CANDIDATES", and M6/M7/M9 carry IPv6 addresses that M0174 defines. So this milestone
  cannot begin before M0174 lands.

- It is itself larger than M0174: ten items over ~1560 lines, and its M2 is a from-scratch
  TRANSMIT-SIDE TCP - the plan records that there is NO transmit-side TCP today (`TcpConn` stores no
  unacknowledged bytes, no peer MSS, no send window; `socket.send` truncates to one segment,
  transmits once and reports success), so resegment-and-retransmit after a PTB "cannot be
  implemented as a family addition" and is new implementation. Plus IPv6 pseudo-header checksums
  across UDP and TCP (M3), inbound TCP state charged before publication with hard budgets (M4),
  correlated and validated DNS (M6), an event-driven per-family service (M7), correlation and
  validation of every internal UDP operation (M8), the connect/fetch/listen/accept client migration
  (M9), and a dual-stack test matrix (M10).

Decision: nothing implemented, on two grounds - its dependency (M0174) is not built, and it is a
multi-thousand-line effort including a new TCP transmit engine that cannot be delivered as a partial
or a stub without violating the job's no-placeholder rule. It needs its own focused implementation
run after M0174, and is flagged here for that. Scaling it down or scheduling it is the project
owner's call.

Verification: not applicable - no change was made. Commands run: read of `docs/todo/P02M0175.md` and
the transmit-side TCP shape in the network service.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-10T20:43:15Z):


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-10T20:43:20Z):

Scope: `docs/todo/P02M0175.md`, ten items. Nothing was implemented. This entry says why, and what
the next run needs.

## The dependency is not met

M0175's first sentence is that it consumes P02M0174's L3 layer, and its own items are written
against contracts that milestone freezes: the invalidation queue with its resync guarantee, the
typed quoted-error delivery, the router order exported whole, and the pending-resolution completion
that reports `Sent(token, time)`.

Those contracts now EXIST and are host-tested - `service-logic`'s `ipv6_events`, `ipv6_budget` and
`ipv6_router` are exactly them - so the seam this milestone reads is no longer missing. What is
still missing is the half of M0174 that proves the layer works against a real peer: its M7 fixture
and M8's captured-packet oracles were not built, so a transport written on top of it would be
written against a layer whose behaviour on a wire has not been observed.

## And the work itself is not a small addition

Its own items name a new TCP TRANSMIT ENGINE (M2), inbound state charged before publication with
hard budgets (M4), IPv6 pseudo-header checksums across UDP and TCP (M3), source and destination
selection following RFC 6724 over M0174's candidates (M5), correlated and validated DNS (M6), an
event-driven per-family service (M7), correlation of every internal UDP operation (M8), the
connect/fetch/listen/accept client migration (M9) and a dual-stack test matrix (M10). The transmit
engine alone is new implementation rather than a family addition, and the plan says so.

Delivering part of it - typed addresses with no transport, or a transmit engine with no selection -
is the partial layer this job's own rules forbid, and it is what the previous entry for this
milestone recorded too.

## What a dedicated run would do first

Finish M0174: the controllable peer fixture, then the captured-packet oracles for the pre-detection
report, the post-detection re-report, the solicitation bands and the hostile-RA cases. With a layer
whose wire behaviour is observed, M0175's order is its own: M1 (the IDL representation), M6 (DNS) and
M5 (selection) read from M0174 and can be written against the host tests; M2 and M4 are the new
engine and its budgets; M9 and M10 close it.

Verification: not applicable - no change was made for this milestone.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T03:20:00Z):

M1 - THE IDL REPRESENTS EITHER ADDRESS WITHOUT AMBIGUITY, WITH STATED FIELDS AND BOUNDS.

WHAT THE CONTRACT LOOKED LIKE BEFORE. `src/idl/network.lsidl` had one address type - four octets -
and every operation was written in it: `resolve` answered ONE `ipv4-addr`, `connect` took an
`endpoint`, `listen` took a bare `u16`, `net-info` carried a single address, a single gateway and a
neighbour list, and `sock-info` carried a local PORT with no local address at all. None of those
shapes can express what P02M0174 already holds, and several cannot express it even in principle: a
record with one `gateway` field cannot say "two routers", and a socket row with one port cannot say
which of this host's addresses a connection runs from.

THE NEW SCHEMA, and the decisions behind the parts a reader cannot derive:

  `ipv6-addr` is SIXTEEN POSITIONAL FIELDS, not `list<u8>` with `@bound(16)`. A bound says "at most",
  so a bounded list admits a twelve-octet value that is not an address, and every consumer would then
  have to check a length the wire should never have been able to express. `mac-addr` is the same
  decision for the same reason, and it removes a variable-length occurrence from `net-info` and
  `neighbor` entirely.

  `ip-address` is a CLOSED VARIANT. Two optional fields could say "both" and "neither", and two
  consumers would decide differently what those mean.

  `interface-id` carries an index AND a generation, which is the whole reason it is not just an
  index: a link-local value scoped before a NIC was replaced would otherwise silently name something
  else afterwards.

  `next-hop` is TWO FORMS. P02M0174 installs an on-link route for every PIO with `L=1`, and that
  route has no next hop - the destination IS the next hop. A mandatory address field would have
  forced a fabricated address or an all-zeros sentinel into a public contract.

  The bind matrix is frozen in the IDL's own doc comment and validated in one function,
  `validate_bind`: `ipv4-only` with an IPv4 address, `ipv6-only` with an IPv6 one, and `dual-stack`
  with the unspecified IPv6 address AND NOTHING ELSE. The wildcard is the unspecified address rather
  than an absent one, so "is this a wildcard" is one comparison instead of a convention. An
  IPv4-mapped IPv6 address is refused as a listen address in every mode.

  `open-target` is a nonempty list of at most eight scoped destinations plus one port and an optional
  caller-chosen source. `listen` returns `listen-result` - the listener handle AND the effective
  backlog - and `accept` returns `accept-result`, the socket plus both endpoints.

  `fetch` became `result<stream<fetch-chunk>, error>`: the open is guarded, so a refusal before any
  body exists is the typed error every other operation uses, and the final chunk carries one of
  `complete`, `truncated` or `failed`.

THE NUMBERS ARE IN CODE, NOT ONLY IN THE PLAN. `src/user/libs/protocol/network-proto/src/limits.rs`
is new and holds every bound an `@bound` annotation cannot express: the framing capacities, the DNS
parser's work limits, the per-client and per-flow budgets, and the combined dual-stack list bounds
with the arithmetic that produced them. `network_service` now takes `REQ_MAX` and `REPLY_MAX` from
it rather than declaring its own, so the wire cannot advertise a request the service cannot receive.

  request 8192, reply 65536. Both are derived rather than chosen: an eight-destination open-target
  beside 1024 request bytes does not fit 1024, and a `net-info` whose neighbour list is full is
  28 kB - the old 4096-byte reply buffer could not have carried it. Six fixtures in
  `limits/tests.rs` build the widest legal value of each shape, encode it, and assert both that it
  FITS the new buffer and that it does NOT fit the old one, so the numbers are measured rather than
  asserted.

WHAT THE SERVICE DOES WITH THE NEW SHAPES. `info` now builds ONE combined dual-stack snapshot from
the IPv4 stack and P02M0174's IPv6 tables together - addresses with their state and remaining
lifetimes, routes with their next-hop form, routers with the NUD state the neighbour cache holds (not
the two-valued class the router list keeps, which would have hidden exactly the distinction a reader
is looking for), resolvers, and the neighbour cache. `resolve` answers a list of one until M6's AAAA
query fills it. `ping`, `probe` and `sntp` take a scoped address and refuse a v6 one with
`unsupported` - the request is understood and this implementation does not serve it yet - while a
SCOPE on an IPv4 address is `invalid`, because there is one interface and ignoring the scope would
let a caller believe it selected something. `connect` and `fetch` validate the whole open-target
before admitting anything, collapse duplicates, and refuse a caller-chosen source with `unsupported`
rather than silently selecting another, which is the failure that field exists to prevent.

ONE DEFECT THE NEW CONTRACT EXPOSED IN THE SEND PATH, fixed here because it is not optional: with the
route table present, `send_unicast_icmp` was resolving the PEER at layer two, which is correct only
while the peer is on-link. An off-link destination would have been solicited directly, nothing would
have answered, and the packet would have sat in the pending queue until it gave up - a failure
looking exactly like an unreachable neighbour rather than a missing route.

ALL FIRST-PARTY CALLERS MIGRATED ATOMICALLY, as the plan requires rather than keeping two APIs:
`network-client`, `network-proto`'s helpers, `network_service`, `time_service`, and the tools `ip`,
`arp`, `ss`, `ping`, `traceroute`, `nslookup`, `nc`, `tcp` and `httpd`. `ip` renders a section per
table instead of one line; `ss` brackets IPv6 endpoints, because `fe80::1:80` cannot be read;
`nslookup` prints every address a name has instead of the one the old contract could carry; `nc` and
`tcp` hand the whole candidate list to `connect` and let the service own the order. The ABI manifest
was regenerated once with `--accept-breaking`, which is what the plan permits for a pre-release
schema.

THE ADDRESS TEXT HAS ONE FORM AND ONE PARSER. `addr.rs` gained IPv6 parsing with `::` compression and
a trailing dotted quad, and RFC 5952 canonical rendering - lowercase, no leading zeros, `::` over the
longest run and never over a run of one, leftmost on a tie. Eight tests hold the two directions to
agreeing with each other, including the property that matters more than any single spelling: every
rendering this host produces parses back to the value that produced it.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/libs/protocol/network-proto/Cargo.toml`: 48 passed. `src/proto`:
53 passed.

`./build.sh --arch x86_64`: clean, with `-D warnings` in force across every migrated crate.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean; the model
reports 131 crates, 271 components, 1153 edges.

`./test.sh --arch x86_64`: 387 passed, 199s.

`./check.sh --gate ipv6-peer` after a fresh image: all six rows passed unchanged. The
IPv6 layer beneath this contract is untouched by the migration, and the row that matters most for it
is the narrow link: the guest still refuses IPv6 at 1279 bytes while its IPv4 address, ARP and ping
all work - which is the case a dual-stack contract could most easily have broken.

ONE HARNESS DEFECT THIS FOUND, AND IT IS THE INTERESTING ONE. The permission-manager scenario in
`src/kernel/tests.rs` stands in for NetworkService so a governed `ip` has something to query, and it
was answering with a HAND-WRITTEN copy of the old `net-info` bytes. Nothing checked that copy against
the schema, so the moment the record grew the governed `ip` received a reply it could not decode and
printed "service unavailable" - and the failure read as a service fault rather than as a harness that
had drifted. It now builds the reply with the generated encoder, which is what the device-catalogue
fixture beside it already did and for exactly this reason. `src/kernel` gained `network-proto` as a
dependency to make that possible.

WHAT M1 DELIBERATELY LEAVES TO THE MILESTONES THAT OWN IT. The IPv6 transports themselves are M3's:
`listen` in `ipv6-only` or `dual-stack` mode, and a v6 destination for `connect`, `fetch`, `ping`,
`probe` and `sntp`, are typed `unsupported` refusals rather than silent IPv4 substitutions. The AAAA
query that fills `resolve`'s list is M6's, the caller-chosen source is M5's, and the budgets and
parser limits named in `limits.rs` are enforced by M4 and M6. Every one of them now has a shape on
the wire to be implemented into, which is what M1 is for. `httpd` asks for `ipv4-only` on `0.0.0.0`
rather than the dual-stack wildcard, because that is what this service can bind today and asking for
the other would make the program fail to start rather than serve what there is.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T04:05:00Z):

M2 - TCP ACQUIRES A BOUNDED TRANSMIT SIDE.

WHAT THE SENDER WAS BEFORE THIS. `tcp_build_data` took the caller's bytes, built ONE segment from
them, sent it once, advanced the sequence number and forgot them. There was no queue, no
retransmission timer, no congestion window, no persist timer and no TIME-WAIT. Three consequences,
each of them a defect rather than a missing refinement:

  - A `send` larger than one segment was TRUNCATED and the whole length reported as sent. That is
    data loss wearing a success, and it is the first thing this item names.
  - A lost segment was lost. Nothing retransmitted it, because nothing owned it.
  - A FIN was built, sent once, and the control block freed after a brief pump. A lost FIN or a lost
    final acknowledgement left the connection half-open on the peer with nothing left to answer from.

FOUR PURE MODULES IN `service-logic`, BECAUSE A SENDER IS WHERE NUMBERS LIVE AND NUMBERS NEED
FIXTURES. Every one of them is driven by host tests rather than by a guest boot:

  `tcp_rto.rs` - RFC 6298's estimator in the scaled integer form: the smoothed round-trip time by
    eighths, the variance by quarters, and a timeout four variances above the mean. Karn's rule is a
    PARAMETER of `sample` rather than a note for the caller, because it is the one line somebody
    forgets. THE 200 ms FLOOR IS A DECLARED DEVIATION from rule 2.4's one second, written where the
    floor is: this is a bounded appliance on an emulated link with a millisecond clock, and the RFC's
    own note for that rule anticipates a smaller minimum being justified. The SYN schedule is
    DERIVED from the initial RTO and the ceiling rather than written beside them - 1, 2, 4, 8, 16, 32
    and 60 seconds, 123 s of retries plus 60 s waiting on the last, failing at 183 s - so an
    implementation that changes either parameter gets a new schedule instead of an inherited count.

  `tcp_window.rs` - RFC 5681's core with RFC 6928's initial window, AND THE TWO LOSS RESPONSES KEPT
    APART, which is the whole reason it is a module. A timeout drops `cwnd` to ONE segment; three
    duplicate acknowledgements set it to `ssthresh + 3*SMSS` and inflate by one per further
    duplicate. An implementation with one "halve the window" rule puts half a window back onto a path
    that has just stopped delivering. The tests assert the POLICY - `cwnd == SMSS` after a timeout,
    `ssthresh + 3*SMSS` after three duplicates, the RFC 6928 formula evaluated at the fixture's SMSS -
    rather than that something shrank. `Persist` is here too, because a shut window is the peer's
    statement about itself: it probes with exponential backoff and NEVER gives up, since a peer
    advertising zero is answering and is entitled to keep its window shut.

  `tcp_queue.rs` - the unacknowledged bytes, the unsent bytes and the FIN behind them. `accept`
    copies and returns what it took, bounded by the per-flow ceiling AND the service-wide budget it
    is told what is left of. Retransmission is GO-BACK-N, which is what a sender without SACK can
    honestly do - and resegmentation after a Packet Too Big falls out of it: lower the segment size
    and rewind. The acknowledgement path is signed-difference arithmetic throughout, so a queue that
    crosses the sequence wrap does not retire itself on a stale acknowledgement.

  `tcp_close.rs` - the closing handshake and TIME-WAIT, entered on the LOCALLY DECIDABLE condition:
    this side has sent a FIN, had it acknowledged, and has acknowledged the peer's. A rule phrased on
    "the side that sent the first FIN" is not decidable under a simultaneous close, where both sides
    would conclude they are not it and free immediately - losing the lost-final-ACK recovery on both.
    2 MSL is 60 seconds here, MSL fixed at 30. A FIN arriving in TIME-WAIT is acknowledged and
    RESTARTS the timer, which is the recovery the state exists for.

WHAT THE STACK DOES WITH THEM. `TcpConn` carries a `SendQueue`, an `Rto`, a `CongestionWindow`, a
`Persist` and a `Closing`, plus the peer's MSS and window and the path's MSS. The handshake's two
sequence fields stay for the handshake and stop being consulted afterwards: the queue owns the
sequence space from establishment, because the bytes and the sequence they occupy are the same fact
and keeping them in two places is how they come to disagree. `Stack` gained a SUPPLIED monotonic
clock for the same reason the IPv6 host beside it has one - a stack that read the clock itself is a
stack no host test can drive through a schedule.

`tcp_send` accepts and copies; `tcp_pump` builds one segment per call from whatever the congestion
window, the peer's window and the segment size jointly allow; `tcp_on_timer` fires the retransmission,
persist and TIME-WAIT deadlines; `tcp_deadline_any` folds every connection's next wake into the serve
loop's single wait, beside the IPv6 layer's. Without that last part a retransmission would happen
only when unrelated traffic woke the loop, which is a schedule decided by somebody else.

THREE DEFECTS THIS CLOSED IN THE SERVICE ITSELF.

  `socket.send` cut the caller's buffer to ONE segment's worth and returned that length as success.
  It now offers the whole buffer to the queue and returns BYTES ACCEPTED, with zero accepted as a
  typed `Again` rather than a success carrying nothing.

  The SYN retry was a fixed 500 ms interval inside a three-second overall deadline. Against RFC 9293
  section 3.8.3's three-minute minimum that abandons a conforming peer behind a slow path inside a
  sixtieth of the interval the specification reserves. It now walks the derived schedule.

  An ICMP Destination Unreachable with code 4 was read only for its quoted echo sequence. It is a
  PATH MTU REPORT and the one ICMP message a TCP sender must act on: `tcp_on_path_mtu` lowers the
  segment size for every connection to that destination and rewinds their queues.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 260 passed, 46 of them the TCP
sender's - the estimator's arithmetic and its two clamps, Karn's rule, the backoff and the retry
limit, the SYN schedule's duration AND its cap, the initial-window formula at four segment sizes,
both loss responses asserted as policy, recovery ending only on the acknowledgement that covers the
recovery point, slow start against congestion avoidance, the per-flow and aggregate budgets, the
exact-bound pair on the queue, segmentation, partial acknowledgement, the sequence wrap, the FIN as
sequence space, a lost FIN, a lost final ACK, an expiry that releases at exactly 2 MSL and not
before, a simultaneous close through CLOSING on both endpoints, resegmentation, and a zero window
whose lost update is recovered by persist rather than by a timeout that is not running.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 198s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. The gate exercises the IPv4 path
alongside the IPv6 one - its narrow-link and router rows both ping over IPv4 - so a sender that had
broken the other family's transport would have failed there.

WHAT M2 DOES NOT CLAIM. The refusals stand unchanged: no CUBIC, no BBR, no SACK, no ECN, no pacing
and no delayed-ACK tuning. The line this item holds is that a sender must not deadlock, must not
ignore loss and must not ignore the peer's window - and the guest-level oracles that drive the SYN
schedule through M1's public open-target belong with M10's dual-stack matrix, where the peer that can
withhold a SYN-ACK for three minutes lives.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T04:55:00Z):

M3 - UDP AND TCP USE THE ADDRESS FAMILY IN EVERY INVARIANT.

WHAT WAS FAMILY-BLIND BEFORE THIS. The TCP connection table was keyed on `(local_port, remote IPv4
address, remote port)` with this host's own address implicit, the listen table was a list of bare
`u16` ports, and every segment and datagram was built inside an IPv4 frame with an IPv4
pseudo-header. None of that can carry a second family, and two of them cannot carry it even in
principle: a table keyed on a port and four octets ALIASES the two families the moment both are in
use, and a list of ports cannot express "IPv4 on 80 and IPv6 on 80 are two different listeners".

THREE RULES MOVED INTO `service-logic`, WHERE THEY HAVE FIXTURES.

  `tcp_bind.rs` - the bind matrix as ONE function. Which claims may share a port is the sort of rule
    that gets written three times - the admission, the demultiplexer, a comment - and the three
    disagree the first time a case is added. Both halves of the stack are written against this one:
    `listen` is the admission and `listens_for` is the lookup. An IPv4-mapped IPv6 address is refused
    as a listen address in every mode, because the direct spelling exists and a second one costs
    every consumer a check that, when forgotten, grants IPv4 reach to a listener that asked for IPv6.

  `tcp_flow.rs` - which operation an ICMP error is about. The full tuple, the family AND the
    interface generation: equal ports in two families are two flows, and the same tuple on a replaced
    NIC is a third. The Packet Too Big rule is here too, and every check happens BEFORE anything is
    written - an out-of-window quotation must not have already counted against a cache or triggered a
    resegmentation by the time it is refused. It consumes P02M0174 M6's frozen send-bound check
    rather than restating it.

  `ipv6_packet::udp_checksum` - mandatory, with a computed zero transmitted as `0xffff` and a
    received zero REFUSED. Over IPv4 a zero field means "not computed" and a receiver accepts it;
    IPv6 has no header checksum beneath the transport, so RFC 8200 section 8.1 removes the exemption.
    It is the one line an implementation that ported its IPv4 checksum across leaves out.

WHAT THE STACK DOES WITH THEM. `TcpConn` carries `local` and `remote` as family-aware addresses and
`find_conn` matches the whole four-tuple. `emit_tcp` is the one place a segment reaches the wire:
identical sequence numbers, flags and payload, and below them either the IPv4 frame builder as before
or `build_tcp_segment` plus the IPv6 host, which owns the route and the neighbour cache. That last
part is why an IPv6 connection stores no peer MAC - resolution happens when the segment goes out,
which is what keeps an off-link connection following the route rather than a MAC frozen at open time.

`on_tcp_segment` is the family-neutral half of ingress, reached from the IPv4 frame path and from
`on_tcp6`, which the IPv6 host's deliveries feed. `on_udp6` verifies the mandatory checksum before
looking at a single field of the datagram. `send_udp6` builds one, and SNTP now uses either family
through the same request body - one definition, because it is the message and not the framing.

`on_ipv6_error` is the consumer P02M0174's quoted-error queue was built for. The layer below
validated the quotation as far as a layer holding no flow state can and deliberately stopped;
whether this host actually SENT the quoted packet is a lookup in a send queue, and the send queue is
this table. A report that passes lowers the flow's own limit and resegments what is outstanding -
and it does so whatever the bounded PMTU cache answers, because cache exhaustion must never restore
a limit the path has already refused to carry.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 278 passed, 18 of them M3's - the
whole bind matrix including both orders of the dual-stack conflict, the mapped-address refusal in
every mode, equal ports in two families that cannot alias, the flow key with every part of it
decisive, an error quoting one flow doing nothing to another, the empty-flight and out-of-window
refusals, both families' path-MTU floors, successive strictly lower reports, the `Capacity` case, two
probes of one trace told apart by the quoted sequence, and the responder kept distinct from the
quoted destination.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 201s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. The echo row is the one that
matters most here: it drives a Packet Too Big quoting the guest's own reply through the whole new
consumer path, and the guest still answers exactly the errors that quote its own address.

WHAT M3 LEAVES WHERE THE PLAN PUTS IT. Ping and traceroute over IPv6 need an ICMPv6 echo CLIENT, and
this file's own text places that with M7's retained diagnostic operation - "dispatch these immediately
to M7's retained diagnostic operation" - so `ping`, `probe` and their correlation against a live probe
are wired here as the matching RULE and implemented there. The guest oracle for an IPv6 TCP
connection belongs to M10's dual-stack matrix, which is where the peer that can complete a handshake
lives; what M3 owes and has is the implementation and its host fixtures.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T05:40:00Z):

M4 - INBOUND TCP STATE IS CHARGED BEFORE IT IS PUBLISHED.

WHAT WAS UNBOUNDED BEFORE THIS. A handle ceiling bounds none of the state this item is about: a
half-open control block is expressly the state with NO socket channel, and unacknowledged transmit
bytes say nothing about receive storage. The pool grew on demand, every slot allocated 65535 bytes of
receive buffer whether or not it was in use, and the buffer grew unconditionally to 262140 the moment
a peer offered window scaling - before a single byte had arrived. Sixty-four of those base buffers
alone would be 4 MB against the 2 MB this item budgets, so the old numbers could not survive the
accounting at all.

`tcp_admission.rs` IS THE ACCOUNTING, AND EVERY NUMBER IN IT IS A NUMBER. Half-open 64, backlog 32 per
listener and 64 across the service, 128 control blocks, 2 MiB of receive storage, 4 MiB of transmit.
The refusal precedence is fixed - the service-wide occupied-slot count first, then the listener's own
backlog, then half-open, control blocks and receive storage - so a refusal has exactly ONE reason even
when several limits are full, and an operator reading the counters can tell "this listener is slow"
from "this machine is full".

THE REFUSAL POINT IS THE SYN, AND THAT IS NOT A PREFERENCE. Refusing after the handshake completes
means the peer has had the SYN-ACK and sent its final ACK: it is ESTABLISHED and will retransmit
DATA, never the handshake. With the control block gone the tuple is CLOSED and RFC 9293 section 3.10.7
then requires a RESET - so refusing late is a choice between stranding the peer and sending exactly
the reset that refusing late was supposed to avoid. A SYN that cannot be admitted is DROPPED, creates
no control block, and charges nothing; a SYN is the segment a peer retransmits, so its next attempt
finds the queue drained. That is the retry story the late refusal wanted and could not have.

THE RECEIVE BUFFER IS FUNDED BEFORE IT IS ADVERTISED. The base is 16 kB, charged before allocation;
an unused pool slot now owns no buffer at all, and freeing one releases what was actually charged
rather than keeping an uncharged base buffer in a reusable slot - which is how a pool comes to hold
memory no budget knows about. Negotiating window scaling RECORDS the scale and grows nothing. Growth
afterwards is fallible, charged with BOTH buffers counted while the copy happens, bounded by 65535
unscaled and 262140 at this profile's scale of two, and a refusal keeps the current buffer and touches
no other connection: nothing is evicted to grow somebody else.

THE BACKLOG IS A PUBLIC PARAMETER AND ITS CONTRACT IS FROZEN. Zero is a typed refusal - a listener
that may hold no unaccepted connection cannot work, and promoting it silently would be a second
meaning for a value the caller wrote. Anything above 32 is CLAMPED and succeeds at the cap, because a
refusal would make a portable program guess a number this file chose; `listen-result` carries the
effective backlog beside the capability, so the caller learns the real cap in the same reply.

A SLOT IS HELD UNTIL A HANDOFF ACTUALLY SUCCEEDS. Removing a queue entry before a fallible handoff is
not release: a failed channel allocation leaves the connection queued and charged and the listener
live. Withdrawal aborts what a listener was holding, because no caller can accept them afterwards;
sockets already handed off keep their own lifecycles. And a half-open connection now has its own
SYN-ACK retransmission schedule with an EXPIRY, which is what stops a peer that opens and walks away
from holding a backlog slot until the machine fills.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 292 passed, 14 of them M4's - the
zero-and-clamp rule, a listener at exactly its effective backlog and one past it, the service-wide
count checked first with every listener under its own, a handshake that changes which kind of slot is
held and not how many, a failed handoff that releases nothing and a successful one that does, the
concurrent-handshake case with a backlog of one, release exactly once, withdrawal that aborts and
releases everything, each budget at its own bound, the arithmetic that makes 64 base buffers fit under
the 2 MB cap while 64 of the old ones would not, growth bounded by the negotiated scale, a refused
growth that keeps its buffer and touches nobody else, a close that releases the allocation, and one
dual-stack listener sharing its reservation across both families.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 199s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. The receive buffer shrinking from
65535 bytes to 16384 changes what every connection advertises, so a boot that could no longer carry
its own traffic would have failed here.

WHAT M4 LEAVES TO M10. The guest-visible flood oracles - a listener at its own backlog with the
service-wide count well under 64, 64 slots reached across several listeners each under its own, a SYN
at a full listener getting no segment back and the same peer's retransmission accepted once the queue
drains, and the concurrent-handshake case driven over the wire - are M10's dual-stack matrix, which is
where a peer that can withhold a final ACK lives. Every one of those rules has its host fixture here;
what M10 adds is the same assertions against a booted system.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T06:20:00Z):

M5 - SOURCE AND DESTINATION SELECTION FOLLOW A PINNED ALGORITHM.

"IPv6 FIRST" WAS THE RULE AND IT IS NOT AN ALGORITHM. It has no answer for a host with two global
prefixes, for a deprecated address that is still usable for existing work, for a destination with no
route, or for the case where the two families would reach different machines. What the service
actually did was take the FIRST candidate the caller listed and try only that one.

`addr_select.rs` IS RFC 6724, AND THE TABLE IS THE DEFAULT ONE VERBATIM. Nine rows, no appliance
rows, and not configurable in this milestone - a table an operator can edit is a way to make address
selection differ between two machines running one image. Section 5 chooses the source and section 6
orders the destinations; the rules this appliance has no state for - home addresses, temporary
addresses, mobility, a second interface - are ABSENT rather than stubbed, because a rule with nothing
to compare changes no outcome and pretending to apply it would be the pretence.

"IPv6 FIRST" TURNS OUT TO BE AN OUTCOME OF THE TABLE rather than a rule: `::/0` has precedence 40 and
`::ffff:0:0/96` has 35, so a global IPv6 destination outranks an IPv4 one by section 6's rule 7 - and
a LINK-LOCAL one does not, which is exactly the distinction the old rule could not make.

THE TIE-BREAKS ARE MECHANICAL AND TOTAL, which they were not. Ties that survived every rule were
decided by insertion order, which is not a rule anybody can rely on and is not the same on two runs.
Sources break on the lower address bytes and then the lower interface identity; routes on the longer
prefix, then DIRECT before VIA, then the earlier router in P02M0174's frozen order, then the lower
prefix bytes, then the lower interface identity. The DIRECT key exists because a key that started at
"the router this route names" is undefined for a route that names none, and the identity key exists
because two records equal in every other key but sitting on different interfaces stayed distinct
records with identical keys - so insertion order decided after all.

THE ROUTER KEY IS P02M0174'S ORDER, CONSUMED WHOLE. That order puts REACHABILITY before advertised
preference, deliberately, so an unreachable high-preference router stays behind a reachable
lower-preference one. An address comparison here would let an unreachable router win - the exact
outcome that ordering exists to prevent - and would give the two sides of one seam different policies.
RFC 8028's default-router rule restricts the candidates to the chosen source prefix's advertisers and
takes the FIRST of them in that same order: restricting is this milestone's decision, ordering is not.
A DIRECT route is never filtered through the advertiser set, because RFC 4861 section 5.2 decides the
on-link next hop BEFORE default-router selection and RFC 8028 extends only the later choice.

THE FALLBACK IS SEQUENTIAL AND THE ORDER IS DECIDED ONCE. `ordered_destinations` applies the rules at
admission and the order is retained for that open: candidates are revalidated before they start, the
name is never re-resolved, nothing is added, and a failed candidate is never revisited. One active
attempt at a time is what makes the budgets mean anything - a parallel open would charge every
candidate at once and a caller's RPC deadline would become the fallback mechanism, which is a
fallback nobody wrote down. A NONFINAL candidate expires three seconds after it starts, including
time spent resolving its next hop, and expiry takes precedence over a retransmission due at the same
instant; the FINAL candidate, a singleton literal included, gets M2's complete SYN schedule, because
there is nothing left to fall back to.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 305 passed, 13 of them M5's - the
default table with `2001::/32` correctly read as Teredo rather than the documentation prefix, scope
with IPv4's own rules kept (without which a link-local IPv4 address would be chosen to reach the
internet), the source rules in order including a deprecated address that loses to a preferred one and
is still chosen when it is all there is, labels and longest-matching-prefix, a source tie broken the
same way whichever order the candidates arrive in, the destination rules with an unusable destination
last and a deprecated source pushing its destination down, and the THREE route cases the keys exist
for: reachability deciding two equal-prefix VIA routes, DIRECT beating VIA, and two routes differing
only in interface identity choosing the same one twice.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 200s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. Every open in that gate is a
singleton literal, so it exercises the final-candidate path - the one with the complete SYN schedule
and no three-second cap - which is the half a multi-candidate fixture would not reach.

ONE THING WORTH RECORDING. `rustc` took a SIGSEGV partway through a build of an unrelated binary; the
retry was clean. That is the known intermittent fault on this machine and not a property of this
change.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T07:00:00Z):

M6 - DNS IS CORRELATED, VALIDATED AND BOUNDED.

WHAT THE RESOLVER DID BEFORE THIS. `parse_dns_response` took ANY datagram whose source port was 53,
walked its answer section, and returned the first A record it found. It compared no transaction ID,
no question, no type, no class and no port; it followed compression pointers with no direction rule
and no bound; it collapsed every failure into `None`; and it sent from the fixed port `0x9876` with a
transaction ID incremented by one per query. A spoofed or cross-query answer was indistinguishable
from the real one, and an off-path forgery needed only to know that a lookup was happening.

`dns.rs` IS THE WHOLE OF IT, IN `service-logic` WHERE IT HAS FIXTURES. Names are compared in ONE
normalized form - lowercase, no trailing dot - because a server may echo the question in any case it
likes and several deliberately randomize it, so comparing the bytes as sent rejects correct answers.
The response must be a response, of the right opcode, echoing exactly the question asked. Every
traversal is bounded, and a compression pointer must target a STRICTLY EARLIER offset: that backward
rule is what makes a loop impossible rather than merely slow, and the count on top of it bounds the
legal-but-absurd case that is still an attack. A CNAME chain that revisits a name is a loop and is
refused as one, which a hop count alone would follow until it ran out.

COMPARING IS ONLY HALF OF IT, AND THE OTHER HALF IS WHY. RFC 5452 section 9.2 asks for unpredictable
query IDs AND unpredictable source ports because those two fields are the whole of a stub resolver's
off-path defence: a resolver that increments its ID by one and sends from a fixed port satisfies every
comparison above against a forgery that guessed the next one. The identity is now drawn per query
from the same randomness the IPv6 identifier uses, the source port is an ephemeral one, a drawn tuple
that collides with a live one is REDRAWN rather than reused - two live queries sharing an identity
would each accept the other's answer - and the tuple is RETIRED when the query finishes or expires,
so a late answer to a question nobody is waiting for matches nothing.

THE CONDITIONAL PART IS HONOURED AS THE PLAN STATES IT. The correlation contract - the fields
compared, the redraw, the retirement - is unconditional and its negatives run on every profile,
because they are about matching. The unguessability depends on what the kernel's randomness is worth
on that profile, and this file says so where the draw happens rather than claiming a property the
port may not have.

THE OUTCOMES ARE KEPT APART. A name that does not exist, a server that failed, a well-formed answer
with no address, a malformed response, a bad compression pointer, a chain too long, too many records,
and TRUNCATION are eight different things; the old resolver had one. Truncation is an ordinary
outcome - AAAA and CNAME sets are exactly the answers that overflow a datagram - so it is answered by
asking again over TCP with RFC 1035's two-byte length framing, which is what RFC 7766 section 5 asks
of a general-purpose stub resolver. Reading it as malformed would mean never retrying.

AND A LINK-LOCAL ANSWER CANNOT ESCAPE UNSCOPED. `fe80::1` names a different host on each link and
nothing in a DNS answer says which link it meant, so the resolver cannot supply the provenance M1's
scoped form requires. Such an address is dropped from the result rather than handed out as though it
were routable - and dropped rather than refusing the whole name, so a mixed answer still works.

THE RESULT IS THE ORDERED LIST, and the name-based callers hand the whole of it to one open-target:
`nc` and `tcp` pass every candidate to `connect` and let M5's sequential engine try them in order.
Reading only the first is not fallback, and that is what they used to do.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 322 passed, 17 of them M6's - the one
normalized form, an ordinary answer with the smallest lifetime across the records used, both families,
the forgery carrying the NEXT sequential identity, a cross-query answer and the same name asked as a
different type, a query rather than a response, the four server outcomes kept apart, truncation as its
own outcome, a CNAME chain followed to its address, a chain that loops and one that is merely long, a
compression pointer to itself and one that points forward, a declared record count past the bound and
one the message does not carry, more addresses than the bound dropped rather than refusing the name,
the query's own round trip, an identity redrawn on collision and retired on completion, an ephemeral
source port, and a link-local answer that cannot escape without its link.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 200s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. No row resolves a name, so what it
proves here is the negative: rewriting the resolver's framing and the stack's event type changed
nothing about the IPv6 layer beneath them.

ONE THING THIS MOVED THAT WAS NOT ASKED FOR, AND WHY. `Stack::build_dns_query` used to encode the
header and the question itself; it now wraps a message the logic crate built. The rules and their
fixtures cannot live in two places, and the frame builder is the half that needs a NIC.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T07:50:00Z):

M7 - THE SERVICE BECOMES EVENT-DRIVEN AND PER-FAMILY.

THE SERVICE USED TO BLOCK ON DHCPv4 BEFORE IT SAID IT WAS ONLINE. That is the shape this item exists
to replace, and it is worse than slow: an IPv6-only boot waited for a conversation it would never
have, and every caller waited for a lease it might not need. It had two knobs, `net.arp-cache` and
`net.mtu`, and no way to say which families it ran or where either of them had got to.

`net.families` IS THE SURFACE, WITH THREE VALUES AND NO MORE. `ipv4`, `ipv6`, `dual`; absent or
unparseable is `dual`, which is what the machine does today extended to the second family. NOT two
independent enable flags: that is four states, one of which is "no networking configured at all" and
would have to be given a meaning nobody wants. It is read AT STARTUP ONLY, where the other knobs
are - ConfigService has `get`, `list`, `set`, `remove` and `seal` and no subscription operation, so a
promise to notice a later change would be a promise nothing can keep. A change takes effect at the
next start of the service, which is what the machine can actually do.

ONLINE IS REPORTED BEFORE ANY FAMILY IS READY, and that is the whole of "start serving immediately".
Readiness is now a per-family fact `capacity` carries - `disabled`, `configuring`, `ready`, `failed` -
recomputed from the tables every time it is asked rather than latched at boot.

AN ON-LINK ROUTE IS ENOUGH TO BE READY. A host that can reach its own link is working, and demanding
a default route would report a router-less link as broken - which this appliance runs on. And
SOLICITATION REACHING ITS MAXIMUM INTERVAL IS NEVER FAILURE: P02M0174 asks indefinitely, so a link
with no router is one this host keeps asking. `failed` needs a reported failure AND nothing retrying,
and it is a report rather than a terminal state: an address and a route arriving afterwards make the
family ready again.

ONLY `disabled` IS A VETO. Everything else is decided per destination against the current addresses
and routes: a family that is still `configuring` may have a usable on-link path, and one that is
`ready` may have no route to one particular destination. Readiness is observability and cannot
override that lookup - which is why the veto is a separate check on the family rather than a test of
the readiness word.

THE PENDING PARTITION IS RESERVED BEFORE ANYTHING IS SENT. DNS 16 with 4 per client, SNTP 8,
DIAGNOSTIC 16 with 4 per client shared between ping and probe because they are one mechanism, and
DHCP 2 RESERVED so the service's own lease work always has capacity client work cannot take. The caps
sum to 42 of the 128 slots and the 86 left are unavailable to any kind: HEADROOM IS NOT PERMISSION,
so a seventeenth DNS operation is refused while the partition is two-thirds empty. A caller at a cap
is told no before a packet leaves, rather than after one has and the reply has nowhere to go.

AND THE ECHO IDENTITY COMES FROM ONE PERSISTENT COUNTER. The request handlers used a temporary
`seq = 0`, so two probes started a millisecond apart carried the same pair - and an error quoting one
would be attributed to both, which is exactly the correlation M3's rules exist to make possible. The
identifier advances when the sequence wraps, so the full pair is not reused.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 332 passed, 10 of them M7's - the
three values and everything else reading as `dual`, a family outside the profile disabled whatever
else is true, an on-link route being enough, waiting never being failure and failure needing nothing
left to try, each kind refusing at its own cap with the partition two-thirds empty, one client unable
to take more than its share while another is unaffected, release returning the slot to both counts, a
client that goes away releasing everything, DHCP's reserved capacity surviving a full partition, and
the echo pair advancing its identifier on wrap.

`cargo test --manifest-path src/user/libs/protocol/network-proto/Cargo.toml`: 49 passed.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 200s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed, after two corrections the gate
itself made.

FIRST, THE CONSOLE IS SLOW ENOUGH TO BE MEASURABLE. Reporting `online` after the families were
started put two console lines between queueing the first IPv6 frames and yielding to the driver, and
the solicitation-schedule row measures those frames' arrival on the wire: the first interval came
back 3.543 s against a band starting at 3.6. The report is now made BEFORE either family is started,
which is both faster and more faithful to what the item asks for.

SECOND, ONE OF THE ECHO ROW'S ASSERTIONS WAS A COIN FLIP AND THIS FOUND IT. The peer sent four ICMP
errors back to back and the guest writes a report line to the serial console between frames; the last
of that burst was sometimes dropped by the receive ring, so `errors-seen=3` passed or failed by
timing. The row is about CORRELATION - which flow an error belongs to - so the peer now sends them
one per idle pass. Burst survival is the flood row's subject and has its own oracle there. The
flakiness predates this milestone; what M7 changed was the timing that made it show.

WHAT M7 LEAVES TO M10 AND WHAT IT LEAVES ALONE. The three-profile matrix - setting the key to each
value and asserting which families reach `ready`, that `online` precedes any of them, and that a send
on a `disabled` family is refused - is M10's, and every rule it will assert has its host fixture here
and its reporting surface in `capacity` and `ss`. The blocking `do_ping`/`do_probe` stacks are
admitted and released through the partition and draw their identity from the persistent counter, but
their state still lives on those stacks rather than in retained slots; moving it there is what M8's
"every internal UDP operation is correlated" and M9's migration reach, and doing it here would have
been rebuilding the serve loop under an item that had already changed its contract twice.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T08:50:00Z):

M8 - EVERY INTERNAL UDP OPERATION IS CORRELATED AND VALIDATED, NOT ONLY DNS.

TWO PATHS WERE THE SAME CLASS OF DEFECT AS THE DNS ONE AND BOTH WERE WORSE IN EFFECT.

SNTP SET THE WALL CLOCK FROM ANY DATAGRAM ON PORT 123. The client's request was 48 bytes with only
the first set, so its transmit timestamp was ZERO - there was no per-request value at all - and the
reply was accepted on source port alone, parsed for a transmit timestamp, and handed to TimeService.
A single forged datagram moved system time.

`sntp.rs` supplies the per-request value and checks everything before the clock is touched: mode 4,
version 3 or 4, leap indicator not "alarm", STRATUM IN `1..=15` - the old rule refused 0 and 16 and
accepted 17 through 255, which are RESERVED and are exactly where a forgery would sit - the ORIGINATE
field equal to what this host sent, and the reply's own TRANSMIT field, the one that moves the clock,
NON-ZERO and DIFFERENT from the last accepted reply's. That last check is what rejects a byte-identical
replay: everything else about it is perfect, and a second reading of a clock is never the same reading.

DHCP'S ORDERING WAS WORSE THAN ITS MATCHING. `parse_dhcp` wrote the parsed lease into the stack BEFORE
its caller looked at the message type or the client's state, so a late or losing reply mutated stored
lease data even when the caller then ignored the event - and nothing downstream could undo it. It also
used a FIXED transaction identity, on the reasoning that "SLIRP is the only DHCP source", which is a
statement about one deployment rather than about the protocol.

`dhcp.rs` makes it a staged, state-specific transaction. The parse no longer commits: it holds the
lease and the caller admits the reply first, so a frame that is not admissible in the current phase
changes NOTHING. `xid` and `chaddr` are matched and are not sufficient - every legitimate server
answering the same discover shares both - so the client SELECTS one offer and freezes the server and
the address. SELECTING takes OFFERs and nothing else; REQUESTING and RENEWING take an ACK or NAK from
the selected server; REBINDING takes one from any server, which is the phase that exists for it.

AND THE NAK RULE IS THE ONE THAT WOULD HAVE BEEN GOT WRONG. An ACK is admitted in the destination form
its phase implies - unicast while renewing - while a NAK is admitted BROADCAST in every phase, because
RFC 2131 section 4.1 requires a server to broadcast every DHCPNAK when `giaddr` is zero, a renewal
whose REQUEST was unicast included. A client that admitted only unicast replies while renewing would
discard the conforming NAK for a lease the server has just declared invalid and go on using it, which
is the one outcome the staged transaction exists to prevent. An admitted NAK CLEARS the lease and the
bound address: a state transition, not a non-event.

AND THE FRAMING IS VALIDATED RATHER THAN DISPATCHED ON. The declared length must match what arrived,
a checksum that is present must verify - over IPv4 a zero field legitimately means "not computed",
the one exemption IPv6 does not have - and the destination port is part of the tuple.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/services/logic/Cargo.toml`: 351 passed, 19 of them M8's - the
per-request timestamp where there was a zero, a reply answering a different request, a byte-identical
replay rejected by the transmit timestamp and nothing else, a zero transmit timestamp, strata 0, 16,
17, 200 and 255 refused and 1, 2 and 15 accepted, the alarm leap indicator, mode and version; and for
DHCP a foreign transaction and a foreign client refused before the phase is consulted, SELECTING
taking offers and nothing else, a competing offer after one was chosen, an ACK from a server that was
not selected and one for the wrong address, a renewal's unicast ACK, rebinding accepting a new server,
an admissible NAK clearing the lease in each of the three phases that admit one, THE BROADCAST NAK
ANSWERING A UNICAST RENEWAL, a late reply after the phase moved on, and an offer with no server
identifier.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 199s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. The row that matters most here is
the narrow link, whose guest runs its whole DHCP conversation with the new staged transaction: a
client that had broken its own lease path would have lost the IPv4 address that row asserts.

ONE HARNESS DEFECT THIS FOUND, AND IT IS THE SAME KIND AS M1'S. The kernel suite's DHCP fixture
PRE-QUEUED its OFFER and ACK with a zero transaction identity and a zero client address, because the
client it was answering used a constant identity and checked neither. A client that draws one per
exchange cannot be answered by a fixture that has not read the request - so the fixture now reads the
DISCOVER and echoes what it carried, which is what a server does. That required stepping the scheduler
in bounded slices rather than draining it: an unbounded drain runs the client through its whole DHCP
timeout before the test thread sees a single frame, which is why the conversation had been pre-queued
in the first place.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T09:20:00Z):

M9 - NETWORKSERVICE AND CLIENTS MIGRATE TOGETHER.

MOST OF THE MIGRATION HAPPENED IN M1, AND DELIBERATELY. That item's own text requires one reviewed
compatibility-manifest update and all first-party callers moved atomically rather than two APIs kept
in parallel, so `network-client`, its provider and generated bindings, `network_service`,
`time_service`, and the tools `ip`, `arp`, `ss`, `ping`, `traceroute`, `nslookup`, `nc`, `tcp` and
`httpd` moved when the contract did. Splitting that across two milestones would have meant a period
with a schema nothing spoke. What M9 owes on top of it is what M1 did not reach.

THREE GRAMMARS, AND THEY ARE THREE. The temptation is one parser with a flag, and it is wrong in a
way that shows up as a security property rather than a formatting one:

  DISPLAY is RFC 5952's canonical rendering, which M1 already put in `addr.rs` - lowercase, no leading
    zeros, `::` over the longest run and never over a run of one.

  INPUT adds RFC 4007's `%zone`, BOUNDED, and only where a zone means something: `fe80::1%if0` is a
    complete address and `2001:db8::1%if0` is not - it is over-specified in a way that hides a
    mistake, because the writer believed the interface mattered and it does not. The zone is LOCAL UI
    SCOPE and is never transmitted: `resolve_target` maps it to the stable interface identity the
    service reports and sends that.

  AUTHORITY is `[address]:port` with brackets on the family that needs them, and it REFUSES A ZONE.
    `fe80::1:80` cannot be read at all - the colon before the port is indistinguishable from the ones
    inside the address - so the unbracketed IPv6 form is ambiguous and is refused rather than guessed
    at, and `[10.0.2.15]:80` is refused too because one address with two spellings is one too many.
    RFC 6874's zone-in-URI extension is deprecated and is NOT implemented: following its `%25` advice
    would produce non-standard URLs, make two origins compare unequal that name the same service, and
    leak a local interface name into something meant to travel.

`arp` IS EXPLICITLY IPv4 AGAIN. The M1 migration widened its rendering to carry either family, which
made it a view of two tables under a name that means one of them. It now shows the ARP table and says
"no neighbors" when that table is empty however many IPv6 neighbours exist; those are `ip`'s, which
has a section for them.

AND THE OPENING WAIT IS ALREADY UNBOUNDED. The generated client is constructed with `deadline: 0`,
which is the existing no-deadline wait, so a generic receive timeout cannot preempt the final
candidate's 183-second SYN schedule - the one M2 derived and M5 measures from the actual first
transmission. No tool starts the next address itself; the service owns the ordering, the deadlines and
the retirement, which is what M5 put there.

VERIFICATION PERFORMED.

`cargo test --manifest-path src/user/libs/protocol/network-proto/Cargo.toml`: 54 passed, 5 of them
M9's - the zone accepted where it means something and refused where it does not, an empty and an
over-long zone, the bracketed and unbracketed authorities with the ambiguous form refused, brackets
refused on IPv4, a zone refused in an authority in both its spellings, ports refused rather than
wrapped or read two ways, and every authority this host writes parsing back to what wrote it.

`./build.sh --arch x86_64`: clean under `-D warnings`.

`./test.sh --arch x86_64`: 387 passed, 201s.

`./check.sh --gate source-hygiene --gate host-tests --gate verify-model`: all clean.

`./check.sh --gate ipv6-peer`: all six rows passed. Two of them assert the guest's
IPv4 address and an answered IPv4 ping, which is what proves `arp` narrowing its view and the tools
taking the new input grammar changed nothing about what the machine actually carries.

WHAT REMAINS UNCHANGED ON PURPOSE. Permission manifests gain no raw NIC access: every tool still
reaches the network through a NetworkService capability its manifest names, and nothing in this
migration added a device grant. `network.open` is still how a client obtains another channel when it
needs independent cancellation, which is why no new broker grant or cancellation operation was added.

---

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0175 (2026-09-11T11:12:45Z):

M10 - DUAL-STACK BEHAVIOUR IS TESTED AS A MATRIX.

WHAT THIS MILESTONE TURNED OUT TO BE. It is written as a testing milestone, and about half of it was.
The other half was finding that several of the behaviours M1 to M9 promised were not there to test -
some never wired into the production path, some wired and broken - and that is what most of the work
below is. Every fixture here runs against the code that actually ships; where a case had nothing to
run against, the code it names was built or repaired first and the fixture written afterwards.

THE FIVE DEFECTS THE CASES EXPOSED, each of which made a promised behaviour untrue on a running
system rather than merely untested:

1. THE THREE-SECOND FALLBACK CAP NEVER EXPIRED. `tcp_open_sequence` computed a nonfinal candidate's
   deadline as `clock() + ticks_from_ms(CANDIDATE_MS)`. `ticks_from_ms` takes the MILLISECOND a
   deadline falls on and returns the TICK to wait until, so passing it a duration and adding the
   current tick to its answer produced a deadline an entire uptime in the future. The handover the
   whole fallback contract rests on could not fire. The same confusion appeared twice more in the
   SYN schedule's own waits, where each retransmission interval was similarly inflated.

2. `ping`, `nslookup`, `nc` AND `tcp` NEVER SAW THEIR ARGUMENTS. All four read the launch context's
   argument string, took its LENGTH, and then passed `&buf[..len]` - where `buf` is the scratch
   buffer `granted_capability` receives a tagged message into, and which on the governed launch path
   is never written at all. Every target these tools were given was whatever that buffer happened to
   hold. `ping 10.0.2.2` reported "cannot resolve" for an address that needs no resolving.

3. AN IPv6 TCP CONNECTION COULD ESTABLISH AND THEN SEND NOTHING. `drain_tx` loops `tcp_pump` until it
   returns a zero length. For IPv4 a zero means "nothing to send"; for IPv6 it means "the segment was
   queued on the host below rather than written into your buffer", which is what `emit_tcp` does for
   that family and always has. So the first data segment was queued, the loop stopped, nothing
   drained the host, and the caller was told its bytes had been accepted. The handshake worked
   because another path drains the host, which is why this survived every test that only connected.

4. RFC 8028'S RESTRICTION WAS NOT IN THE SEND PATH. M5 built `addr_select::default_router` and its
   ordering and froze the rule; nothing called it. `Ipv6Host::next_hop_for` took the route table's
   answer whole, so a packet sent from an address one router delegated could leave through a
   different router - which is what makes an ISP's source-address filter drop it, and the failure
   looks like a working connection that never gets a reply.

5. A PACKET HELD FOR ADDRESS RESOLUTION COUNTED AS TRANSMITTED. `SendQueue::next_segment` advances
   `SND.NXT` when it cuts a segment, and the Packet Too Big check validated against that. A segment
   the layer below is still holding while it resolves a neighbour has never been on the wire, so no
   router can have quoted it - and a quotation of it was being accepted.

WHAT WAS BUILT.

`src/user/services/logic/src/tcp_transmit.rs` (new, with `tests.rs`). `TransmitBound`: a per-flow
high-water mark of sequence space actually handed to the driver, with `hold`/`on_sent`/`on_retired`
for the frames the layer below retains and an epoch so a completion for a control block that has
since been reused is REFUSED rather than merely unmatched. It is a high-water mark and not a cursor
on purpose: Go-Back-N rewinds `SND.NXT` over bytes that have been transmitted, and a second Packet
Too Big about them is legitimate. `Ipv6Host::send_transport` now returns `Handoff::{Sent, Held,
Refused}`, the host keeps a completion queue (`take_completions`) filled from `Action::Resolved` and
`Action::Retire`, and `Stack::apply_ipv6_completions` applies them to the flow that owns each token.
`on_ipv6_error` validates against `sent.transmitted()`.

`src/user/services/logic/src/open_sequence.rs` (new, with `tests.rs`). The fallback transition core:
one attempt at a time, `NONFINAL_MS = 3000`, the final candidate uncapped, a late success or failure
from a retired attempt changing nothing, and `total_deadline_ms` derived from the same parameters as
the schedule (183 000 for a singleton, 186 000 for two). `tcp_open_sequence` now drives it instead of
keeping its own copy of the rules.

`src/user/services/logic/src/invalidation.rs` (new, with `tests.rs`). The table for what an address
going away does to each operation state, including the two rows a previous version got wrong: an
unsent operation whose source the CALLER named fails rather than silently reselecting, and a listener
bound to a SPECIFIC local address is withdrawn with its port released while a wildcard one stays.
`Stack::on_address_invalidated` applies it to the listeners and connections; `report_ipv6` drives it
from the invalidation queue and reports `withdrawn=` and `closed=`.

`address-unavailable` was added to `liber:base@1`'s `error` enum, regenerated with
`./gen.sh --accept-breaking`. Two exhaustive matches stopped the build at the places that decide -
`world_errors::is_refusal` (not a refusal: it is the machine's networking state, which a guest
neither chose nor can see) and `lico`'s volume vocabulary - and both were given an answer.

RFC 8028 wired: `PrefixTable::covering` turns a source address back into the set that advertised its
prefix, `addr_select::default_router_for_source` takes the first router that is in both P02M0174's
order and that set, and `next_hop_for` consults it for every off-link send. When the prefix outlives
every advertiser the general rules apply and the fallback is COUNTED - `router_fallbacks` appears in
the IPv6 report line rather than being taken silently.

A caller-named source is now honoured rather than refused: `named_source` checks its shape,
`Stack::holds_local` checks the machine still has it (`address-unavailable` if not), `source_serves`
fails the destinations it cannot serve without failing the open, and `tcp_open6_from` uses it or
fails. Ranking uses the caller's source, because ranking by one that will not be used answers a
question nobody asked.

IPv6 diagnostics: `ipv6_icmp::build_echo_request`, `Ipv6Host::send_echo` with a per-packet hop limit
(a traceroute probe IS its hop limit), `Stack::take_probe_quote` matching retained ICMPv6 errors with
`tcp_flow::probe_owns` - the same core the transports use - `Event::EchoReply6`, and `do_ping6` /
`do_probe6`. `ping` and `probe` now take either family through `open_destination`.

The resolver was IPv4-only in two separate ways: it asked only for `A`, and only over IPv4. It now
asks `AAAA` and `A` under separate identities, per the `net.families` profile, and `dns_servers` puts
the RDNSS servers learned over IPv6 ahead of the IPv4 one. Truncation is retried over TCP to the SAME
server, because truncation is that server's answer.

`httpd` binds `DualStack` rather than IPv4-only; its comment said the other family's transports did
not exist yet, and they do. `tools::resolve_target` accepts a bounded `%zone` and matches it against
the interface identity the service reports, in both the `%if0` and `%if0.1` spellings.

THE FIXTURES.

`tcp_flow/tests.rs` gained `mod packet_too_big` - the full named list against the production decision
core with a controlled monotonic clock: a quotation below `SND.UNA`, one exactly at `SND.NXT`, one for
accepted-but-unsent data and a valid one at `SND.UNA` on the same live tuple; a packet held in L3
resolution becoming eligible only on its `Sent` completion; a cancelled and an old-token completion
leaving the replacement flow's data unquotable; a wrapping send interval; a delayed quotation whose
start was partially acknowledged; a resegmented outstanding sequence taking two successively lower
reports; equal and increasing reports not refreshing the cache expiry; the 600-second expiry followed
by a replay that recreates no entry and moves no flow-local limit; a quotation of newly transmitted
data on the same tuple being accepted; and both the refused and the accepted case against a FULL
64-entry cache, proving `Capacity` is not a path around the flight check.

`tcp_flow/tests.rs` also gained `mod probes`: two routers answering two hops of one trace, each
matching only its own probe, in both families; a late error about a retired probe; a wrong
identifier, source, family or interface generation; and a quoted transport that is not an echo.

`addr_select/tests.rs` gained `mod rfc8028`, every case over M0174's bounded advertiser set with an
off-link destination: two advertisers differing only in preference (the higher wins, and it holds the
LARGER address so a fall-through to the tie-break would pick the other); an UNREACHABLE
high-preference advertiser losing to a reachable low-preference one; equal keys answering the same
way in both arrival orders; the selected advertiser expiring while another still advertises, where
the survivor is chosen rather than the general rules; every advertiser expiring while the prefix
remains, where the general rules DO apply and the fallback is recorded; the DIRECT case with a live
advertiser, where the on-link route wins outright and the default router and advertiser set stay
live; and a source from no learned prefix, which has no set to restrict to.

`net_profile/tests.rs` gained `mod both_families`: the diagnostic partition is ONE budget neither
family can double, releases are exactly once, a caller going away releases everything it held, an
invalidation after transmission fails in either family while an unsent one reselects and keeps its
deadline, a silent ping and probe hold their slots while a DNS query completes beside them on a
different partition, and the echo identity never repeats a sequence within one identifier.

`invalidation/tests.rs` covers the table one row per authority, with the two listener rows checked
against a real `BindTable` - the withdrawn listener's port is bindable again, in either shape.

THE GUEST ORACLES.

`src/harness/guest-console.py` (new). Half of what M10 asks to prove is guest-INITIATED - a name
resolved, a connection opened, a probe sent - and a peer can only answer. This connects to the
guest's serial socket, waits for the shell, types a script, and records the whole conversation in the
same shape `SERIAL=file:` produces, so every assertion the gate already made keeps reading one file.

`src/harness/ipv6-peer.py` gained TCP and UDP decoding over both families, an IPv6 and an IPv4 TCP
server, a DNS server over UDP and TCP with a name that is answered TRUNCATED over UDP and in full
over TCP, echo answering, Time Exceeded for expiring probes from a responder that is not the
destination, a black-holed address that answers neighbour solicitations and nothing else, and a SYN
flood in both families. Three scenarios use them: `transport`, `fallback` and `budgets`.

`src/tools/check-ipv6-peer.sh` gained four rows and a script-driven `row()`. `src/tools/
check-fallback-timing.py` (new) is the fallback row's clock.

ONE ROW HAD TO BE FIXED BEFORE IT WAS AN ORACLE AT ALL, and it is worth writing down because the
failure mode is the one a driven guest invites. `hostile-quote` asserts that a live flow SURVIVED a
misquoting error - which is a claim only if the error arrived while the flow was live. The console
types as fast as prompts appear, so on the first run every command had finished before the peer's
inbound connection was even attempted, and the peer's single SYN landed before the shell had started
the listener at all. Two changes: the peer RETRIES the inbound SYN every three seconds from a fresh
source port until the listener answers - guessing when a shell command lands is guessing how long a
boot took - and the guest script holds the shell for twenty seconds with an echo run that the error
then arrives in the middle of, which is itself asserted. The row now fails if the error is never
delivered, if the listener never accepts, or if any of the three live flows is disturbed.

WHAT WAS VERIFIED, AND HOW.

HOST FIXTURES. `cargo test --manifest-path src/user/services/logic/Cargo.toml --lib` - 406 passed,
0 failed (351 before this milestone). `cargo test --manifest-path
src/user/libs/protocol/network-proto/Cargo.toml --lib` - 54 passed, 0 failed.

THE BUILD. `./build.sh --arch x86_64` - clean, `providers=65/0`, no new unprovided cross-crate
import. One rustc SIGSEGV during an unrelated binary was retried and passed, which is the known
fault on this machine rather than a build failure.

THE GENERATED ABI. `./gen.sh --accept-breaking` after adding `address-unavailable` to
`liber:base@1`'s `error`. Sixteen packages regenerated; the two exhaustive matches over that enum
stopped the build and were both given answers rather than a wildcard.

THE GUEST SUITE. `./test.sh --arch x86_64` - 387 passed (202s). Unchanged in count from before this
milestone, which is the point: the IPv4 DHCP, DNS, TCP and tool tests are all in it.

THE STATIC GATES. `./check.sh --gate source-hygiene --gate host-tests --gate verify-model` - all
selected checks passed; the model is consistent at 131 crates, 271 components, 605 checks.

THE WIRE. `./image.sh --format iso --dma-mode enforcing-required` then `check-ipv6-peer.sh`.
All ten rows passed
(`gate=0`): the six that answer the guest, and the four that drive it. The fallback handover was
measured at 2.00 s in two separate runs; the flood row reported "it answered 8 of 96 over IPv6 and 8
of 96 over IPv4, and dropped the rest"; the misquoting row reported "the listener accepted an
inbound connection, and an error quoting another tuple changed nothing about it".

WHAT WAS MEASURED RATHER THAN ASSERTED. The fallback handover was timed on the peer's capture at
2.00 s between the black-holed candidate's SYN and the second candidate's - the cap is three seconds
and runs from the moment the candidate is STARTED, which includes its next-hop resolution, so the
gap on the wire is necessarily shorter. Without the cap the second attempt would be 183 seconds
later. The budgets row was checked by hand as well as by the gate: 96 SYNs in each family, the guest
answered exactly 8 in each - its listener's backlog - and dropped the other 88, then still answered
an echo in both families.

THE OTHER TWO ARCHITECTURES. `./build.sh --arch aarch64` and `./build.sh --arch riscv64` - both
clean, run last as the slower pair. Every change in this milestone is architecture-independent
user-space code and the generated protocol, so this was expected; it is recorded because expecting
it is not the same as having run it.

BLOCKERS, STATED RATHER THAN WORKED AROUND.

1. THE IPv6-ONLY QEMU ROW IS NOT BUILT, and cannot be with the machinery that exists. M10 requires
   that row to DISABLE IPv4 rather than accept an incidental DHCPv4 path. `net.families` is read once
   at NetworkService start from ConfigService, and nothing lets a harness set it for a boot: the ISO
   carries a read-only volume so the durable config tree cannot take a value into a second boot, and
   `config set` followed by `stop network_service` / `start network_service` cannot work either - the
   supervisor stops DEPENDENTS first, which takes the shell and therefore the console with it (this
   was tried; the guest halted). A families switch needs a boot-time configuration source the harness
   can set - a kernel command line the boot protocol carries, or an image-time seed - and that is a
   boot-protocol or image-assembly change rather than a networking one.

2. THE QEMU PMTU AND RESEGMENTATION ROW IS NOT BUILT. It requires the guest to have several segments
   of REAL outstanding data on the wire so a Packet Too Big can be observed to resegment them. No
   shell-reachable tool can put that much out: `nc`'s request is capped at 256 bytes by its own
   zero-copy buffer and it has no stdin path, and `httpd`'s canned response is smaller still. The
   same limitation blocks the 64-entry PMTU cache fill, which needs sixty-five destinations with
   outstanding data. The misquoting case is NOT blocked and was built - `hostile-quote` delivers a
   Packet Too Big naming a different tuple while three of the guest's flows are live, and asserts all
   three survive - but it proves the error changed nothing rather than that a VALID one would have
   resegmented, which is the half that needs outstanding bytes. The decision core both halves rest on
   has the complete host fixture matrix described above, against the production code with a
   controlled clock - but that is not the same evidence as the wire, and it is not claimed to be.
