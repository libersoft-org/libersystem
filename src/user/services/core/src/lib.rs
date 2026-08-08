#![no_std]

extern crate alloc;

// The pure half lives in `service-logic` and is re-exported here, so `services::executable` and
// `services::shell_language` still name what they always named. The split is a testing one: this
// crate's binaries link `rt`, whose `panic_impl` collides with the `std` that `cargo test` needs,
// and that made four suites of pure logic unrunnable on the host. Moving them out is what lets
// `./check.sh --gate host-tests` reach them.
pub use service_logic::{executable, graph_limits, service_lifecycle, shell_language};

// What a development agent says on the resolution channel when it takes it, and the only
// thing on that channel that is not a query or an answer to one. It lives here because both
// ends have to agree on it: the agent sends it so ProcessService knows the channel has someone
// on it, and ProcessService recognises it so an announcement arriving mid-query - which is
// what a restarted agent does - is not mistaken for the answer to that query.
pub const REGISTRY_ANNOUNCEMENT: &[u8] = b"AGENT";
