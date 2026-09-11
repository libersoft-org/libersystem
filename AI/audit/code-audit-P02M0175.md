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
