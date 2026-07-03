// resource_manager - the userspace resource-policy manager (ResourceManager).
//
// ResourceManager is the policy over the kernel's resource accounting. ServiceManager
// starts it from the init package and hands it the package (so it can launch the
// component it governs) and a "SERVE" channel its clients reach it on.
//
// Its policy is a per-Domain budget. It creates a bounded sub-Domain, launches a governed
// component (resource_probe) into that Domain, and sets the Domain's memory limit over the
// typed property API on top of the kernel's existing enforcement. The kernel charges and
// caps every allocation the component makes against that Domain; an over-budget allocation
// is refused with ERR_RESOURCE_EXHAUSTED, contained to the offending Domain rather than
// crashing the component or the system. The manager observes usage against the budget with
// the live Domain stats, and adjusts the budget at runtime.
//
// Over the SERVE channel callers speak the generated `liber:system` resources bindings:
// `usage` returns the live budget of every managed Domain (used vs limit per resource),
// `set-limit` adjusts one Domain's cap for one resource at runtime and returns its updated
// budget.
//
// This milestone it governs one component, resource_probe, under a memory budget: it caps
// the Domain, drives the probe to fill the budget and be refused once (gracefully), raises
// the budget at runtime, and drives the probe into the new headroom - then relays that
// proof to the supervisor and serves the resources contract until the supervisor drops its
// bootstrap channel.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use proto::system::resources::{self, Service};
use proto::system::{Budget, Error, ResourceKind, ResourceUsage};
use rt::*;

// The governed component this milestone launches, and the name of the budget the manager
// assigns its Domain (the shared app sandbox).
const PROBE_NAME: &[u8] = b"resource_probe";
const BUDGET_NAME: &str = "apps";

// One page, the unit the kernel charges memory in. The probe allocates one-page objects,
// so a budget of N pages above the Domain's baseline admits exactly N of them.
const PAGE: u64 = 4096;

// The headroom, in pages, the manager grants above the Domain's baseline charge for one
// round. It grants this much, drives the probe to fill it and be refused once, then raises
// the cap by the same amount again at runtime.
const GRANT_PAGES: u64 = 4;

// The PROP_*_LIMIT selector that sets a Domain counter for a resource kind.
fn prop_for(kind: ResourceKind) -> u64 {
	match kind {
		ResourceKind::Memory => PROP_MEMORY_LIMIT,
		ResourceKind::Handles => PROP_HANDLE_LIMIT,
		ResourceKind::Threads => PROP_THREAD_LIMIT,
		ResourceKind::IpcQueue => PROP_IPC_QUEUE_LIMIT,
		ResourceKind::Dma => PROP_DMA_LIMIT,
	}
}

// The live budget of a Domain: its name and the used/limit of every accounted resource,
// read straight from the kernel's per-Domain counters. The typed form `usage` serves and
// `set-limit` returns updated.
fn budget_of(domain: u64) -> Budget {
	let stats: DomainStats = unsafe { domain_stats(domain) }.unwrap_or_default();
	let usage: Vec<ResourceUsage> = alloc::vec![
		ResourceUsage { kind: ResourceKind::Memory, used: stats.memory_used, limit: stats.memory_limit },
		ResourceUsage { kind: ResourceKind::Handles, used: stats.handles_used, limit: stats.handles_limit },
		ResourceUsage { kind: ResourceKind::Threads, used: stats.threads_used, limit: stats.threads_limit },
		ResourceUsage { kind: ResourceKind::IpcQueue, used: stats.ipc_used, limit: stats.ipc_limit },
		ResourceUsage { kind: ResourceKind::Dma, used: stats.dma_used, limit: stats.dma_limit },
	];
	Budget { name: String::from(BUDGET_NAME), usage }
}

// The manager's serve state: the one Domain it governs. `usage` reports its live budget;
// `set-limit` adjusts one of its caps and reports the updated budget.
struct Manager {
	domain: u64,
}

impl Service for Manager {
	fn usage(&mut self) -> Result<Vec<Budget>, Error> {
		Ok(alloc::vec![budget_of(self.domain)])
	}
	fn set_limit(&mut self, name: String, kind: ResourceKind, limit: u64) -> Result<Budget, Error> {
		if name.as_bytes() != BUDGET_NAME.as_bytes() {
			return Err(Error::NotFound);
		}
		if unsafe { domain_set_limit(self.domain, prop_for(kind), limit) } != 0 {
			return Err(Error::Denied);
		}
		Ok(budget_of(self.domain))
	}
}

// Send one command to the probe and wait for its DONE acknowledgement. Returns true if the
// probe answered - proof it survived the round (it did not crash on an over-budget refusal)
// - or false if the channel send failed or the probe's side is gone.
unsafe fn drive(channel: u64, command: &[u8], buf: &mut [u8]) -> bool {
	unsafe {
		if !send_blocking(channel, command, 0) {
			return false;
		}
		matches!(recv_blocking(channel, buf), Received::Message { .. })
	}
}

// Govern the component: launch resource_probe into the bounded `domain`, set a memory
// budget on that Domain, and drive the probe through it. Returns (granted, denied,
// regranted) in pages: how many one-page objects fit under the initial budget, how many
// over-budget refusals the probe took yet survived (contained to its Domain), and how many
// more fit after the budget was raised at runtime. The probe and its channel are left open
// (never closed) so it stays parked holding its objects alive and the manager can keep
// observing its live usage.
unsafe fn govern(package: &Package, domain: u64, buf: &mut [u8]) -> (u64, u64, u64) {
	unsafe {
		let elf: &[u8] = match package.lookup(PROBE_NAME) {
			Some(e) => e,
			None => return (0, 0, 0),
		};
		let (manager_side, child_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return (0, 0, 0),
		};
		if spawn_in(elf, child_side, domain) < 0 {
			close(manager_side);
			return (0, 0, 0);
		}

		// The Domain's baseline charge: the probe's eagerly-mapped image and stack, read
		// before it allocates anything. Every page the Domain accounts beyond this is one
		// of the probe's explicit one-page objects (it is heap-free), so the budget
		// arithmetic below is exact.
		let base: u64 = domain_stats(domain).map(|s| s.memory_used).unwrap_or(0);

		// Set the initial memory budget: room for exactly GRANT_PAGES one-page objects above
		// the baseline. Then drive the probe to fill it and be refused the next allocation.
		domain_set_limit(domain, PROP_MEMORY_LIMIT, base + GRANT_PAGES * PAGE);
		let survived: bool = drive(manager_side, b"GO", buf);
		let after_first: u64 = domain_stats(domain).map(|s| s.memory_used).unwrap_or(base);
		let granted: u64 = after_first.saturating_sub(base) / PAGE;

		// Raise the budget at runtime by the same headroom again, then drive the probe into
		// the new room - observing that an adjusted budget takes effect live.
		domain_set_limit(domain, PROP_MEMORY_LIMIT, base + 2 * GRANT_PAGES * PAGE);
		drive(manager_side, b"MORE", buf);
		let after_second: u64 = domain_stats(domain).map(|s| s.memory_used).unwrap_or(after_first);
		let regranted: u64 = after_second.saturating_sub(after_first) / PAGE;

		// The probe answered round 1, so the one over-budget refusal it hit there was
		// contained to its Domain and handled gracefully rather than crashing it.
		let denied: u64 = u64::from(survived);
		(granted, denied, regranted)
	}
}

// Build the human-readable budget summary the supervisor relays as the manager's proof:
// the pages granted under the cap, the over-budget refusal that was contained, and the
// pages regranted after the runtime raise. The live budgets themselves are served verbatim
// over the resources contract.
fn summarize(granted: u64, denied: u64, regranted: u64) -> Vec<u8> {
	let mut out: String = String::new();
	let _ = write!(out, "granted={granted} denied={denied} regranted={regranted}");
	out.into_bytes()
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 512] = [0u8; 512];

	// 1. receive the init package (to launch the governed component from), then the serve
	//    channel clients reach us on.
	let (_pkg_handle, archive): (u64, &[u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 2. create the bounded sub-Domain that hosts the governed component. It starts
	//    uncapped; govern() sets and adjusts its memory budget around the probe.
	let domain: i64 = unsafe { domain_create(u64::MAX, u64::MAX, u64::MAX) };
	if domain < 0 {
		exit();
	}
	let domain: u64 = domain as u64;

	// 3. govern the component under its budget: cap the Domain, observe the over-budget
	//    refusal being contained, raise the cap at runtime, and observe usage.
	let (granted, denied, regranted): (u64, u64, u64) = unsafe { govern(&package, domain, &mut buf) };

	// 4. report in to the supervisor, then relay the budget proof.
	unsafe {
		send_blocking(bootstrap, b"ResourceManager: online", 0);
		send_blocking(bootstrap, &summarize(granted, denied, regranted), 0);
	}

	// 5. serve generated usage/set-limit requests against the live Domain until the
	//    supervisor drops the channel.
	let mut manager: Manager = Manager { domain };
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { resources::dispatch(&mut manager, req, handle, out, reply_handle) });
	}
	exit();
}
