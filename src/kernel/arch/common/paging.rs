// Portable page-table permission flags - the flag set every arch's `map_page`
// accepts.
//
// These are ABSTRACT permission bits the portable kernel ORs together and passes to
// `arch::paging::map_page`; each backend maps them onto its real hardware encoding
// (x86 uses these values directly as the PTE bits; aarch64 / riscv64 translate them
// to the VMSAv8 / Sv48 descriptor bits inside `map_page`). Keeping them in one place
// makes the `arch::paging` permission contract a single source of truth, so a new
// architecture re-exports these names instead of re-declaring them.

// The values coincide with the x86-64 PTE bit positions (PRESENT = bit 0, WRITABLE =
// bit 1, USER = bit 2, PCD/no-cache = bit 4, NX = bit 63), so the x86 backend can use
// them as hardware bits verbatim; other backends only rely on the distinct bit
// pattern, not the positions.
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4;
pub const NO_EXECUTE: u64 = 1 << 63;
