//! THE BLUETOOTH STACK'S BOUNDS, stated before any code that relies on them, and the Domain derived from them.
//!
//! Every table the service keeps is sized here, and the storage the worst case of all of them together needs is
//! COMPUTED from the same numbers rather than written beside them - so a bound raised here raises the derivation, and a
//! derivation past the stated budget fails its test instead of passing silently.

/// Controllers the service drives at once.
pub const CONTROLLERS: usize = 2;
/// ACL links per controller across both radios; the controller's own smaller count wins.
pub const LINKS_PER_CONTROLLER: usize = 8;
/// Of those, BR/EDR links: a piconet's active members.
pub const BREDR_LINKS_PER_CONTROLLER: usize = 7;
/// L2CAP channels per link.
pub const CHANNELS_PER_LINK: usize = 16;
/// Of those, channels in enhanced retransmission mode.
pub const ERTM_CHANNELS_PER_LINK: usize = 2;
/// RFCOMM channels in a link's one RFCOMM session.
pub const RFCOMM_CHANNELS_PER_LINK: usize = 8;
/// ACL packets queued per link.
pub const QUEUED_ACL_PER_LINK: usize = 32;
/// The MTU of every BR/EDR dynamic channel: BNEP's minimum, above what AVDTP, AVCTP, HID, RFCOMM and OBEX need.
pub const BREDR_MTU: usize = 1691;
/// OBEX's maximum packet, offered as the same.
pub const OBEX_PACKET: usize = BREDR_MTU;
/// The ATT MTU on LE: above BAP's minimum of 64, and one Data Length Extension PDU with its L2CAP header.
pub const ATT_MTU: usize = 247;
/// Incomplete SDUs per channel, and how long one may take to assemble.
pub const INCOMPLETE_SDUS_PER_CHANNEL: usize = 1;
pub const ASSEMBLY_DEADLINE_MS: u64 = 1000;
/// Enhanced retransmission: the transmit window and the specification's default time-outs.
pub const ERTM_WINDOW: usize = 8;
pub const ERTM_RETRANSMISSION_MS: u64 = 2000;
pub const ERTM_MONITOR_MS: u64 = 12000;
/// Synchronous links: one SCO/eSCO link per controller; four connected isochronous streams; one broadcast group of
/// at most two streams.
pub const SCO_LINKS_PER_CONTROLLER: usize = 1;
pub const CONNECTED_ISO_STREAMS: usize = 4;
pub const BROADCAST_GROUPS: usize = 1;
pub const BROADCAST_STREAMS: usize = 2;
/// The ISO transport ceiling.
pub const ISO_CEILING: usize = 4100;
/// The largest ACL packet the transport carries, header included.
pub const ACL_PACKET: usize = 1028;
/// Client connections and minted grants.
pub const CLIENT_CONNECTIONS: usize = 16;
pub const MINTED_GRANTS: usize = 32;
/// Bonds: records, and the store's size.
pub const BOND_RECORDS: usize = 64;
pub const BOND_STORE_BYTES: usize = 64 * 1024;
/// A bond record, keys and trust bits included, at most.
pub const BOND_RECORD_BYTES: usize = 200;
/// The aggregate packet storage, shared by both controllers.
pub const PACKET_STORAGE_BYTES: usize = 1_500_000;

/// The links of both controllers together.
pub const fn links() -> usize {
	CONTROLLERS * LINKS_PER_CONTROLLER
}

/// THE WORST CASES OF EVERY LINK TOGETHER, in bytes: reassembly (one incomplete SDU on every channel of every link),
/// the ERTM windows (two directions of every ERTM channel), and the queued ACL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Storage {
	pub reassembly: usize,
	pub ertm_windows: usize,
	pub queued_acl: usize,
}

impl Storage {
	pub const fn total(&self) -> usize {
		self.reassembly + self.ertm_windows + self.queued_acl
	}
}

pub const fn storage() -> Storage {
	Storage { reassembly: links() * CHANNELS_PER_LINK * INCOMPLETE_SDUS_PER_CHANNEL * BREDR_MTU, ertm_windows: links() * ERTM_CHANNELS_PER_LINK * ERTM_WINDOW * BREDR_MTU, queued_acl: links() * QUEUED_ACL_PER_LINK * ACL_PACKET }
}

/// THE DOMAIN: memory, handles, threads and stacks stay; the IPC queues grow for the standing queues a PAN link's
/// frames and four audio endpoints' periods need.
pub const DOMAIN_MEMORY: usize = 64 << 20;
pub const DOMAIN_HANDLES: usize = 256;
pub const DOMAIN_THREADS: usize = 4;
pub const DOMAIN_STACK: usize = 2 << 20;
pub const DOMAIN_IPC_QUEUES: usize = 4 << 20;
/// A PAN link's standing frames each way, and an audio endpoint's periods each way.
pub const PAN_FRAMES: usize = 64;
pub const AUDIO_ENDPOINTS: usize = 4;
pub const AUDIO_PERIODS: usize = 16;
/// The bytes of one PAN frame on the wire: the Ethernet frame and its header.
pub const PAN_FRAME_BYTES: usize = 1514;
/// One audio period at its largest: 10 ms of 48 kHz stereo 16-bit.
pub const AUDIO_PERIOD_BYTES: usize = 1920;

/// What the new standing queues hold at most.
pub const fn standing_queues() -> usize {
	2 * PAN_FRAMES * PAN_FRAME_BYTES + 2 * AUDIO_ENDPOINTS * AUDIO_PERIODS * AUDIO_PERIOD_BYTES
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_worst_case_storage_is_derived_and_inside_the_budget() {
		let worst = storage();
		assert_eq!(worst.reassembly, 16 * 16 * 1691, "16 x 16 x 1691");
		assert_eq!(worst.ertm_windows, 16 * 2 * 8 * 1691, "16 x 2 x 8 x 1691");
		assert_eq!(worst.queued_acl, 16 * 32 * 1028, "16 x 32 x 1028");
		assert_eq!(worst.reassembly, 432_896);
		assert_eq!(worst.ertm_windows, 432_896);
		assert_eq!(worst.queued_acl, 526_336);
		assert!(worst.total() <= PACKET_STORAGE_BYTES, "about 1.39 MB inside 1.5 MB");
		assert_eq!(worst.total(), 1_392_128);
	}

	#[test]
	fn the_standing_queues_fit_the_queue_budget_beside_the_old_one() {
		assert!(standing_queues() < DOMAIN_IPC_QUEUES - (2 << 20), "inside the two megabytes the queues grew by");
		assert!(BOND_RECORDS * BOND_RECORD_BYTES <= BOND_STORE_BYTES);
		assert!(BREDR_LINKS_PER_CONTROLLER < LINKS_PER_CONTROLLER);
		assert!(ATT_MTU >= 64 && BREDR_MTU >= 672);
	}
}
