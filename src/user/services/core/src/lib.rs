#![no_std]

extern crate alloc;

pub mod executable;
pub mod graph_limits;
pub mod service_lifecycle;
pub mod shell_language;

// What a development agent says on the resolution channel when it takes it, and the only
// thing on that channel that is not a query or an answer to one. It lives here because both
// ends have to agree on it: the agent sends it so ProcessService knows the channel has someone
// on it, and ProcessService recognises it so an announcement arriving mid-query - which is
// what a restarted agent does - is not mistaken for the answer to that query.
pub const REGISTRY_ANNOUNCEMENT: &[u8] = b"AGENT";
