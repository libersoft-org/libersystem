//! The decisions CoreServices makes that depend on nothing but their inputs.
//!
//! Parsing a shell line, deciding whether an artifact name is a legal executable, bounding a module
//! graph, ordering a service shutdown: all of them are functions of their arguments, and none of
//! them needs a running system to be judged. They lived inside the `services` crate, which builds
//! twenty-odd binaries against the freestanding runtime, and `cargo test` therefore could not build
//! them at all - so the only thing that ever exercised them was a QEMU boot.

#![no_std]

extern crate alloc;

pub mod addr_select;
pub mod admin_broker;
pub mod admin_descriptor;
pub mod admin_journal;
pub mod aes;
pub mod att;
pub mod bond_store;
pub mod bounded_event;
pub mod camera_streams;
pub mod card_slots;
pub mod catalogue_scope;
pub mod cmac;
pub mod dhcp;
pub mod display_lock;
pub mod dns;
pub mod executable;
pub mod font_clients;
pub mod font_record;
pub mod font_rescan;
pub mod font_scan;
pub mod gatt_mouse;
pub mod graph_limits;
pub mod hci;
pub mod hci_codec;
pub mod hogp;
pub mod invalidation;
pub mod ipv6;
pub mod ipv6_budget;
pub mod ipv6_events;
pub mod ipv6_icmp;
pub mod ipv6_mld;
pub mod ipv6_nd;
pub mod ipv6_neighbour;
pub mod ipv6_packet;
pub mod ipv6_quote;
pub mod ipv6_route;
pub mod ipv6_router;
pub mod ipv6_slaac;
pub mod ipv6_solicit;
pub mod ipv6_timers;
pub mod l2cap;
pub mod media_import;
pub mod modem_commands;
pub mod net_profile;
pub mod open_sequence;
pub mod piv;
pub mod power_registry;
pub mod printer_status;
pub mod ptp;
pub mod raw_ip;
pub mod selection;
pub mod service_lifecycle;
pub mod shell_language;
pub mod smp;
pub mod smp_pairing;
pub mod sntp;
pub mod spool_jobs;
pub mod tcp_admission;
pub mod tcp_bind;
pub mod tcp_close;
pub mod tcp_flow;
pub mod tcp_queue;
pub mod tcp_rto;
pub mod tcp_transmit;
pub mod tcp_window;
pub mod trusted_keys;
pub mod uplink;
pub mod world_errors;
