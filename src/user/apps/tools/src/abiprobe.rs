// abiprobe - call every admitted foreign ABI facility once, in a guest, and say which answered.
//
// WHY EVERY ONE AND NOT A SAMPLE. The substrate provides EXACTLY what the converged audit link
// resolved - forty-eight symbols, no more and no fewer - and the gates that hold it to that count
// are static: they read export tables and inventories. None of them shows that a facility WORKS.
// A symbol that is present and wrong is a link that succeeds and a driver that misbehaves, which is
// the failure mode a C contract has: violations are silent.
//
// IT IS A QUARANTINE ARTIFACT, like `vkprobe`, and for a plainer reason: the substrate is not a
// staged library. It defines `malloc`, `free`, `strlen` and the rest under their C names, which
// `lsrt.lslib` and every other library would collide with - so it is linked into the consumer that
// uses it, by the audit link, and staged only into the development image.
//
// WHAT "USED" MEANS HERE. Each call is made with arguments whose answer is known, and the answer is
// CHECKED: a facility that returns the wrong thing is reported by name. Calling without checking
// would prove the symbol resolves, which the link already proved.

#![no_std]
#![no_main]

extern crate alloc;

use rt::*;

unsafe extern "C" {
	// The allocator.
	fn malloc(size: usize) -> *mut core::ffi::c_void;
	fn calloc(count: usize, size: usize) -> *mut core::ffi::c_void;
	fn realloc(pointer: *mut core::ffi::c_void, size: usize) -> *mut core::ffi::c_void;
	fn free(pointer: *mut core::ffi::c_void);
	// Strings and characters.
	fn strlen(text: *const u8) -> usize;
	fn strcmp(left: *const u8, right: *const u8) -> i32;
	fn strncmp(left: *const u8, right: *const u8, count: usize) -> i32;
	fn strcpy(destination: *mut u8, source: *const u8) -> *mut u8;
	fn strncpy(destination: *mut u8, source: *const u8, count: usize) -> *mut u8;
	fn strncat(destination: *mut u8, source: *const u8, count: usize) -> *mut u8;
	fn strchr(text: *const u8, character: i32) -> *mut u8;
	fn strrchr(text: *const u8, character: i32) -> *mut u8;
	fn strstr(haystack: *const u8, needle: *const u8) -> *mut u8;
	fn strerror(number: i32) -> *mut u8;
	fn tolower(character: i32) -> i32;
	fn thread_safe_strtok(text: *mut u8, delimiters: *const u8, context: *mut *mut u8) -> *mut u8;
	// Numbers.
	fn atoi(text: *const u8) -> i32;
	fn strtod(text: *const u8, end: *mut *mut u8) -> f64;
	fn fabs(value: f64) -> f64;
	// Formatting.
	fn snprintf(buffer: *mut u8, size: usize, format: *const u8, ...) -> i32;
	fn vsnprintf(buffer: *mut u8, size: usize, format: *const u8, arguments: *mut core::ffi::c_void) -> i32;
	// Diagnostics and process.
	fn fputs(text: *const u8, stream: *mut core::ffi::c_void) -> i32;
	fn fputc(character: i32, stream: *mut core::ffi::c_void) -> i32;
	fn __liber_errno_location() -> *mut i32;
	fn __liber_assert_failed(expression: *const u8, file: *const u8, line: i32);
	fn abort() -> !;
	static mut stderr: *mut core::ffi::c_void;
	// Streams and directories, which answer from the record rather than from a file system.
	fn fopen(path: *const u8, mode: *const u8) -> *mut core::ffi::c_void;
	fn fread(buffer: *mut core::ffi::c_void, size: usize, count: usize, stream: *mut core::ffi::c_void) -> usize;
	fn fclose(stream: *mut core::ffi::c_void) -> i32;
	fn fileno(stream: *mut core::ffi::c_void) -> i32;
	fn fstat(descriptor: i32, buffer: *mut core::ffi::c_void) -> i32;
	fn opendir(path: *const u8) -> *mut core::ffi::c_void;
	fn readdir(directory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
	fn closedir(directory: *mut core::ffi::c_void) -> i32;
	fn sscanf(text: *const u8, format: *const u8, ...) -> i32;
	// Providers.
	fn dladdr(address: *const core::ffi::c_void, info: *mut core::ffi::c_void) -> i32;
	fn loader_platform_is_path_absolute(path: *const u8) -> bool;
	fn loader_platform_file_exists(path: *const u8) -> bool;
	fn loader_platform_executable_path(buffer: *mut u8, size: usize) -> *mut u8;
	fn loader_platform_open_library(path: *const u8) -> *mut core::ffi::c_void;
	fn loader_platform_open_library_error(path: *const u8) -> *mut u8;
	fn loader_platform_get_proc_address(library: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
	fn loader_platform_close_library(library: *mut core::ffi::c_void);
	// Synchronisation, under the documented single-threaded contract.
	fn loader_platform_thread_create_mutex(mutex: *mut core::ffi::c_void);
	fn loader_platform_thread_lock_mutex(mutex: *mut core::ffi::c_void);
	fn loader_platform_thread_unlock_mutex(mutex: *mut core::ffi::c_void);
	fn loader_platform_thread_delete_mutex(mutex: *mut core::ffi::c_void);
	// The record installer, which is the substrate's control surface rather than a facility.
	fn liber_foreign_install_sink(report: unsafe extern "C" fn(*const u8, usize), stop: unsafe extern "C" fn() -> !);
	fn liber_foreign_install_icd(manifest_path: *const u8, manifest: *const u8, library_path: *const u8, self_path: *const u8, negotiate: unsafe extern "C" fn(*mut u32) -> i32, gpa: unsafe extern "C" fn(*mut core::ffi::c_void, *const u8) -> *mut core::ffi::c_void) -> i32;
	// The ICD bound into this process through its selection slot, which the record needs.
	fn vk_icdNegotiateLoaderICDInterfaceVersion(version: *mut u32) -> i32;
	fn vk_icdGetInstanceProcAddr(instance: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
}

static FAILURES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static CHECKED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn check(name: &[u8], held: bool) {
	CHECKED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
	if !held {
		FAILURES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
		print(b"abiprobe: WRONG ANSWER from ");
		print(name);
		print(b"\n");
	}
}

unsafe extern "C" fn report(bytes: *const u8, len: usize) {
	print(b"abiprobe: foreign: ");
	// SAFETY: the substrate passes a slice out of its own image.
	print(unsafe { core::slice::from_raw_parts(bytes, len) });
	print(b"\n");
}

unsafe extern "C" fn stop() -> ! {
	print(b"abiprobe: the substrate stopped the process\n");
	exit()
}

fn decimal(value: u32) -> ([u8; 10], usize) {
	let mut digits = [b'0'; 10];
	let mut at = digits.len();
	let mut left = value;
	loop {
		at -= 1;
		digits[at] = b'0' + (left % 10) as u8;
		left /= 10;
		if left == 0 {
			break;
		}
	}
	(digits, at)
}

fn number(label: &[u8], value: u32) {
	print(label);
	let (digits, at) = decimal(value);
	print(&digits[at..]);
	print(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	// SAFETY: both are functions in this image, which outlives every foreign call.
	unsafe { liber_foreign_install_sink(report, stop) };

	print(b"abiprobe: group allocator\n");
	// THE ALLOCATOR. Each call's answer is checked rather than merely made: an allocator that
	// returned the same block twice would pass a call-it-once test and fail every real caller.
	unsafe {
		let first = malloc(64);
		check(b"malloc", !first.is_null());
		let zeroed = calloc(8, 8) as *mut u8;
		check(b"calloc", !zeroed.is_null() && (0..64).all(|index| *zeroed.add(index) == 0));
		check(b"malloc twice", first as usize != zeroed as usize);
		let grown = realloc(first, 256);
		check(b"realloc", !grown.is_null());
		free(grown);
		free(zeroed as *mut core::ffi::c_void);
	}

	print(b"abiprobe: group strings\n");
	// STRINGS. Every answer is one C states exactly.
	unsafe {
		let hello = b"hello world\0".as_ptr();
		check(b"strlen", strlen(hello) == 11);
		check(b"strcmp", strcmp(hello, b"hello world\0".as_ptr()) == 0 && strcmp(hello, b"hello worle\0".as_ptr()) < 0);
		check(b"strncmp", strncmp(hello, b"hello there\0".as_ptr(), 6) == 0);
		let mut buffer = [0u8; 32];
		check(b"strcpy", strcpy(buffer.as_mut_ptr(), b"abc\0".as_ptr()) == buffer.as_mut_ptr() && buffer[..4] == *b"abc\0");
		check(b"strncat", {
			strncat(buffer.as_mut_ptr(), b"def\0".as_ptr(), 8);
			buffer[..7] == *b"abcdef\0"
		});
		let mut padded = [0x7fu8; 8];
		strncpy(padded.as_mut_ptr(), b"xy\0".as_ptr(), 8);
		// `strncpy` PADS WITH NUL to the full count, which is the property nobody remembers and the
		// one a caller relying on it depends on.
		check(b"strncpy", padded == [b'x', b'y', 0, 0, 0, 0, 0, 0]);
		check(b"strchr", strchr(hello, b' ' as i32) == hello.add(5) as *mut u8);
		check(b"strrchr", strrchr(hello, b'l' as i32) == hello.add(9) as *mut u8);
		check(b"strstr", strstr(hello, b"world\0".as_ptr()) == hello.add(6) as *mut u8);
		check(b"strerror", !strerror(2).is_null());
		check(b"tolower", tolower(b'Q' as i32) == b'q' as i32 && tolower(b'q' as i32) == b'q' as i32);
		let mut tokens = *b"a,b\0";
		let mut context: *mut u8 = core::ptr::null_mut();
		let first = thread_safe_strtok(tokens.as_mut_ptr(), b",\0".as_ptr(), &raw mut context);
		let second = thread_safe_strtok(core::ptr::null_mut(), b",\0".as_ptr(), &raw mut context);
		check(b"thread_safe_strtok", !first.is_null() && !second.is_null() && *first == b'a' && *second == b'b');
	}

	print(b"abiprobe: group numbers\n");
	// NUMBERS.
	unsafe {
		check(b"atoi", atoi(b"-42\0".as_ptr()) == -42);
		let mut end: *mut u8 = core::ptr::null_mut();
		let value = strtod(b"2.5rest\0".as_ptr(), &raw mut end);
		check(b"strtod", value == 2.5 && !end.is_null() && *end == b'r');
		check(b"fabs", fabs(-3.25) == 3.25 && fabs(3.25) == 3.25);
	}

	print(b"abiprobe: group formatting\n");
	// FORMATTING. `snprintf` returns what it WOULD have written, which is its defining property.
	unsafe {
		let mut small = [0u8; 4];
		let would = snprintf(small.as_mut_ptr(), small.len(), b"%s=%d\0".as_ptr(), b"n\0".as_ptr(), 123);
		check(b"snprintf", would == 5 && small[..3] == *b"n=1");
		// `vsnprintf` IS RESOLVED AND DELIBERATELY NOT CALLED. Rust cannot construct a `va_list`
		// portably, and the first version of this line called it with a null format to prove the
		// symbol was there - which is not a test, it is undefined behaviour, and the guest faulted
		// on it immediately. What the two share in this substrate is the rendering path `snprintf`
		// above just exercised; what differs is only how the arguments arrive, and the host fixtures
		// cover that.
		let _ = vsnprintf;
	}

	print(b"abiprobe: group diagnostics\n");
	// DIAGNOSTICS. `fputs` and `fputc` reach the sink installed above, so this output is the
	// substrate's own path rather than this program's.
	unsafe {
		check(b"fputs", fputs(b"fputs reached the sink\0".as_ptr(), stderr) == 0);
		check(b"fputc", fputc(b'.' as i32, stderr) == b'.' as i32);
		// A WRITE THAT IS NOT stderr IS REFUSED rather than discarded, which is the contract.
		check(b"fputs refuses", fputs(b"nowhere\0".as_ptr(), core::ptr::null_mut()) == -1);
		let slot = __liber_errno_location();
		check(b"__liber_errno_location", !slot.is_null() && slot == __liber_errno_location());
	}

	print(b"abiprobe: group record\n");
	// THE RECORD, so the provider facilities below have something to answer from.
	let installed = unsafe { liber_foreign_install_icd(b"/system/share/vulkan/icd.d/icdprobe.json\0".as_ptr(), b"{\"file_format_version\":\"1.0.0\",\"ICD\":{\"library_path\":\"/system/lib/foreign/icdprobe.lslib\",\"api_version\":\"1.3.0\"}}\0".as_ptr(), b"/system/lib/foreign/icdprobe.lslib\0".as_ptr(), b"/system/libexec/abiprobe.lsexe\0".as_ptr(), vk_icdNegotiateLoaderICDInterfaceVersion, vk_icdGetInstanceProcAddr) };
	check(b"liber_foreign_install_icd", installed == 0);

	print(b"abiprobe: group streams\n");
	// STREAMS AND DIRECTORIES, answered from that record.
	unsafe {
		let manifest = b"/system/share/vulkan/icd.d/icdprobe.json\0".as_ptr();
		let stream = fopen(manifest, b"r\0".as_ptr());
		check(b"fopen", !stream.is_null());
		check(b"fopen refuses a write", fopen(manifest, b"w\0".as_ptr()).is_null());
		let mut body = [0u8; 32];
		check(b"fread", fread(body.as_mut_ptr() as *mut core::ffi::c_void, 1, body.len(), stream) > 0);
		let descriptor = fileno(stream);
		check(b"fileno", descriptor >= 0);
		let mut status = [0u8; 64];
		check(b"fstat", fstat(descriptor, status.as_mut_ptr() as *mut core::ffi::c_void) == 0);
		check(b"fclose", fclose(stream) == 0);

		let directory = opendir(b"/system/share/vulkan/icd.d\0".as_ptr());
		check(b"opendir", !directory.is_null());
		check(b"readdir", !readdir(directory).is_null());
		check(b"readdir ends", readdir(directory).is_null());
		check(b"closedir", closedir(directory) == 0);
		check(b"opendir refuses", opendir(b"/nowhere\0".as_ptr()).is_null());

		let mut major: i32 = 0;
		let mut minor: i32 = 0;
		check(b"sscanf", sscanf(b"1.3\0".as_ptr(), b"%d.%d\0".as_ptr(), &raw mut major, &raw mut minor) == 2 && major == 1 && minor == 3);
	}

	print(b"abiprobe: group providers\n");
	// PROVIDERS. Opening one is taking a reference to something already in the verified closure.
	unsafe {
		check(b"loader_platform_is_path_absolute", loader_platform_is_path_absolute(b"/a\0".as_ptr()) && !loader_platform_is_path_absolute(b"a\0".as_ptr()));
		check(b"loader_platform_file_exists", loader_platform_file_exists(b"/system/lib/foreign/icdprobe.lslib\0".as_ptr()) && !loader_platform_file_exists(b"/nowhere\0".as_ptr()));
		let mut path = [0u8; 128];
		check(b"loader_platform_executable_path", !loader_platform_executable_path(path.as_mut_ptr(), path.len()).is_null());
		let library = loader_platform_open_library(b"/system/lib/foreign/icdprobe.lslib\0".as_ptr());
		check(b"loader_platform_open_library", !library.is_null());
		check(b"loader_platform_get_proc_address", !loader_platform_get_proc_address(library, b"vk_icdGetInstanceProcAddr\0".as_ptr()).is_null());
		loader_platform_close_library(library);
		check(b"loader_platform_open_library refuses", loader_platform_open_library(b"/nowhere\0".as_ptr()).is_null());
		check(b"loader_platform_open_library_error", !loader_platform_open_library_error(b"/nowhere\0".as_ptr()).is_null());
		let mut info = [0u8; 32];
		check(b"dladdr", dladdr(__user_main as *const core::ffi::c_void, info.as_mut_ptr() as *mut core::ffi::c_void) != 0);
	}

	print(b"abiprobe: group synchronisation\n");
	// SYNCHRONISATION, under the documented single-threaded contract: an uncontended lock is a
	// counter, and taking it twice in a row must not deadlock a process that has one thread.
	unsafe {
		let mut mutex = [0u8; 64];
		let pointer = mutex.as_mut_ptr() as *mut core::ffi::c_void;
		loader_platform_thread_create_mutex(pointer);
		loader_platform_thread_lock_mutex(pointer);
		loader_platform_thread_unlock_mutex(pointer);
		loader_platform_thread_lock_mutex(pointer);
		loader_platform_thread_unlock_mutex(pointer);
		loader_platform_thread_delete_mutex(pointer);
		check(b"loader_platform_thread_*", true);
	}

	// `abort` AND `__liber_assert_failed` ARE NOT CALLED, and saying so is more honest than calling
	// them: both diverge, and a probe that ended in one would report nothing about the forty-six
	// facilities it checked first. They are reached by the substrate's own failure paths, whose
	// contracts the host fixtures cover.
	let _ = abort;
	let _ = __liber_assert_failed;
	print(b"abiprobe: abort, __liber_assert_failed and vsnprintf are resolved and deliberately not called\n");

	number(b"abiprobe: checks=", CHECKED.load(core::sync::atomic::Ordering::Relaxed));
	number(b"abiprobe: failures=", FAILURES.load(core::sync::atomic::Ordering::Relaxed));
	print(b"abiprobe: done\n");
	exit();
}
