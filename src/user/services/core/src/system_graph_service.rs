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
use ipc_client::{ChannelTransport, SvcTransport};
use proto::system::{BindingRecord, BindingState, Component, ComponentState, ComponentType, Counters, DeviceEntry, DeviceType, Error, FailureCause, Graph, TraceSpan, device, provider_catalogue, supervisor, system_graph};
use rt::*;

// One component node the supervisor registered: its name and dependency edges (the
// graph structure) plus a read-only handle to its Process (the live data source).
struct Node {
	name: String,
	deps: Vec<String>,
	process: u64,
}

// The service state: the registered component nodes, a client connection to
// DeviceService for the device nodes (held as a re-resolving transport: DeviceService
// restarts transparently, and the broker answers a RESOLVE with a connection to the
// live instance, so the graph's device nodes survive the restart; None when the
// supervisor delivered no DEVICE connection), and a client connection to the
// ServiceManager supervisor for each node's restart / watchdog history.
struct GraphService {
	nodes: Vec<Node>,
	device: Option<SvcTransport>,
	// DeviceManager's binding snapshot. THE ONE SOURCE for what a device node's state is: the graph
	// used to write `ComponentState::Running` for every device unconditionally, which is the surface
	// that looks most like a status display being the one showing none.
	bindings: u64,
	supervisor_client: u64,
}

// A binding state as the graph's component state. The two vocabularies meet HERE and nowhere else,
// so a state added to one and forgotten in the other is a compile error.
//
// `Binding` and `Stopping` are both `Starting`: a node in transition is neither up nor down, and
// calling either of them running or failed would be the same kind of lie the constant was.
fn component_state(state: BindingState) -> ComponentState {
	match state {
		BindingState::Online => ComponentState::Running,
		// A node in transition is neither up nor down, and calling either of them running or failed
		// would be the same kind of lie the constant was. `Restarting` is what this surface has for
		// it, and a backoff is exactly that.
		BindingState::Binding | BindingState::Stopping | BindingState::Backoff => ComponentState::Restarting,
		BindingState::Failed | BindingState::Quarantined => ComponentState::Failed,
		// WAITING, NOT BROKEN. A device nothing binds and one waiting for a declared provider are
		// facts about the machine - a machine without a NIC is a machine, not a broken image - so
		// they read as `Pending` rather than `Failed`.
		BindingState::Unbound | BindingState::DependencyPending => ComponentState::Pending,
		// And one an operator turned off is STOPPED, which is what was asked for.
		BindingState::Disabled => ComponentState::Stopped,
	}
}

// The cause's name, for the node's `last_failure`. Empty for a binding that has not failed - a
// binding waiting for a provider has not failed, and rendering it as one would report a machine
// without a NIC as a broken image.
fn cause_text(cause: FailureCause) -> &'static [u8] {
	match cause {
		FailureCause::None => b"",
		FailureCause::DriverMissing => b"driver-missing",
		FailureCause::ProtocolMismatch => b"protocol-mismatch",
		FailureCause::ClaimRefused => b"claim-refused",
		FailureCause::IommuRequired => b"iommu-required",
		FailureCause::ResourceExhausted => b"resource-exhausted",
		FailureCause::SpawnFailed => b"spawn-failed",
		FailureCause::HandshakeTimeout => b"handshake-timeout",
		FailureCause::DriverExited => b"driver-exited",
		FailureCause::DriverReportedFailure => b"driver-reported-failure",
		FailureCause::TeardownUnconfirmed => b"teardown-unconfirmed",
		FailureCause::Hung => b"hung",
	}
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
			components.push(Component { name: node.name.clone(), r#type: ComponentType::Service, state, deps: node.deps.clone(), counters });
		}
		spans.push(TraceSpan { name: String::from("process.stats"), duration_ns: unsafe { clock_ns() }.wrapping_sub(stats_start) });

		// Device nodes: enumerate the hardware devices over the DeviceService connection,
		// timing the call as a "device.list" trace span. Each device is a leaf node owned
		// by DeviceManager, carrying its identity and zero counters. The transport
		// re-resolves through the broker when the connection died with a restarted
		// DeviceService, so the device nodes survive the restart.
		let list_start: u64 = unsafe { clock_ns() };
		let devices: Vec<DeviceEntry> = match self.device.as_mut().and_then(|t| device::Client::new(t).list()) {
			Some(Ok(d)) => d,
			_ => Vec::new(),
		};
		spans.push(TraceSpan { name: String::from("device.list"), duration_ns: unsafe { clock_ns() }.wrapping_sub(list_start) });
		// THE BINDINGS, from the one process that holds them. Asked once and matched to the device
		// nodes by index, so the graph renders what DeviceManager decided rather than a constant.
		let bindings: Vec<BindingRecord> = if self.bindings != 0 { provider_catalogue::Client::new(ChannelTransport { chan: self.bindings }).bindings().unwrap_or_default() } else { Vec::new() };
		for (at, d) in devices.iter().enumerate() {
			// A DEVICE WITH NO BINDING RECORD IS `Unknown`, NOT `Running`. That is the honest answer
			// for a node DeviceManager has not spoken about, and it is the whole difference from
			// what this line used to say.
			let (state, failure) = match bindings.get(at) {
				Some(record) => (component_state(record.state), String::from_utf8_lossy(cause_text(record.cause)).into_owned()),
				// A DEVICE DEVICEMANAGER HAS NOT SPOKEN ABOUT IS `Pending`, NOT `Running`. That is
				// the honest answer for a node with no record, and it is the whole difference from
				// what this line used to say.
				None => (ComponentState::Pending, String::new()),
			};
			let restarts: u32 = bindings.get(at).map_or(0, |record| record.attempts);
			components.push(Component { name: device_name(d), r#type: ComponentType::Device, state, deps: alloc::vec![String::from("device_manager")], counters: Counters { messages_sent: 0, messages_received: 0, handles: 0, memory_bytes: 0, restarts, watchdog_trips: 0, last_failure: failure } });
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
						components.push(Component { name: s.name.clone(), r#type: ComponentType::Service, state: ComponentState::Running, deps: Vec::new(), counters: Counters { messages_sent: 0, messages_received: 0, handles: 0, memory_bytes: 0, restarts: s.restarts, watchdog_trips: s.watchdog_trips, last_failure: s.last_failure.clone() } });
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
	let class: &str = match d.r#type {
		DeviceType::Net => "net",
		DeviceType::Block => "block",
		DeviceType::Console => "console",
		DeviceType::Usb => "usb",
		DeviceType::Unknown => "device",
	};
	alloc::format!("{class}-{}", d.index)
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
	// The binding snapshot's client, or 0 on a boot that granted none - in which case the device
	// nodes carry no state rather than an invented one.
	let mut bindings_client: u64 = 0;
	let service: u64 = loop {
		match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { len, handle } => {
				if len >= 4 && &buf[..4] == b"NODE" {
					nodes.push(parse_node(&buf[4..len], handle));
				} else if len >= 10 && &buf[..10] == b"SUPERVISOR" {
					supervisor_client = handle;
				} else if len >= 8 && &buf[..8] == b"BINDINGS" {
					bindings_client = handle;
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

	// 3. serve generated `snapshot` requests on connections minted from the channel the
	//    supervisor holds the client end of.
	//
	//    A factory rather than a single connection, because that is what being resolvable by
	//    name requires: the supervisor mints a fresh connection per client from this root, so a
	//    client that lost its channel when this process was replaced can ask for another. A
	//    service served on one channel can be restarted, but nobody can reconnect to it.
	let mut graph: GraphService = GraphService { nodes, device: if device_client != 0 { Some(SvcTransport::new(bootstrap, CAP_DEVICE, device_client)) } else { None }, bindings: bindings_client, supervisor_client };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan, req, handle, out, reply_handle| -> Option<usize> { system_graph::dispatch(&mut graph, req, handle, out, reply_handle) });
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
