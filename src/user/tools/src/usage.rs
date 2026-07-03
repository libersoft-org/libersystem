// usage - show the live per-Domain resource budgets, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a ResourceManager client - and forwards it the shell's stdout console and
// the argument string (the sub-form: "" for a text table or "json"). usage reads the budgets
// through its grant and prints them to the inherited stdout, then exits. A standalone command,
// not a shell built-in: it reaches the service only through the one capability the permission
// store granted it, and renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{Budget, resources};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" for JSON).
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a ResourceManager client.
		let ressvc: u64 = recv_tagged(bootstrap, &mut buf, b"RESOURCE").unwrap_or_else(|| exit());
		query_resource(ressvc, &args[..] == b"json");
	}
	exit();
}

// Read the live per-Domain budgets through the grant and render them: as JSON (the generated
// wire form, one document per budget) or as a compact text table - one line per budget, each
// resource shown as `kind=used/limit`, with an unlimited limit (u64::MAX, the kernel's
// UNLIMITED sentinel) shown as `unlimited` rather than the raw number.
unsafe fn query_resource(ressvc: u64, json: bool) {
	unsafe {
		let mut client = resources::Client::new(ChannelTransport { chan: ressvc });
		match client.usage() {
			Some(Ok(budgets)) => {
				if json {
					print(b"[");
					let mut first: bool = true;
					for b in budgets.iter() {
						if !first {
							print(b",");
						}
						first = false;
						print(b.to_json().as_bytes());
					}
					print(b"]\n");
				} else {
					for b in budgets.iter() {
						print_budget(b);
					}
				}
			}
			Some(Err(_)) => print(b"usage: query error\n"),
			None => print(b"usage: service unavailable\n"),
		}
	}
}

// Render one budget as a compact text line: `<name>: kind=used/limit ...`, with the kernel's
// UNLIMITED sentinel (u64::MAX) shown as `unlimited`.
unsafe fn print_budget(budget: &Budget) {
	unsafe {
		let mut line = String::new();
		line.push_str(&budget.name);
		line.push(':');
		for u in budget.usage.iter() {
			line.push(' ');
			line.push_str(&u.kind.to_text());
			line.push('=');
			push_amount(&mut line, u.used);
			line.push('/');
			push_amount(&mut line, u.limit);
		}
		line.push('\n');
		print(line.as_bytes());
	}
}

// Append a resource amount, rendering the kernel's UNLIMITED sentinel (u64::MAX) as
// `unlimited` rather than the raw 64-bit number.
fn push_amount(out: &mut String, value: u64) {
	use core::fmt::Write as _;
	if value == u64::MAX {
		out.push_str("unlimited");
	} else {
		let _ = write!(out, "{value}");
	}
}
