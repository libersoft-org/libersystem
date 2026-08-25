// The capability TCB, exercised through the real syscalls rather than through its Rust API.
//
// `object::tests` drives `HandleTable` and `Channel` directly, which is where the model's actions
// are and what the trace records. This drives the SYSCALLS: two threads sharing one handle table
// and one endpoint pair, user buffers that stop being there partway through a copy, a destination at
// its quota, a queue that is full and a peer that has closed. What it asserts afterwards is what the
// model says must hold - identities, rights, live handles, bookings, queue depth and the Domain's
// charges - measured from outside the operation that was supposed to preserve them.
//
// QEMU IS CONFORMANCE EVIDENCE, NOT STATE EXPLORATION. Nothing here explores an interleaving space;
// it drives the paths the model describes and checks the state they leave behind.

// THE WHOLE MODULE IS THE SUITE, which is why it is a directory with one `tests.rs` in it rather
// than a single file. The gates that exempt test code - `kernel-allocations`, `frame-retirement` -
// recognise it by the file being named `tests.rs`, and a test-only module named anything else is
// held to the rules production code is held to: no infallible allocation, no plain free of a frame
// that was mapped. Those rules are right for the kernel and wrong for a fixture that owns its own
// machine, and the convention is how the difference is stated.

#[cfg(test)]
mod tests;
