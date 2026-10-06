// btserial - the BR/EDR gate's application: a serial port on one aliased peer, through the grant PermissionManager
// mints for it and nothing else. DEVELOPMENT-ONLY.
//
// Its policy row names the peer aliased `serial-1`: the fixture's serial device, which echoes what it receives. It
// proves the grant is a byte stream with backpressure: refused before it connects; connected through the stored key,
// an SDP search and RFCOMM; a line written and read back whole; a burst larger than the DLC's queue taken in parts,
// `again` where the peer's credits are spent, and every byte of it read back in order while the reader holds the peer;
// and closed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, bluetooth_serial};
use rt::*;

const TICKS: u64 = 100;

fn fail(line: &str) -> ! {
	print(format!("btserial: FAIL {line}\n").as_bytes());
	exit_with(1);
}

fn say(line: &str) {
	print(format!("btserial: {line}\n").as_bytes());
}

// What the stream delivers until `want` bytes have arrived, or the deadline.
fn read_until(stream: u64, got: &mut Vec<u8>, want: usize, ticks: u64) {
	let deadline = clock() + ticks;
	let mut buf = [0u8; 1200];
	while got.len() < want {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, handles } => {
				let mut handles = handles;
				if let Some(piece) = bluetooth_serial::read_read(&buf[..len], &mut handles) {
					got.extend_from_slice(&piece.bytes);
				}
				for &leftover in handles.as_slice() {
					close(leftover);
				}
			}
			PolledCaps::Empty => {
				if clock() >= deadline {
					return;
				}
				let _ = wait(stream, deadline);
			}
			PolledCaps::Closed => return,
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 128];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let grant = recv_tagged(bootstrap, &mut buf, b"BTSERIAL").unwrap_or(0);
	if grant == 0 {
		fail("no serial port grant was delivered");
	}
	let client = || bluetooth_serial::Client::new(ChannelTransport { chan: grant });
	if !matches!(client().write(&b"early".to_vec()), Some(Err(Error::Closed))) {
		fail("a write before the port was connected was not refused closed");
	}
	match client().connect() {
		Some(Ok(())) => {}
		other => fail(&format!("the port did not connect: {other:?}")),
	}
	let stream = match client().read() {
		Some(Ok(stream)) => stream,
		other => fail(&format!("the read stream was refused: {other:?}")),
	};
	if !matches!(client().read(), Some(Err(Error::Again))) {
		fail("a second read stream was not refused");
	}
	say("the port connected through the grant, and one read stream is all it has");

	// A LINE, AND ITS ECHO.
	let line = b"hello over a Bluetooth serial port\n".to_vec();
	match client().write(&line) {
		Some(Ok(taken)) if taken as usize == line.len() => {}
		other => fail(&format!("the line was not taken whole: {other:?}")),
	}
	let mut echoed = Vec::new();
	read_until(stream, &mut echoed, line.len(), 5 * TICKS);
	if echoed != line {
		fail(&format!("the echo was not the line: {} bytes", echoed.len()));
	}
	say("a line written came back whole");

	// A BURST larger than the DLC's queue: taken in parts, `again` while the peer's credits are spent, and every byte
	// read back in order.
	// LARGER THAN EVERYTHING BETWEEN THE TWO ENDS - the DLC's queue, the device's credits and its echo backlog, this
	// grant's stream - so the writer meets the backpressure.
	let burst: Vec<u8> = (0..64_000u32).map(|n| b'a' + (n % 26) as u8).collect();
	let (mut offset, mut again, mut writes) = (0usize, 0u32, 0u32);
	let mut back = Vec::new();
	let deadline = clock() + 40 * TICKS;
	while offset < burst.len() {
		if clock() >= deadline {
			fail(&format!("the burst stalled at {offset} bytes"));
		}
		let end = (offset + 1024).min(burst.len());
		match client().write(&burst[offset..end].to_vec()) {
			Some(Ok(taken)) if taken > 0 => {
				offset += taken as usize;
				writes += 1;
			}
			Some(Err(Error::Again)) => {
				again += 1;
				let want = back.len() + 1;
				read_until(stream, &mut back, want, TICKS / 10);
			}
			other => fail(&format!("a write of the burst answered {other:?}")),
		}
	}
	read_until(stream, &mut back, burst.len(), 10 * TICKS);
	if back != burst {
		fail(&format!("the burst did not come back whole and in order: {} of {} bytes", back.len(), burst.len()));
	}
	if again == 0 {
		fail("the burst never met the peer's credits: no write was answered again");
	}
	say(&format!("a burst of {} bytes taken in {writes} writes, {again} answered again while the credits were spent, and read back whole", burst.len()));

	if !matches!(client().close(), Some(Ok(()))) {
		fail("the port could not be closed");
	}
	let mut rest = Vec::new();
	read_until(stream, &mut rest, usize::MAX, 3 * TICKS);
	if !matches!(try_recv_caps(stream, &mut [0u8; 8]), PolledCaps::Closed) {
		fail("the read stream did not end with the channel");
	}
	say("closed: the read stream ended with the channel");
	print(b"btserial: PASS\n");
	exit();
}
