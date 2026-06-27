// SystemGraphService - the userspace observability service.
//
// ServiceManager starts this program once the core services are up and hands it,
// over its bootstrap channel, one "NODE" message per component - the component's
// name and declared dependencies as the payload, and a read-only capability to that
// component's Process as the transferred handle - followed by a "DEVICE" connection
// to DeviceService and a "SERVE" channel its own clients (the shell) reach it on.
//
// It then serves the generated `system-graph` interface: on each `snapshot` it reads
// every component's live counters and state straight from the kernel over the process
// handles it holds (SYS_PROCESS_STATS_GET), enumerates the hardware devices over its
// DeviceService connection, and assembles the whole labeled live graph - components,
// device nodes, dependency edges, per-component counters - as one typed value the
// shell renders as CLI / JSON / CBOR. Because state and counters are derived live from
// the kernel, a component that crashes or is stopped surfaces as failed / stopped at
// the next snapshot, without the component ever self-reporting it.
//
// Lightweight tracing rides along: the snapshot records a trace span for each of its
// downstream call groups (the per-process kernel stats reads, the DeviceService list),
// so the cost of building a graph is queryable over the same typed API. The network-
// exposed remote-admin surface over this graph's JSON / CBOR is a later phase; this is
// the local edge-node observability.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{device, supervisor, system_graph, Component, ComponentKind, ComponentState, Counters, DeviceEntry, DeviceKind, Error, Graph, TraceSpan};
use rt::*;

// One component node the supervisor registered: its name and dependency edges (the
// graph structure) plus a read-only handle to its Process (the live data source).
struct Node {
	name: String,
	deps: Vec<String>,
	process: u64,
}

// The service state: the registered component nodes, a client connection to
// DeviceService for the device nodes, and a client connection to the ServiceManager
// supervisor for each node's restart / watchdog history.
struct GraphService {
	nodes: Vec<Node>,
	device_client: u64,
	supervisor_client: u64,
}

impl system_graph::Service for GraphService {
	fn snapshot(&mut self) -> Result<Graph, Error> {
		let mut components: Vec<Component> = Vec::new();
		let mut spans: Vec<TraceSpan> = Vec::new();

		// Component nodes: read each one's live counters and state from the kernel over
		// its process handle, timing the whole batch as one "process.stats" trace span.
		let stats_start: u64 = unsafe { clock_ns() };
		for node in &self.nodes {
			let (state, counters): (ComponentState, Counters) = match unsafe { process_stats(node.process) } {
				Some(s) => (map_state(s.state), Counters { messages_sent: s.messages_sent, messages_received: s.messages_received, handles: s.handle_count, memory_bytes: s.memory_bytes, restarts: 0, watchdog_trips: 0, last_failure: String::new() }),
				None => (ComponentState::Failed, Counters { messages_sent: 0, messages_received: 0, handles: 0, memory_bytes: 0, restarts: 0, watchdog_trips: 0, last_failure: String::new() }),
			};
			components.push(Component { name: node.name.clone(), kind: ComponentKind::Service, state, deps: node.deps.clone(), counters });
		}
		spans.push(TraceSpan { name: String::from("process.stats"), duration_ns: unsafe { clock_ns() }.wrapping_sub(stats_start) });

		// Device nodes: enumerate the hardware devices over the DeviceService connection,
		// timing the call as a "device.list" trace span. Each device is a leaf node owned
		// by DeviceManager, carrying its identity and zero counters.
		let list_start: u64 = unsafe { clock_ns() };
		let mut dev: device::Client<ChannelTransport> = device::Client::new(ChannelTransport { chan: self.device_client });
		let devices: Vec<DeviceEntry> = match dev.list() {
			Some(Ok(d)) => d,
			_ => Vec::new(),
		};
		spans.push(TraceSpan { name: String::from("device.list"), duration_ns: unsafe { clock_ns() }.wrapping_sub(list_start) });
		for d in &devices {
			components.push(Component { name: device_name(d), kind: ComponentKind::Device, state: ComponentState::Running, deps: alloc::vec![String::from("device_manager")], counters: Counters { messages_sent: 0, messages_received: 0, handles: 0, memory_bytes: 0, restarts: 0, watchdog_trips: 0, last_failure: String::new() } });
		}

		// Supervisor history: query the ServiceManager supervisor and fold each managed
		// component's restart count, watchdog trips and last failure into its node (matched
		// by name), so the kernel's live counters and the supervisor's history sit together.
		// The managed watchdog canary has no kernel process node of its own, so it is added
		// as a synthetic node carrying just its supervisor counters. Timed as one
		// "supervisor.status" trace span. A 0 handle (e.g. a non-primary VT) skips the merge.
		if self.supervisor_client != 0 {
			let sup_start: u64 = unsafe { clock_ns() };
			let mut sup: supervisor::Client<ChannelTransport> = supervisor::Client::new(ChannelTransport { chan: self.supervisor_client });
			if let Some(Ok(stats)) = sup.status() {
				for s in &stats {
					for c in components.iter_mut() {
						if c.name.as_bytes() == s.name.as_bytes() {
							c.counters.restarts = s.restarts;
							c.counters.watchdog_trips = s.watchdog_trips;
							c.counters.last_failure = s.last_failure.clone();
							break;
						}
					}
					if s.name == "watchdog_probe" {
						components.push(Component { name: s.name.clone(), kind: ComponentKind::Service, state: ComponentState::Running, deps: Vec::new(), counters: Counters { messages_sent: 0, messages_received: 0, handles: 0, memory_bytes: 0, restarts: s.restarts, watchdog_trips: s.watchdog_trips, last_failure: s.last_failure.clone() } });
					}
				}
			}
			spans.push(TraceSpan { name: String::from("supervisor.status"), duration_ns: unsafe { clock_ns() }.wrapping_sub(sup_start) });
		}

		Ok(Graph { components, spans })
	}
}

// Map a kernel ProcessStats liveness code to the typed component state.
fn map_state(state: u64) -> ComponentState {
	match state {
		PROC_STATE_RUNNING => ComponentState::Running,
		PROC_STATE_STOPPED => ComponentState::Stopped,
		_ => ComponentState::Failed,
	}
}

// A device node's display name: its class plus its kernel-table index (e.g. "net-0").
fn device_name(d: &DeviceEntry) -> String {
	let kind: &str = match d.kind {
		DeviceKind::Net => "net",
		DeviceKind::Block => "block",
		DeviceKind::Console => "console",
		DeviceKind::Unknown => "device",
	};
	alloc::format!("{kind}-{}", d.index)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	let mut nodes: Vec<Node> = Vec::new();
	let mut device_client: u64 = 0;
	let mut supervisor_client: u64 = 0;

	// 1. receive the component registrations ("NODE"), the DeviceService connection
	//    ("DEVICE"), the supervisor connection ("SUPERVISOR"), and finally the channel
	//    our clients reach us on ("SERVE"), which ends the bootstrap. Each NODE carries a
	//    component's name + dependency edges as its payload and a read-only handle to its
	//    Process as the transferred handle.
	let service: u64 = loop {
		match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { len, handle } => {
				if len >= 4 && &buf[..4] == b"NODE" {
					nodes.push(parse_node(&buf[4..len], handle));
				} else if len >= 10 && &buf[..10] == b"SUPERVISOR" {
					supervisor_client = handle;
				} else if len >= 6 && &buf[..6] == b"DEVICE" {
					device_client = handle;
				} else if len >= 5 && &buf[..5] == b"SERVE" {
					break handle;
				}
			}
			Received::Closed => exit(),
		}
	};

	// 2. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"SystemGraphService: online", 0);
	}

	// 3. serve generated `snapshot` requests until the client side closes.
	let mut graph: GraphService = GraphService { nodes, device_client, supervisor_client };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve(service, &mut request, &mut reply, |req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { system_graph::dispatch(&mut graph, req, handle, out, reply_handle) });
	}
	exit();
}

// Parse one NODE payload: the component name, then (after a '\n') its dependency
// names joined by commas. The transferred process handle is paired with it.
fn parse_node(body: &[u8], process: u64) -> Node {
	let split: usize = body.iter().position(|&b| b == b'\n').unwrap_or(body.len());
	let name: String = String::from_utf8_lossy(&body[..split]).into_owned();
	let deps: Vec<String> = if split < body.len() { body[split + 1..].split(|&b| b == b',').filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect() } else { Vec::new() };
	Node { name, deps, process }
}
