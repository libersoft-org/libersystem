use super::*;
use render3d::BufferUsage;
use render3d::submission::{Queue, Status};

fn description(visibility: HostVisibility) -> BufferDesc {
	BufferDesc { size: 16, usage: BufferUsage { vertex: true, index: true, uniform: true, storage: true, copy_source: true, copy_destination: true }, host_visibility: visibility }
}

fn buffer(visibility: HostVisibility) -> Buffer {
	Buffer::new(description(visibility), &Render3DLimits::PROFILE_MINIMUM).unwrap()
}

#[test]
fn mapped_upload_and_device_copies_reach_readback_and_shader_bindings() {
	let mut upload = buffer(HostVisibility::Upload);
	let mut mapped = upload.map_write(4..12).unwrap();
	mapped.copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
	mapped.unmap();
	let mut device = buffer(HostVisibility::None);
	device.copy_from(0, &upload, 4, 8).unwrap();
	device.write(8, &[9, 10, 11, 12]).unwrap();
	assert_eq!(device.vertex_bytes(0..4).unwrap(), &[1, 2, 3, 4]);
	assert_eq!(device.index_bytes(4..8).unwrap(), &[5, 6, 7, 8]);
	assert_eq!(device.uniform_bytes(8..12).unwrap(), &[9, 10, 11, 12]);
	assert_eq!(device.storage_bytes(12..16).unwrap(), &[0; 4]);
	device.copy_within(2, 0..8).unwrap();
	let mut readback = buffer(HostVisibility::Readback);
	readback.copy_from(0, &device, 0, 16).unwrap();
	let mapped = readback.map_read(0..12).unwrap();
	assert_eq!(&*mapped, &[1, 2, 1, 2, 3, 4, 5, 6, 7, 8, 11, 12]);
	mapped.unmap();
}

#[test]
fn refused_ranges_and_usage_preserve_destination_bytes() {
	let mut destination = buffer(HostVisibility::Readback);
	destination.write(0, &[19; 16]).unwrap();
	let source = buffer(HostVisibility::None);
	assert!(destination.write(u64::MAX, &[1]).is_err());
	assert!(destination.write(15, &[1, 2]).is_err());
	assert!(destination.copy_from(0, &source, 15, 2).is_err());
	assert!(destination.copy_from(15, &source, 0, 2).is_err());
	assert!(destination.copy_within(0, 9..3).is_err());
	assert!(destination.copy_within(15, 0..2).is_err());
	assert!(destination.map_read(0..17).is_err());
	assert!(destination.map_write(0..1).is_err());
	assert!(source.map_read(0..1).is_err());
	assert_eq!(&*destination.map_read(0..16).unwrap(), &[19; 16]);
	let desc = BufferDesc { usage: BufferUsage { vertex: true, ..BufferUsage::default() }, ..description(HostVisibility::Upload) };
	let mut vertex = Buffer::new(desc, &Render3DLimits::PROFILE_MINIMUM).unwrap();
	vertex.map_write(0..1).unwrap()[0] = 7;
	assert!(vertex.write(0, &[9]).is_err());
	assert!(vertex.uniform_bytes(0..1).is_err());
	assert!(destination.copy_from(0, &vertex, 0, 1).is_err());
	assert_eq!(vertex.vertex_bytes(0..1).unwrap(), &[7]);
	assert_eq!(&*destination.map_read(0..16).unwrap(), &[19; 16]);
}

#[test]
fn storage_allocation_refusal_is_typed() {
	let result = crate::counted::fail_after(0, || Buffer::new(description(HostVisibility::None), &Render3DLimits::PROFILE_MINIMUM));
	assert!(matches!(result, Err(Error::OutOfMemory { bytes: 16 })));
}

/// The profile permits a pending test backend. Unlike an ID-only Queue test this backend owns
/// actual bytes, and the caller transfers them at submission. All host mutations check ownership
/// before touching those bytes; terminal submissions release in the production Queue's order.
struct Pending {
	queue: Queue,
	buffers: [Option<Buffer>; 2],
	serials: [u64; 2],
}

impl Pending {
	fn new(first: Buffer, second: Buffer) -> Self {
		let mut queue = Queue::with_capacity(2).unwrap();
		let serials = [queue.submit(0, &[0]).unwrap(), queue.submit(0, &[1]).unwrap()];
		Self { queue, buffers: [Some(first), Some(second)], serials }
	}

	fn write(&mut self, id: usize, bytes: &[u8]) -> Result<(), Error> {
		render3d::resource::refuse_write_in_flight(self.queue.owned_by_a_submission(id as u32))?;
		self.buffers[id].as_mut().unwrap().write(0, bytes)
	}

	fn settle(&mut self, index: usize, status: Status) -> [Option<Buffer>; 2] {
		let released = self.queue.settle(self.serials[index], status).unwrap();
		let mut buffers = [None, None];
		for id in released {
			buffers[id as usize] = self.buffers[id as usize].take();
		}
		buffers
	}
}

impl Drop for Pending {
	fn drop(&mut self) {
		// Backend loss is terminal before its owned Buffer fields are destroyed.
		self.queue.lose_backend();
	}
}

#[test]
fn pending_backend_retains_real_buffers_until_ordered_terminal_completion() {
	let mut first = buffer(HostVisibility::Readback);
	let mut second = buffer(HostVisibility::Readback);
	first.write(0, &[42; 16]).unwrap();
	second.write(0, &[73; 16]).unwrap();
	let mut backend = Pending::new(first, second); // caller ownership moves into the submissions
	assert!(backend.write(0, &[99]).is_err());
	assert!(backend.write(1, &[99]).is_err());
	assert!(backend.settle(1, Status::Complete).iter().all(Option::is_none));
	assert!(backend.write(1, &[99]).is_err(), "completion behind an earlier pending submission retains bytes");
	let [first, second] = backend.settle(0, Status::Cancelled);
	let mut first = first.unwrap();
	let second = second.unwrap();
	assert_eq!(&*first.map_read(0..16).unwrap(), &[42; 16]);
	assert_eq!(&*second.map_read(0..16).unwrap(), &[73; 16]);
	first.write(0, &[99]).unwrap();
	assert_eq!(&*first.map_read(0..1).unwrap(), &[99]);
	assert_eq!(backend.queue.in_flight(), 0);
}
