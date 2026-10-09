//! Backend-owned buffer bytes. Software execution borrows these through `frame::Source` until
//! it has completed; an outstanding read or map therefore excludes mutation and destruction.
//! A future pending backend must retain that ownership until its terminal completion, rather
//! than retaining only the resource's integer name.

use alloc::vec::Vec;
use core::ops::{Deref, DerefMut, Range};
use render3d::{BufferDesc, Error, HostVisibility, Render3DLimits};

/// Storage for a validated buffer. Destruction is ordinary Rust ownership: dropping the buffer
/// releases its bytes, and cannot happen while a map or an executing Source borrows them.
// @handles: Buffer
pub struct Buffer {
	desc: BufferDesc,
	bytes: Vec<u8>,
}

impl Buffer {
	pub fn new(desc: BufferDesc, limits: &Render3DLimits) -> Result<Self, Error> {
		desc.validate(limits)?;
		let size = usize::try_from(desc.size).map_err(|_| Error::OutOfMemory { bytes: desc.size })?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(size).map_err(|_| Error::OutOfMemory { bytes: desc.size })?;
		bytes.resize(size, 0);
		Ok(Self { desc, bytes })
	}

	pub fn description(&self) -> BufferDesc {
		self.desc
	}

	fn range(&self, range: Range<u64>) -> Result<Range<usize>, Error> {
		if range.start > range.end || range.end > self.desc.size {
			return Err(Error::InvalidRenderState { reason: "a buffer access lies outside its storage" });
		}
		Ok(range.start as usize..range.end as usize)
	}

	fn extent(&self, offset: u64, length: u64) -> Result<Range<usize>, Error> {
		let end = offset.checked_add(length).ok_or(Error::InvalidRenderState { reason: "a buffer access overflows its address range" })?;
		self.range(offset..end)
	}

	/// Write an upload into a declared copy destination. Validation precedes every changed byte.
	/// A Source/map borrowing this buffer makes this mutable operation unavailable until released.
	pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Error> {
		if !self.desc.usage.copy_destination {
			return Err(Error::InvalidRenderState { reason: "write_buffer requires copy-destination usage" });
		}
		let range = self.extent(offset, bytes.len() as u64)?;
		self.bytes[range].copy_from_slice(bytes);
		Ok(())
	}

	pub fn copy_from(&mut self, destination: u64, source: &Buffer, offset: u64, length: u64) -> Result<(), Error> {
		if !self.desc.usage.copy_destination || !source.desc.usage.copy_source {
			return Err(Error::InvalidRenderState { reason: "a buffer copy requires source and destination usages" });
		}
		let destination = self.extent(destination, length)?;
		let source_range = source.extent(offset, length)?;
		self.bytes[destination].copy_from_slice(&source.bytes[source_range]);
		Ok(())
	}

	/// A copy within one resource has memmove semantics, including overlapping ranges.
	pub fn copy_within(&mut self, destination: u64, source: Range<u64>) -> Result<(), Error> {
		if !self.desc.usage.copy_source || !self.desc.usage.copy_destination {
			return Err(Error::InvalidRenderState { reason: "a buffer copy requires source and destination usages" });
		}
		let source = self.range(source)?;
		let destination = self.extent(destination, source.len() as u64)?;
		self.bytes.copy_within(source, destination.start);
		Ok(())
	}

	/// Map an upload buffer, even when it is a vertex/uniform resource without copy usage.
	/// The guard is the exclusive ownership of the map; `unmap` or dropping it releases that borrow.
	///
	/// ```compile_fail
	/// # use soft3d::buffer::Buffer;
	/// # fn example(buffer: &mut Buffer) {
	/// let mut map = buffer.map_write(0..4).unwrap();
	/// buffer.write(0, &[1, 2, 3, 4]).unwrap(); // still mapped
	/// map[0] = 9;
	/// # }
	/// ```
	pub fn map_write(&mut self, range: Range<u64>) -> Result<WriteMap<'_>, Error> {
		if self.desc.host_visibility != HostVisibility::Upload {
			return Err(Error::InvalidRenderState { reason: "only upload buffers may be mapped for writing" });
		}
		let range = self.range(range)?;
		Ok(WriteMap(&mut self.bytes[range]))
	}

	pub fn map_read(&self, range: Range<u64>) -> Result<ReadMap<'_>, Error> {
		if self.desc.host_visibility != HostVisibility::Readback {
			return Err(Error::InvalidRenderState { reason: "only readback buffers may be mapped for reading" });
		}
		Ok(ReadMap(&self.bytes[self.range(range)?]))
	}

	/// Device-side views used by Source implementations. Host visibility does not restrict a
	/// shader read; the declared binding usage does. The view retains the backing storage borrow.
	pub fn vertex_bytes(&self, range: Range<u64>) -> Result<&[u8], Error> {
		self.binding_bytes(self.desc.usage.vertex, range)
	}

	pub fn index_bytes(&self, range: Range<u64>) -> Result<&[u8], Error> {
		self.binding_bytes(self.desc.usage.index, range)
	}

	pub fn uniform_bytes(&self, range: Range<u64>) -> Result<&[u8], Error> {
		self.binding_bytes(self.desc.usage.uniform, range)
	}

	pub fn storage_bytes(&self, range: Range<u64>) -> Result<&[u8], Error> {
		self.binding_bytes(self.desc.usage.storage, range)
	}

	fn binding_bytes(&self, permitted: bool, range: Range<u64>) -> Result<&[u8], Error> {
		if !permitted {
			return Err(Error::InvalidRenderState { reason: "a buffer binding requires its declared usage" });
		}
		Ok(&self.bytes[self.range(range)?])
	}
}

pub struct WriteMap<'a>(&'a mut [u8]);
impl WriteMap<'_> {
	pub fn unmap(self) {}
}
impl Deref for WriteMap<'_> {
	type Target = [u8];
	fn deref(&self) -> &[u8] {
		self.0
	}
}
impl DerefMut for WriteMap<'_> {
	fn deref_mut(&mut self) -> &mut [u8] {
		self.0
	}
}

pub struct ReadMap<'a>(&'a [u8]);
impl ReadMap<'_> {
	pub fn unmap(self) {}
}
impl Deref for ReadMap<'_> {
	type Target = [u8];
	fn deref(&self) -> &[u8] {
		self.0
	}
}

#[cfg(test)]
#[path = "buffer/tests.rs"]
mod tests;
