//! The decisions CoreServices makes that depend on nothing but their inputs.
//!
//! Parsing a shell line, deciding whether an artifact name is a legal executable, bounding a module
//! graph, ordering a service shutdown: all of them are functions of their arguments, and none of
//! them needs a running system to be judged. They lived inside the `services` crate, which builds
//! twenty-odd binaries against the freestanding runtime, and `cargo test` therefore could not build
//! them at all - so the only thing that ever exercised them was a QEMU boot.

#![no_std]

extern crate alloc;

pub mod activity;
pub mod addr_select;
pub mod admin_broker;
pub mod admin_descriptor;
pub mod admin_journal;
pub mod aes;
pub mod ascs;
pub mod att;
pub mod audio_routing;
pub mod avdtp;
pub mod avrcp;
pub mod bap;
pub mod bnep;
pub mod bond_store;
pub mod bounded_event;
pub mod brightness;
pub mod bt_bounds;
pub mod bt_keys;
pub mod bt_pairing;
pub mod bt_policy;
pub mod camera_streams;
pub mod cap;
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
pub mod gtbs;
pub mod hci;
pub mod hci_bredr;
pub mod hci_codec;
pub mod hfp;
pub mod hibernation;
pub mod hogp;
// HID OVER GATT WITH ARBITRARY REPORT MAPS: the walk that finds a device's report map, its input reports and their
// ids, and turns them on.
pub mod hogp_map;
// THE GATT SERVER this host holds: GAP and GATT, answered to a peer acting as a client.
pub mod gatt_server;
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
pub mod l2cap_bredr;
pub mod lc3;
pub mod le_audio;
pub mod le_iso;
pub mod media_import;
pub mod modem_commands;
pub mod net_profile;
pub mod obex;
pub mod open_sequence;
pub mod piv;
pub mod power_registry;
pub mod printer_status;
pub mod processor_policy;
pub mod provider_retry;
pub mod ptp;
pub mod raw_ip;
pub mod rfcomm;
pub mod sbc;
pub mod sdp;
pub mod selection;
pub mod service_lifecycle;
pub mod sha256;
pub mod shell_language;
pub mod sleep_order;
pub mod sleep_policy;
pub mod sleep_transaction;
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
pub mod tpm;
pub mod trusted_keys;
pub mod typec_requests;
pub mod uplink;
pub mod watchdog;
pub mod world_errors;
