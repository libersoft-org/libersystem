// driver.virtio-net - the userspace virtio network-device driver.
//
// It brings the NIC up, then drives both virtqueues: the receive queue (0) is
// interrupt-driven (DeviceManager hands it the device's Interrupt, like the
// virtio-input driver), and the transmit queue (1) is polled synchronously. A small
// in-process network stack (`net`) handles Ethernet / ARP / IPv4 / ICMP: the driver
// posts a pool of receive buffers, blocks on its IRQ, and on each wake drains the
// frames the device filled, feeds each to the stack, and transmits any reply. M33
// extracts the stack into a standing NetworkService with typed sockets; here it
// lives in the driver to prove the receive path and the lowest layers.

#![no_std]
#![no_main]

mod common;
mod net;
mod virtio;

use rt::*;

use crate::net::{Event, Ipv4Addr, MacAddr, Stack};
use crate::virtio::{Queue, Virtio};

// The virtio_net_hdr prepended to every frame on both queues (VERSION_1: 12 bytes).
const NET_HDR_LEN: u64 = 12;
// The receive buffer pool: a handful of slots, each holding one full frame (the
// 12-byte header + an up-to-1514-byte Ethernet frame fits comfortably in 2 KiB).
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 2048;
// The transmit scratch buffer (one frame at a time, built then submitted).
const TX_BUF: u64 = 4096;
// How long a `ping` waits for its reply before reporting a timeout (100 Hz ticks).
const PING_TIMEOUT_TICKS: u64 = 50;

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24 and the gateway/host is 10.0.2.2, which answers ARP and ICMP. A DHCP
// client (M33) later replaces this static configuration.
const OUR_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const GATEWAY_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up and receive our device's Interrupt capability.
		let device: Virtio = common::bringup(bootstrap);
		let irq: u64 = recv_irq(bootstrap);
		// read the NIC's MAC from device-specific config (our Ethernet source).
		let mut mac: [u8; 6] = [0u8; 6];
		for (i, b) in mac.iter_mut().enumerate() {
			*b = device.config_read(i as u64);
		}
		// 2. set up the queues: receiveq 0 (interrupt-driven), transmitq 1 (polled).
		let mut rx: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		let tx: Queue = match device.setup_queue(1) {
			Some(q) => q,
			None => exit(),
		};
		rx.enable_interrupts();
		let (_rxpool, rx_virt, rx_phys): (u64, u64, u64) = match dma_buffer(RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer(TX_BUF) {
			Some(t) => t,
			None => exit(),
		};
		// 3. post the receive pool and go live.
		let mut id: u16 = 0;
		while id < RX_SLOTS {
			rx.post_recv(id, rx_phys + id as u64 * RX_SLOT, RX_SLOT as u32);
			id += 1;
		}
		rx.notify();
		device.driver_ok();
		// 4. create the control channel the shell reaches us on (the `ip` / `ping`
		//    commands), hand its far end up with the online report (DeviceManager routes
		//    it to the shell), discover the gateway, then serve the network and control.
		let (control, control_far): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		send_blocking(bootstrap, b"driver.virtio-net: online", control_far);
		let mut stack: Stack = Stack::new(MacAddr(mac), OUR_IP, GATEWAY_IP);
		let arp_len: usize = stack.build_arp_request(GATEWAY_IP, frame_out(tx_virt));
		transmit(&tx, tx_virt, tx_phys, arp_len);
		serve(irq, control, &device, &mut rx, &tx, &mut stack, rx_virt, rx_phys, tx_virt, tx_phys)
	}
}

// Receive the "IRQ" message carrying this device's Interrupt capability, which
// DeviceManager acquired and transferred to us. Exits if it does not arrive.
unsafe fn recv_irq(bootstrap: u64) -> u64 {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => exit(),
		}
	}
}

// The transmit frame area (after the virtio_net_hdr) as a writable slice the stack
// builds an outgoing frame into.
unsafe fn frame_out(tx_virt: u64) -> &'static mut [u8] {
	unsafe { core::slice::from_raw_parts_mut((tx_virt + NET_HDR_LEN) as *mut u8, (TX_BUF - NET_HDR_LEN) as usize) }
}

// Transmit the frame of `len` bytes already built into the transmit area: zero the
// virtio_net_hdr and submit the header + frame on the transmit queue. A zero length
// (the stack produced no frame) sends nothing.
unsafe fn transmit(tx: &Queue, tx_virt: u64, tx_phys: u64, len: usize) {
	unsafe {
		if len == 0 {
			return;
		}
		core::ptr::write_bytes(tx_virt as *mut u8, 0, NET_HDR_LEN as usize);
		tx.submit(&[(tx_phys, NET_HDR_LEN as u32 + len as u32, false)]);
	}
}

// Stand on the device interrupt and the shell's control channel at once (wait_any):
// on an interrupt, deassert and drain the receive ring (answering ARP/ICMP); on a
// control message, answer the shell's `ip` (interface state) or `ping` (send an echo
// request and report the reply). The control channel closing (the shell exited)
// drops us back to serving only the network.
#[allow(clippy::too_many_arguments)]
unsafe fn serve(irq: u64, control: u64, device: &Virtio, rx: &mut Queue, tx: &Queue, stack: &mut Stack, rx_virt: u64, rx_phys: u64, tx_virt: u64, tx_phys: u64) -> ! {
	unsafe {
		let mut buf: [u8; 128] = [0u8; 128];
		let mut seq: u16 = 0;
		let mut control_open: bool = true;
		loop {
			let ready: i64 = if control_open {
				wait_any(&[irq, control], 0)
			} else {
				wait(irq, 0);
				0
			};
			if ready == 0 {
				drain_rx(irq, device, rx, tx, stack, rx_virt, rx_phys, tx_virt, tx_phys);
			} else if ready == 1 {
				match recv_blocking(control, &mut buf) {
					Received::Message { len, .. } => handle_control(&buf[..len], control, irq, device, rx, tx, stack, &mut seq, rx_virt, rx_phys, tx_virt, tx_phys),
					Received::Closed => control_open = false,
				}
			}
		}
	}
}

// Drain every frame the device received: feed each to the stack, transmit any reply,
// and re-post the buffer. Deasserts the ISR line first and re-arms the interrupt
// after. Returns the source of the last ICMP echo reply seen (so an in-flight `ping`
// detects its answer), or None.
#[allow(clippy::too_many_arguments)]
unsafe fn drain_rx(irq: u64, device: &Virtio, rx: &mut Queue, tx: &Queue, stack: &mut Stack, rx_virt: u64, rx_phys: u64, tx_virt: u64, tx_phys: u64) -> Option<Ipv4Addr> {
	unsafe {
		device.isr_ack();
		let mut echo: Option<Ipv4Addr> = None;
		while let Some((id, len)) = rx.take_used() {
			if id < RX_SLOTS && len as u64 > NET_HDR_LEN {
				let frame: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT + NET_HDR_LEN) as *const u8, (len as u64 - NET_HDR_LEN) as usize);
				let outcome: net::Outcome = stack.on_frame(frame, frame_out(tx_virt));
				if let Event::EchoReply(ip) = outcome.event {
					echo = Some(ip);
				}
				transmit(tx, tx_virt, tx_phys, outcome.reply_len);
			}
			rx.post_recv(id, rx_phys + id as u64 * RX_SLOT, RX_SLOT as u32);
		}
		rx.notify();
		interrupt_ack(irq);
		echo
	}
}

// Answer one control request from the shell: `ip` replies with the serialized
// interface state; `ping` + a 4-byte address sends an echo request and replies with
// a status byte (1 = reply, 0 = timeout, 2 = unresolved) and the address.
#[allow(clippy::too_many_arguments)]
unsafe fn handle_control(req: &[u8], control: u64, irq: u64, device: &Virtio, rx: &mut Queue, tx: &Queue, stack: &mut Stack, seq: &mut u16, rx_virt: u64, rx_phys: u64, tx_virt: u64, tx_phys: u64) {
	unsafe {
		let mut out: [u8; 128] = [0u8; 128];
		if req == b"IP" {
			let n: usize = stack.write_state(&mut out);
			send_blocking(control, &out[..n], 0);
		} else if req.len() >= 8 && &req[..4] == b"PING" {
			let ip: Ipv4Addr = Ipv4Addr([req[4], req[5], req[6], req[7]]);
			let status: u8 = do_ping(ip, irq, device, rx, tx, stack, seq, rx_virt, rx_phys, tx_virt, tx_phys);
			out[0] = status;
			out[1..5].copy_from_slice(&ip.0);
			send_blocking(control, &out[..5], 0);
		}
	}
}

// Send an ICMP echo request to `ip` (ARP-resolving it first if its MAC is unknown)
// and wait for the reply, draining receive frames as they arrive. Returns 1 = reply
// received, 0 = timed out, 2 = the address could not be resolved.
#[allow(clippy::too_many_arguments)]
unsafe fn do_ping(ip: Ipv4Addr, irq: u64, device: &Virtio, rx: &mut Queue, tx: &Queue, stack: &mut Stack, seq: &mut u16, rx_virt: u64, rx_phys: u64, tx_virt: u64, tx_phys: u64) -> u8 {
	unsafe {
		if stack.lookup(ip).is_none() {
			let arp: usize = stack.build_arp_request(ip, frame_out(tx_virt));
			transmit(tx, tx_virt, tx_phys, arp);
			let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
			while clock() < deadline && stack.lookup(ip).is_none() {
				wait(irq, deadline);
				drain_rx(irq, device, rx, tx, stack, rx_virt, rx_phys, tx_virt, tx_phys);
			}
		}
		let mac: MacAddr = match stack.lookup(ip) {
			Some(m) => m,
			None => return 2,
		};
		*seq = seq.wrapping_add(1);
		let echo: usize = stack.build_icmp_echo(mac, ip, 1, *seq, frame_out(tx_virt));
		transmit(tx, tx_virt, tx_phys, echo);
		let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
		while clock() < deadline {
			wait(irq, deadline);
			if let Some(reply) = drain_rx(irq, device, rx, tx, stack, rx_virt, rx_phys, tx_virt, tx_phys) {
				if reply == ip {
					return 1;
				}
			}
		}
		0
	}
}
