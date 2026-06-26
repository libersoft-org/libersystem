use core::panic::PanicInfo;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
	crate::serial_println!();
	crate::serial_println!("*** KERNEL PANIC ***");
	crate::serial_println!("{}", info);
	// Drain the panic message to the wire before halting (serial is asynchronous).
	crate::arch::serial::flush_sync();
	crate::arch::halt_loop();
}

// under the test harness a panic means a failed test: report and exit QEMU
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
	crate::serial_println!("[failed]");
	crate::serial_println!("{}", info);
	crate::arch::serial::flush_sync();
	crate::arch::exit_qemu(false);
}
