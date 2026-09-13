// The network driver's frame arithmetic, as pure decisions: how large a link's frames are, where a
// received one is, and what fits in a transmit buffer - tested on the host through the crate's seam.
//
// WHAT THE DEVICE CHOOSES HERE. The MTU comes from the device's own configuration space, and it sizes
// every receive slot and the one transmit buffer: an unbounded answer is an unbounded allocation
// asked for on the device's word, and a zero is a slot that cannot hold a header. The frame's own
// length and slot index come from the used ring, which the shared virtio path bounds - and what it
// bounds is the DESCRIPTOR, so the arithmetic that turns an index and a length into a pointer and a
// count is still this driver's to get right.

// The virtio_net_hdr prepended to every frame on both queues (VERSION_1).
pub const NET_HDR_LEN: u64 = 12;
// The Ethernet header that rides in front of the payload.
pub const ETHERNET_HEADER: u64 = 14;
// What a link carries when the device does not say: standard Ethernet.
pub const DEFAULT_MTU: u64 = 1500;
// The smallest MTU IPv4 requires a link to carry, which is the floor below which a link is not one.
pub const MIN_MTU: u64 = 68;
// The largest this driver will size its buffers for: a jumbo frame and its headers. A device that
// reports more is reporting a link this driver will not allocate for on its word alone.
pub const MAX_MTU: u64 = 9216;

// The link MTU to use, from what the device reported and whether it reported anything at all.
pub fn link_mtu(reported: Option<u64>) -> u64 {
	match reported {
		Some(mtu) if (MIN_MTU..=MAX_MTU).contains(&mtu) => mtu,
		_ => DEFAULT_MTU,
	}
}

// One receive slot's bytes: the virtio header, the Ethernet header and the payload.
pub fn slot_bytes(mtu: u64) -> u64 {
	NET_HDR_LEN + ETHERNET_HEADER + mtu
}

// Where a received frame is inside the pool, and how long it is - or `None` when the used element
// does not describe a frame.
//
// THREE WAYS IT IS NOT A FRAME: a slot index the pool does not have, a length that does not even
// cover the virtio header, and a length past the slot it claims to be in. The third is the one the
// shared ring check does not answer, because it bounds the DESCRIPTOR and this is the arithmetic on
// top of it.
pub fn received_frame(id: u16, len: u32, slots: u16, slot_bytes: u64) -> Option<(u64, usize)> {
	if id >= slots {
		return None;
	}
	let len = len as u64;
	if len <= NET_HDR_LEN || len > slot_bytes {
		return None;
	}
	let offset = (id as u64).checked_mul(slot_bytes)?.checked_add(NET_HDR_LEN)?;
	Some((offset, (len - NET_HDR_LEN) as usize))
}

// Whether a frame handed down for transmission fits the buffer behind its header.
pub fn transmit_fits(frame_len: usize, slot_bytes: u64) -> bool {
	frame_len > 0 && (frame_len as u64) <= slot_bytes.saturating_sub(NET_HDR_LEN)
}

#[cfg(test)]
mod tests;
