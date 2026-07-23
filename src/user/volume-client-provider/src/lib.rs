#![no_std]

use core::arch::global_asm;
use proto::system::{Error, volume};
use rt::{ReceivedVec, close, recv_vec_blocking, send_blocking};
use wire::{Sink, VecWriter};

#[unsafe(export_name = "liber_channel_liber_storage_volume_write_stream_begin")]
pub unsafe fn write_stream_begin(chan: u64, correlation: u32, path: &str, data: u64) -> bool {
	unsafe {
		let mut writer = VecWriter::new();
		let encoded = (|| {
			writer.u16(volume::OP_WRITE_STREAM)?;
			writer.u32(correlation)?;
			writer.bytes_lp(path.as_bytes())?;
			writer.set_handle(data)?;
			writer.u32(0)?;
			Some(())
		})();
		if encoded.is_none() {
			close(data);
			return false;
		}
		let request_handle = writer.handle();
		let request = writer.into_inner();
		if send_blocking(chan, &request, request_handle) {
			true
		} else {
			close(data);
			false
		}
	}
}

#[unsafe(export_name = "liber_channel_liber_storage_volume_write_stream_finish")]
pub unsafe fn write_stream_finish(chan: u64, correlation: u32) -> Option<Result<(), Error>> {
	unsafe {
		let ReceivedVec::Message { bytes, handle } = recv_vec_blocking(chan) else { return None };
		if handle != 0 {
			close(handle);
			return None;
		}
		if bytes.len() < 5 || u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != correlation {
			return None;
		}
		if bytes[4] != 0 {
			return if bytes.len() == 5 { Some(Ok(())) } else { None };
		}
		if bytes.len() != 6 {
			return None;
		}
		Some(Err(Error::decode(&bytes[5..])?))
	}
}

#[cfg(target_arch = "x86_64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",@function\n", $symbol, ":\n", "jmp ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

#[cfg(target_arch = "aarch64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",%function\n", $symbol, ":\n", "b ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

#[cfg(target_arch = "riscv64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",%function\n", $symbol, ":\n", "tail ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

forward!("liber_channel_liber_storage_volume_open", "liber_channel_impl_liber_storage_volume_open");
forward!("liber_channel_liber_storage_volume_remove", "liber_channel_impl_liber_storage_volume_remove");
forward!("liber_channel_liber_storage_volume_mkdir", "liber_channel_impl_liber_storage_volume_mkdir");
forward!("liber_channel_liber_storage_volume_rmdir", "liber_channel_impl_liber_storage_volume_rmdir");
forward!("liber_channel_liber_storage_volume_list", "liber_channel_impl_liber_storage_volume_list");
forward!("liber_channel_liber_storage_volume_write", "liber_channel_impl_liber_storage_volume_write");
forward!("liber_channel_liber_storage_volume_snap_create", "liber_channel_impl_liber_storage_volume_snap_create");
forward!("liber_channel_liber_storage_volume_snap_list", "liber_channel_impl_liber_storage_volume_snap_list");
forward!("liber_channel_liber_storage_volume_snap_delete", "liber_channel_impl_liber_storage_volume_snap_delete");
forward!("liber_channel_liber_storage_volume_snap_open", "liber_channel_impl_liber_storage_volume_snap_open");
forward!("liber_channel_liber_storage_volume_capacity", "liber_channel_impl_liber_storage_volume_capacity");
forward!("liber_channel_liber_storage_volume_status", "liber_channel_impl_liber_storage_volume_status");
forward!("liber_channel_liber_storage_volume_set_compression", "liber_channel_impl_liber_storage_volume_set_compression");
forward!("liber_channel_liber_storage_volume_fsck", "liber_channel_impl_liber_storage_volume_fsck");
forward!("liber_channel_liber_storage_volume_restore", "liber_channel_impl_liber_storage_volume_restore");
