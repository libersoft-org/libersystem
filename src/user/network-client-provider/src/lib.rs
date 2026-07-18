#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(
			".section .text.", $symbol, ",\"ax\",@progbits\n",
			".globl ", $symbol, "\n",
			".type ", $symbol, ",@function\n",
			$symbol, ":\n",
			"jmp ", $implementation, "\n",
			".size ", $symbol, ", . - ", $symbol, "\n",
		));
	};
}

#[cfg(target_arch = "aarch64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(
			".section .text.", $symbol, ",\"ax\",@progbits\n",
			".globl ", $symbol, "\n",
			".type ", $symbol, ",%function\n",
			$symbol, ":\n",
			"b ", $implementation, "\n",
			".size ", $symbol, ", . - ", $symbol, "\n",
		));
	};
}

#[cfg(target_arch = "riscv64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(
			".section .text.", $symbol, ",\"ax\",@progbits\n",
			".globl ", $symbol, "\n",
			".type ", $symbol, ",%function\n",
			$symbol, ":\n",
			"tail ", $implementation, "\n",
			".size ", $symbol, ", . - ", $symbol, "\n",
		));
	};
}

forward!("liber_channel_liber_network_network_capacity", "liber_channel_impl_liber_network_network_capacity");
forward!("liber_channel_liber_network_network_connect", "liber_channel_impl_liber_network_network_connect");
forward!("liber_channel_liber_network_network_fetch", "liber_channel_impl_liber_network_network_fetch");
forward!("liber_channel_liber_network_network_info", "liber_channel_impl_liber_network_network_info");
forward!("liber_channel_liber_network_network_listen", "liber_channel_impl_liber_network_network_listen");
forward!("liber_channel_liber_network_network_open", "liber_channel_impl_liber_network_network_open");
forward!("liber_channel_liber_network_network_ping", "liber_channel_impl_liber_network_network_ping");
forward!("liber_channel_liber_network_network_resolve", "liber_channel_impl_liber_network_network_resolve");
forward!("liber_channel_liber_network_network_sntp", "liber_channel_impl_liber_network_network_sntp");
forward!("liber_channel_liber_network_network_sockets", "liber_channel_impl_liber_network_network_sockets");
forward!("liber_channel_liber_network_socket_close", "liber_channel_impl_liber_network_socket_close");
forward!("liber_channel_liber_network_socket_recv", "liber_channel_impl_liber_network_socket_recv");
forward!("liber_channel_liber_network_socket_send", "liber_channel_impl_liber_network_socket_send");
forward!("liber_channel_liber_network_listener_accept", "liber_channel_impl_liber_network_listener_accept");
