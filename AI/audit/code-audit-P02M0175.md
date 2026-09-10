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
