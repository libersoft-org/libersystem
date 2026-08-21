use core::panic::PanicInfo;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
	crate::serial_println!();
	crate::serial_println!("*** KERNEL PANIC ***");
	crate::serial_println!("{}", info);
	// AND WHAT EVERY PARKED THREAD WAS WAITING FOR.
	//
	// A panic is one of the two moments when "who is blocked, and on what" is worth having and
	// cannot be asked for afterwards - the machine stops here. The other is a hang, which has no
	// hook at all, so this is where the answer gets printed while there is still a wire to print
	// it on. It costs nothing on a system that does not panic.
	crate::sched::dump_blocked("at panic");
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
