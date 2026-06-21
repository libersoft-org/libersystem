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
		send_blocking(bootstrap, b"driver.virtio-net: online", 0);
		// 4. discover the gateway (an ARP request that also proves the receive path),
		//    then stand on the interrupt serving the network.
		let mut stack: Stack = Stack::new(MacAddr(mac), OUR_IP, GATEWAY_IP);
		let arp_len: usize = stack.build_arp_request(GATEWAY_IP, frame_out(tx_virt));
		transmit(&tx, tx_virt, tx_phys, arp_len);
		event_loop(irq, &device, &mut rx, &tx, &mut stack, rx_virt, rx_phys, tx_virt, tx_phys)
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

// Block on the device interrupt forever: on each wake deassert the ISR line, drain
// every frame the device received, feed each to the stack, transmit any reply,
// re-post the drained buffer, and re-arm the interrupt.
unsafe fn event_loop(irq: u64, device: &Virtio, rx: &mut Queue, tx: &Queue, stack: &mut Stack, rx_virt: u64, rx_phys: u64, tx_virt: u64, tx_phys: u64) -> ! {
	unsafe {
		let mut pinged: bool = false;
		loop {
			wait(irq, 0);
			device.isr_ack();
			while let Some((id, len)) = rx.take_used() {
				if id < RX_SLOTS && len as u64 > NET_HDR_LEN {
					let frame: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT + NET_HDR_LEN) as *const u8, (len as u64 - NET_HDR_LEN) as usize);
					let outcome: net::Outcome = stack.on_frame(frame, frame_out(tx_virt));
					handle_event(&outcome.event, tx, stack, tx_virt, tx_phys, &mut pinged);
					transmit(tx, tx_virt, tx_phys, outcome.reply_len);
				}
				rx.post_recv(id, rx_phys + id as u64 * RX_SLOT, RX_SLOT as u32);
			}
			rx.notify();
			interrupt_ack(irq);
		}
	}
}

// React to a stack event: log it, and on first learning the gateway's MAC send it
// one ICMP echo request (a self-test proving the transmit + receive ICMP path).
unsafe fn handle_event(event: &Event, tx: &Queue, stack: &Stack, tx_virt: u64, tx_phys: u64, pinged: &mut bool) {
	unsafe {
		match event {
			Event::Learned(ip, _mac) => {
				print(b"net: arp reply from ");
				print_ip(*ip);
				print(b"\n");
				if !*pinged && *ip == GATEWAY_IP {
					if let Some(mac) = stack.lookup(GATEWAY_IP) {
						*pinged = true;
						let len: usize = stack.build_icmp_echo(mac, GATEWAY_IP, 1, 1, frame_out(tx_virt));
						transmit(tx, tx_virt, tx_phys, len);
					}
				}
			}
			Event::EchoReply(ip) => {
				print(b"net: echo reply from ");
				print_ip(*ip);
				print(b"\n");
			}
			Event::None => {}
		}
	}
}

// Print an IPv4 address in dotted-decimal form.
unsafe fn print_ip(ip: Ipv4Addr) {
	unsafe {
		for (i, octet) in ip.0.iter().enumerate() {
			if i != 0 {
				print(b".");
			}
			print_dec(*octet);
		}
	}
}

// Print a byte as decimal (0-255), no leading zeros.
unsafe fn print_dec(n: u8) {
	unsafe {
		if n >= 100 {
			print(&[b'0' + n / 100]);
		}
		if n >= 10 {
			print(&[b'0' + (n / 10) % 10]);
		}
		print(&[b'0' + n % 10]);
	}
}
