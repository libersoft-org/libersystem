use core::panic::PanicInfo;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
	// THE TERMINAL-PATH WRITER FIRST, before a byte of the panic is printed: it takes the console UART back
	// from whoever drives it - a driver may hold it and have stopped draining - re-initialises it, writes out
	// the ring's backlog and what its bound dropped, and from then on every line goes to the wire
	// synchronously, so this text cannot be lost at a full ring.
	crate::arch::serial::flush_sync();
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
	// Everything above went to the wire synchronously; this lets the last of it leave the UART.
	crate::arch::serial::flush_sync();
	crate::arch::halt_loop();
}

// under the test harness a panic means a failed test: report and exit QEMU
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
	crate::arch::serial::flush_sync();
	crate::serial_println!("[failed]");
	crate::serial_println!("{}", info);
	crate::arch::serial::flush_sync();
	crate::arch::exit_qemu(false);
}
