// Boot-mode checks use the production volume services and their normal generated clients.
// A report emitted by a lazy mount is insufficient evidence that its routed medium can be read.
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, volume};
use rt::*;

unsafe fn read_fixture(root: u64, path: &str) -> Option<Vec<u8>> {
	unsafe {
		let connection = service_connect(root)?;
		let opened = volume::Client::new(ChannelTransport { chan: connection }).open(&OpenOpts { path: String::from(path), write: false, create: false });
		close(connection);
		let Some(Ok(file)) = opened else { return None };
		let Some(base) = map_object(file.file) else {
			close(file.file);
			return None;
		};
		let bytes = core::slice::from_raw_parts(base as *const u8, file.size as usize).to_vec();
		unmap_object(file.file);
		close(file.file);
		Some(bytes)
	}
}

pub(super) unsafe fn routed_volumes(system: u64, media: u64, iso: u64, udf: u64, usb: u64) -> bool {
	unsafe {
		let Some(expected) = read_fixture(system, "vol://system/hello.txt") else { return false };
		if expected.is_empty() {
			return false;
		}
		[(media, "vol://media/hello.txt"), (iso, "vol://iso/hello.txt"), (udf, "vol://udf/hello.txt"), (usb, "vol://usb/hello.txt")].iter().all(|(root, path)| read_fixture(*root, path).as_ref() == Some(&expected))
	}
}
