//! The numbers the network contract is bounded by, in one place.
//!
//! WHY THESE ARE HERE AND NOT IN THE SERVICE. Every one of them is a decision two implementations
//! would make differently, and several of them are load-bearing on BOTH sides of the wire: a client
//! that believes a name may return sixteen addresses and a service that returns eight disagree about
//! whether a reply was truncated. The IDL carries the ones that are `@bound` annotations; these are
//! the ones an annotation cannot express - a framing capacity, a parser's work limit, a per-client
//! budget - and they belong beside the generated types rather than inside one consumer of them.
//!
//! THE FRAMING FOLLOWS THE BOUNDS. The wire may not advertise a request the service cannot receive
//! or a reply it cannot send, so `REQUEST_BYTES` and `REPLY_BYTES` are derived from the worst case
//! the contract permits - and `network-proto`'s own tests encode that worst case and check it fits.

/// The most destinations one `open-target` carries, matching its `@bound(8)`.
pub const MAX_OPEN_DESTINATIONS: usize = 8;

/// The most request bytes a `tcp-request` carries.
pub const MAX_REQUEST_BYTES: usize = 1024;

/// The most addresses one `resolve` answers with.
///
/// BEYOND IT THE REST ARE DROPPED, NOT REFUSED. The list is already ordered by the selection rules,
/// so the ones kept are the ones that would have been tried first; a name with nine addresses is a
/// well-provisioned name and not an error.
pub const MAX_RETURNED_ADDRESSES: usize = 8;

/// The most answer records parsed out of one DNS response. A response declaring more is a typed
/// refusal and the query is NOT retried against the same server, which is what stops a hostile
/// server costing an unbounded number of parses.
pub const MAX_ANSWER_RECORDS: usize = 32;

/// The most CNAME hops followed before the answer is refused as a loop or a chain too long to be a
/// real delegation.
pub const MAX_CNAME_CHAIN: usize = 8;

/// The most compression pointers followed while decoding ONE name.
///
/// EVERY POINTER MUST TARGET A STRICTLY EARLIER OFFSET, and that backward rule is what makes a loop
/// impossible rather than merely bounded; this count bounds the legal-but-absurd case that is still
/// an attack.
pub const MAX_COMPRESSION_JUMPS: usize = 16;

/// The most response bytes one `fetch` delivers, and the chunk its stream carries them in.
///
/// REACHING THE CAP IS NOT AN ENDING. A body of exactly this many bytes followed by an orderly close
/// is COMPLETE; truncation requires evidence of a further in-order byte, and that byte is never
/// delivered.
pub const MAX_FETCH_BODY_BYTES: usize = 262_144;
pub const FETCH_CHUNK_BYTES: usize = 4096;

/// Concurrent sockets, per client and across the service.
pub const MAX_SOCKETS_PER_CLIENT: usize = 16;
pub const MAX_SOCKETS_TOTAL: usize = 64;

/// In-flight DNS queries, per client and across the service.
pub const MAX_DNS_QUERIES_PER_CLIENT: usize = 4;
pub const MAX_DNS_QUERIES_TOTAL: usize = 16;

/// Unacknowledged TCP data, per flow and across the service.
pub const MAX_UNACKED_BYTES_PER_FLOW: usize = 256 * 1024;
pub const MAX_UNACKED_BYTES_TOTAL: usize = 4 * 1024 * 1024;

/// The list bounds a combined dual-stack snapshot of ONE interface may reach.
///
/// EACH ONE ADDS THE IPv4 VALUES THE STACK ALREADY HOLDS to the per-family IPv6 maxima. Copying only
/// the IPv6 maxima would overflow exactly when both families are full, which is the case worth
/// reporting rather than the case to lose.
pub const MAX_INTERFACE_ADDRESSES: usize = 17;
pub const MAX_ROUTES: usize = 34;
pub const MAX_ROUTERS: usize = 9;
pub const MAX_DNS_SERVERS: usize = 5;
pub const MAX_NEIGHBORS: usize = 1088;
pub const MAX_INTERFACE_NAME: usize = 16;
pub const MAX_HOST_NAME: usize = 253;
pub const MAX_SOCKET_ROWS: usize = 256;

/// The typed request and reply buffers one client call is framed in.
///
/// NOT ROUND NUMBERS CHOSEN FOR COMFORT. `REQUEST_BYTES` holds an eight-destination open-target
/// beside 1024 request bytes; `REPLY_BYTES` holds a `net-info` with every list full - the 1088
/// neighbours alone are the bulk of it - and a 256-row socket list of scoped endpoints. Both of
/// those exceed 4096, which is why the previous reply buffer could not survive this contract.
pub const REQUEST_BYTES: usize = 8192;
pub const REPLY_BYTES: usize = 65536;

#[cfg(test)]
mod tests;
