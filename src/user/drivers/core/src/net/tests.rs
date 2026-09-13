// The network driver's parser layer, held against the decisions it makes on the device's numbers.
use super::{DEFAULT_MTU, MAX_MTU, MIN_MTU, NET_HDR_LEN, link_mtu, received_frame, slot_bytes, transmit_fits};

#[test]
// THE MTU SIZES EVERY BUFFER THIS DRIVER ALLOCATES, and it comes from the device's own configuration
// space. A zero was refused and everything else was believed: sixty-five thousand gives a pool of
// half a megabyte asked for on the device's word, and a link that claims to carry four bytes is not
// a link.
fn the_link_mtu_is_bounded_rather_than_believed() {
	assert_eq!(link_mtu(Some(1500)), 1500);
	assert_eq!(link_mtu(Some(9000)), 9000, "a jumbo link is a real link");
	assert_eq!(link_mtu(None), DEFAULT_MTU, "a device that reports no MTU gets standard Ethernet");
	assert_eq!(link_mtu(Some(0)), DEFAULT_MTU, "a zero was always refused");
	assert_eq!(link_mtu(Some(MIN_MTU - 1)), DEFAULT_MTU, "below the floor IPv4 requires");
	assert_eq!(link_mtu(Some(MAX_MTU + 1)), DEFAULT_MTU);
	assert_eq!(link_mtu(Some(u64::MAX)), DEFAULT_MTU);
	// And the slot follows the MTU, with both headers in front of the payload.
	assert_eq!(slot_bytes(1500), NET_HDR_LEN + 14 + 1500);
}

#[test]
// A USED ELEMENT IS AN INDEX AND A LENGTH, and the arithmetic that turns them into a pointer and a
// count is this driver's. The shared ring check bounds the DESCRIPTOR; a length past the slot it
// claims to be in is what is left, and it reads into the next slot of the pool.
fn a_used_element_that_does_not_describe_a_frame_is_refused() {
	let slot = slot_bytes(1500);
	assert_eq!(received_frame(0, 12 + 64, 8, slot), Some((NET_HDR_LEN, 64)));
	assert_eq!(received_frame(3, 12 + 64, 8, slot), Some((3 * slot + NET_HDR_LEN, 64)));
	// The three ways it is not a frame.
	assert_eq!(received_frame(8, 100, 8, slot), None, "a slot the pool does not have");
	assert_eq!(received_frame(0, 12, 8, slot), None, "a length that is the header and nothing else");
	assert_eq!(received_frame(0, 0, 8, slot), None);
	assert_eq!(received_frame(0, slot as u32 + 1, 8, slot), None, "a length past the slot reads into the next one");
	// The exact boundary is a frame, because a full slot is what a maximum-sized frame fills.
	assert_eq!(received_frame(7, slot as u32, 8, slot), Some((7 * slot + NET_HDR_LEN, (slot - NET_HDR_LEN) as usize)));
}

#[test]
// A FRAME THAT DOES NOT FIT IS DROPPED RATHER THAN TRUNCATED, and an empty one is not a frame.
fn a_transmitted_frame_has_to_fit_behind_its_header() {
	let slot = slot_bytes(1500);
	assert!(transmit_fits(64, slot));
	assert!(transmit_fits((slot - NET_HDR_LEN) as usize, slot), "a maximum-sized frame fills the buffer");
	assert!(!transmit_fits((slot - NET_HDR_LEN) as usize + 1, slot));
	assert!(!transmit_fits(0, slot), "an empty frame is not a frame");
	assert!(!transmit_fits(1, 4), "a buffer smaller than the header holds nothing");
}
