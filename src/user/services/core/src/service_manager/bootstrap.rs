use super::*;
use ipc_client::ChannelTransport;
use proto::system::volume_admin;

// THE PLAN EXECUTOR: hand one service its declared roles, in the declared order.
//
// This is what the twenty-three hand-written branches below are being replaced by, one service at a
// time. Each branch encodes in code what the manifest now says as data - which capabilities a
// service is given, where each comes from, and in what order - and the two must agree, so the
// executor is COMPARED against the branch before either is switched over.
//
// Three of the eight kinds resolve entirely from the plan, and they are most of every service:
//
//   serve-root  create a channel pair, send the service end, keep the client end under this
//               service's name and this role's tag - which is what makes the next two work
//   client      a duplicate of the client end kept from the provider's serve root named by
//               `source`, narrowed to what a client needs
//   factory     a fresh connection minted from that same kept end
//
// The rest - a driver's handle, a privileged capability from the kernel, a message of bytes - come
// from outside the service graph, and the executor takes them from the caller rather than
// inventing a way to derive what is not derivable.
pub(super) struct Kept {
	// One kept client end per serve root, indexed the same way `MANIFEST` is, so a lookup is a
	// walk over one service's roles rather than a map.
	pub ends: [[u64; MAX_ROLES]; N],
}

// The most roles any one service declares. The plan is generated, so this is checked at build time
// by the array it sizes rather than believed.
pub(super) const MAX_ROLES: usize = 32;

impl Kept {
	pub(super) fn new() -> Kept {
		Kept { ends: [[0; MAX_ROLES]; N] }
	}

	// Record a client end a hand-written branch kept, so a MIGRATED service can be a client of a
	// provider that has not been migrated yet.
	//
	// THE BRIDGE, AND IT IS TEMPORARY BY CONSTRUCTION. Every call here is a serve root the executor
	// would have kept itself; each disappears with the branch that made it, and when the ladder is
	// empty this method has no callers.
	pub(super) fn register(&mut self, service: &[u8], tag: &[u8], client: u64) {
		let Some(index) = index_of(service) else { return };
		for (slot, role) in ROLES[index].iter().enumerate() {
			if role.tag == tag && role.kind == RoleKind::ServeRoot && slot < MAX_ROLES {
				self.ends[index][slot] = client;
				return;
			}
		}
	}

	// The client end kept from `provider`'s serve root called `tag`, or 0 when there is none.
	pub(super) fn end_of(&self, provider: &[u8], tag: &[u8]) -> u64 {
		let Some(index) = index_of(provider) else { return 0 };
		for (slot, role) in ROLES[index].iter().enumerate() {
			if role.tag == tag && role.kind == RoleKind::ServeRoot && slot < MAX_ROLES {
				return self.ends[index][slot];
			}
		}
		0
	}

	// The same end, GIVEN UP: returned and forgotten, so nothing here holds it any more. What an
	// exclusive role needs, and the reason it is a separate call - a `take` that read like a `get`
	// would be a handle two places believe they own.
	pub(super) fn take_end_of(&mut self, provider: &[u8], tag: &[u8]) -> u64 {
		let Some(index) = index_of(provider) else { return 0 };
		for (slot, role) in ROLES[index].iter().enumerate() {
			if role.tag == tag && role.kind == RoleKind::ServeRoot && slot < MAX_ROLES {
				let end: u64 = self.ends[index][slot];
				self.ends[index][slot] = 0;
				return end;
			}
		}
		0
	}
}

// Send one service the roles the plan declares for it, resolving what can be resolved and taking
// the rest from `external`. Returns false at the first role that cannot be delivered.
//
// EXTERNAL IS A FUNCTION, NOT A TABLE, because what it answers is held in the supervisor's own
// locals - a block device's channel, the console-input privilege, the bytes a memory volume is
// sized with. Threading fifty named variables through here would be the ladder again with one more
// level of indirection.
pub(super) unsafe fn deliver_roles(manager_side: u64, index: usize, kept: &mut Kept, external: &mut dyn FnMut(&Role) -> Option<(alloc::vec::Vec<u8>, u64)>) -> bool {
	unsafe {
		for (slot, role) in ROLES[index].iter().enumerate() {
			// THE CALLER GETS FIRST REFUSAL ON EVERY ROLE, not only the kinds the plan cannot
			// resolve. A role can be exactly what the plan says and still need a handle the
			// supervisor already holds for a reason the plan does not carry - the shell's session
			// is minted ONCE and reused for the life of the system, so its working directory
			// survives a logout, and a freshly minted one would quietly lose that.
			//
			// The override is where the model is not finished yet. Each one names a fact the
			// manifest cannot say, which is the list of what it would have to grow to say it.
			if let Some((bytes, handle)) = external(role) {
				if !send_blocking(manager_side, &bytes, handle) {
					return false;
				}
				continue;
			}
			let delivered: bool = match role.kind {
				RoleKind::ServeRoot => {
					let mut client: u64 = 0;
					let ok = serve_root(manager_side, role.tag, &mut client);
					if ok && slot < MAX_ROLES {
						kept.ends[index][slot] = client;
					}
					ok
				}
				RoleKind::Client => {
					// EXCLUSIVE MEANS THE END ITSELF, NOT A COPY OF IT. A duplicate leaves this
					// supervisor holding the other copy for the life of the system, and a channel
					// whose peer is still open never reports its peer closed - so the provider
					// cannot tell that the service it handed the channel to has ended.
					//
					// That is not theoretical. ConsoleService reloads the shell on a VT when that
					// VT's channel closes, which is what makes `exit` on a console return a fresh
					// login prompt; handed a duplicate, the shell exits and the console waits for
					// a peer this process is still holding.
					let root: u64 = kept.end_of(role.provider, role.source);
					if root == 0 {
						// An absent provider is not a failure when the role is optional: a boot
						// with no second disk still sends the tag, carrying nothing.
						!role.required && send_blocking(manager_side, role.tag, 0)
					} else {
						// NARROWED EITHER WAY, and the exclusive one gives up the end it copied
						// from. Rights are the same question for both - a client role's ceiling is
						// send, receive, wait and transfer, and a receiver checking that would
						// refuse a raw end carrying everything the pair was made with. What
						// exclusivity changes is only how many handles are left afterwards: one,
						// held by the service, so its closing is the peer's to see.
						let copy: i64 = duplicate(root, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
						// AFTER THE COPY EXISTS, NOT BEFORE. A failed duplicate leaves the
						// supervisor holding what it held, rather than holding nothing and having
						// nothing to hand over.
						if copy > 0 && role.exclusive {
							kept.take_end_of(role.provider, role.source);
							close(root);
						}
						copy > 0 && send_blocking(manager_side, role.tag, copy as u64)
					}
				}
				RoleKind::Factory => {
					let root: u64 = kept.end_of(role.provider, role.source);
					match if root == 0 { None } else { service_connect(root) } {
						// NARROWED LIKE EVERY OTHER CHANNEL ROLE. A minted connection comes back
						// carrying every right its pair was made with, because the provider made
						// the pair - and a receiver checking the ceiling refuses that, correctly.
						// The provider decides that a connection exists; the supervisor decides
						// what the holder may do with it, and those are different questions.
						Some(connection) => {
							let copy: i64 = duplicate(connection, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
							close(connection);
							copy > 0 && send_blocking(manager_side, role.tag, copy as u64)
						}
						None => !role.required && send_blocking(manager_side, role.tag, 0),
					}
				}
				// A MESSAGE OF BYTES IS ALWAYS SENT, with or without content. `READY` is the plainest
				// case and the one that matters most: it ends the sequence, and a receiver that
				// never sees it waits forever for a send nobody will make. The tag alone is a
				// complete message when the caller supplied no bytes.
				RoleKind::Payload => send_blocking(manager_side, role.tag, 0),
				// A driver's handle or a privileged capability comes from outside the service
				// graph, and the caller was already asked above. Reaching here means it had none,
				// which for an optional role is the ordinary shape of a smaller boot.
				_ => !role.required && send_blocking(manager_side, role.tag, 0),
			};
			if !delivered {
				return false;
			}
		}
		true
	}
}

// Load a non-pinned service from its manifest-declared system-volume path through ProcessService,
// handing the new process `bootstrap` as its bootstrap channel. Mints a dedicated
// launcher connection to the `process` factory (so the client end kept for the shell
// stays pristine). Returns the new process handle, or a negative value on failure.
pub(super) unsafe fn launch_from_volume(process_client: u64, name: &[u8], bootstrap: u64) -> i64 {
	unsafe {
		if process_client == 0 {
			return -1;
		}
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => return -1,
		};
		let launcher: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return -1,
		};
		let started = process::Client::new(ChannelTransport { chan: launcher }).launch(name_str, &bootstrap);
		close(launcher);
		match started {
			Some(Ok(s)) => s.task as i64,
			_ => -1,
		}
	}
}

// Drive DeviceManager's phase 2: now that the system volume is mounted, hand
// it a fresh StorageService connection over its control channel with a "DRIVERS" message,
// so it loads the non-bootstrap drivers from vol://system/drivers/ and hands their channels
// back - the net driver's frame channel, the gpu display channel, the snd control channel,
// the pointer event channel, the USB stick's block channel (each 0 when that device is
// absent), the xHCI driver's USB bus query channel (the `lsusb` inventory) and its
// pointer-event channel (a USB pointing device). Kept for bootstrapping NetworkService,
// ConsoleService, AudioService, InputService, the usb StorageService instance and
// PermissionManager's `usb` grant against the drivers.
pub(super) unsafe fn drive_runtime_drivers(dm_control: u64, storage_client: u64, net_frames: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_raw: &mut u64, block5_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, buf: &mut [u8]) {
	unsafe {
		if dm_control == 0 {
			return;
		}
		let storage: u64 = service_connect(storage_client).unwrap_or(0);
		if !send_blocking(dm_control, b"DRIVERS", storage) {
			return;
		}
		if let Received::Message { handle: net, .. } = recv_blocking(dm_control, buf) {
			*net_frames = net;
		}
		if let Received::Message { handle: gpu, .. } = recv_blocking(dm_control, buf) {
			*gpu_client = gpu;
		}
		if let Received::Message { handle: snd, .. } = recv_blocking(dm_control, buf) {
			*snd_client = snd;
		}
		if let Received::Message { handle: input, .. } = recv_blocking(dm_control, buf) {
			*input_raw = input;
		}
		if let Received::Message { handle: usb, .. } = recv_blocking(dm_control, buf) {
			*block5_client = usb;
		}
		if let Received::Message { handle: usbq, .. } = recv_blocking(dm_control, buf) {
			*usbq_client = usbq;
		}
		if let Received::Message { handle: ptr, .. } = recv_blocking(dm_control, buf) {
			*usb_pointer = ptr;
		}
		if let Received::Message { handle: keys, .. } = recv_blocking(dm_control, buf) {
			*raw_keys = keys;
		}
	}
}

// Start one service: look it up in the package, spawn it with a fresh report
// channel, wait for its "online" report, and relay that report up to `up`. Returns
// the resulting state (Running on success, Failed otherwise).
//
// Three services are bootstrapped specially before they report in: LogService is
// handed the channel its clients reach it on (we keep the client end in
// `*log_client`); StorageService needs the disk-backed block service channel and a
// service channel (we keep the client end in `*storage_client`); the shell needs
// both client channels - the StorageService one so its `cat` round-trips, the
// LogService one so its `log` command can query the journal. Once a service reports
// in, the supervisor records a structured "online" event in the journal.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn start_service(package: &Package, kept: &mut Kept, name: &[u8], program: &[u8], pinned: bool, power: u64, display_ctl: u64, console_input: u64, console_sink: u64, device_manager: u64, live_volume: u64, up: u64, pkg_handle: u64, pkg_len: usize, registry_far: &mut u64, block_client: &mut u64, block2_client: &mut u64, block3_client: &mut u64, block4_client: &mut u64, block5_client: &mut u64, media_client: &mut u64, iso_client: &mut u64, udf_client: &mut u64, ram_client: &mut u64, tmp_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, net_frames: &mut u64, net_client: &mut u64, gpu_client: &mut u64, display_client: &mut u64, display_admin: &mut u64, snd_client: &mut u64, audio_client: &mut u64, audio_admin: &mut u64, time_client: &mut u64, console_client: &mut u64, console_control: &mut u64, storage_client: &mut u64, storage_admin: &mut u64, log_client: &mut u64, device_client: &mut u64, process_client: &mut u64, config_client: &mut u64, input_raw: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, input_client: &mut u64, input_admin: &mut u64, input_focus: &mut u64, input_kill: &mut u64, pointer_console: &mut u64, graph_client: &mut u64, perm_client: &mut u64, res_client: &mut u64, session_client: &mut u64, session1: &mut u64, admin_server: &mut u64, admin_server2: &mut u64, stats_server: &mut u64, stats_server2: &mut u64, procs: &[u64; N], state: &[State; N], proc_out: &mut u64, control: &mut u64, failure_out: &mut String, buf: &mut [u8]) -> (State, Reason) {
	unsafe {
		let (manager_side, service_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return (State::Failed, Reason::BootstrapRefused),
		};
		// The pinned bootstrap set is raw-spawned from the init package (it is on the path
		// to mounting the system volume, so it cannot load from it); every other service is
		// loaded from their manifest-declared volume paths through ProcessService. media / iso /
		// udf storage are extra instances of the pinned storage_service binary.
		let proc: i64 = if pinned {
			let mut artifact: Vec<u8> = program.to_vec();
			artifact.extend_from_slice(services::executable::SUFFIX.as_bytes());
			match package.lookup(&artifact) {
				Some(elf) => spawn(elf, service_side),
				None => return (State::Failed, Reason::BootstrapRefused),
			}
		} else {
			launch_from_volume(*process_client, program, service_side)
		};
		if proc < 0 {
			return (State::Failed, Reason::BootstrapRefused);
		}
		// Keep the spawned Process handle so SystemGraphService can be handed a read-only
		// duplicate of it (the live data source for this component's graph node).
		*proc_out = proc as u64;

		// MIGRATED TO THE PLAN. These three declare nothing but a serve root, which the executor
		// resolves entirely from the manifest - it creates the pair, sends the service end and
		// keeps the client end. There is nothing left for a branch to say about them, so they have
		// none, and `check-bootstrap-plan` no longer compares them because there is nothing to
		// compare against.
		//
		// One at a time, and the comparison first: the gate proved the executor's sequence equal to
		// the branch's before the branch was deleted. That order is the whole discipline - a
		// migration that switches and then checks has already lost the thing it would check
		// against.
		// THE PLAN IS THE DEFAULT AND THE LADDER IS THE EXCEPTION, which is the whole of M6. An
		// ordinary service of an existing shape needs a manifest row and an implementation; this
		// list is what it does NOT need an edit to, because a name that is not on it is executed
		// from the plan.
		//
		// TWO REMAIN, AND BOTH FOR A STATED REASON:
		//
		// PermissionManager holds authority to hand ON, so several of its clients need rights the
		// plan's client role does not grant - `DUPLICATE`, so it can give a sandboxed component a
		// narrowed copy. Expressing that would mean per-role rights in the manifest, which is model
		// growth for one service, and M6 leaves a branch carrying real policy where it is.
		//
		// SystemGraphService is handed one message PER RUNNING COMPONENT, each carrying that
		// component's name, its declared edges and a read-only duplicate of its Process. The plan
		// has one `NODE` role and no way to say "as many as there are"; a role that expands into a
		// variable number of messages is a model this milestone did not build.
		let hand_wired: bool = matches!(name, b"permission_manager" | b"system_graph_service");
		if !hand_wired {
			let index: usize = match index_of(name) {
				Some(index) => index,
				None => return (State::Failed, Reason::BootstrapRefused),
			};
			// THE ONE FACT THE PLAN CANNOT CARRY YET, supplied here rather than hidden in a branch.
			//
			// The shell's session is minted ONCE and reused for the life of the system, so its
			// working directory survives a logout and a reload; a fresh connection per shell would
			// lose it silently. The plan says the role is a factory of SessionService, which is
			// true - what it cannot say is that this one is cached, and that is the whole content
			// of the branch this replaces.
			// Read before the closure borrows `kept`: the executor needs it mutably to record the
			// serve roots it creates, and a closure holding a reference alongside would be two
			// borrows of one table.
			let session_root: u64 = kept.end_of(b"session_service", CAP_SERVE);
			let (fat, iso, udf, usb): (u64, u64, u64, u64) = (*block2_client, *block3_client, *block4_client, *block5_client);
			let (block, snd, frames, gpu): (u64, u64, u64, u64) = (*block_client, *snd_client, *net_frames, *gpu_client);
			let (pointer, pointer2, keys): (u64, u64, u64) = (*input_raw, *usb_pointer, *raw_keys);
			let (storage_root, storage_adm): (u64, u64) = (*storage_client, *storage_admin);
			let pointer_forward: u64 = *pointer_console;
			let mut external = |role: &Role| -> Option<(alloc::vec::Vec<u8>, u64)> {
				// The shell's session is minted ONCE and reused for the life of the system, so its
				// working directory survives a logout and a reload; a fresh connection per shell
				// would lose it silently. The plan says the role is a factory of SessionService,
				// which is true - what it cannot say is that this one is cached.
				if name == b"shell" && role.tag == CAP_SESSION {
					if *session1 == 0 {
						*session1 = service_connect(session_root)?;
					}
					let copy: i64 = duplicate(*session1, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
					if copy < 0 {
						return None;
					}
					return Some((role.tag.to_vec(), copy as u64));
				}
				// A MEMORY VOLUME IS ASKED FOR ITS SIZE, and a size is content rather than shape. A
				// payload role says a message of bytes travels here; what those bytes are is the
				// supervisor's, and putting the number in the manifest would be a policy declared
				// in the one file that is meant to describe wiring.
				if name == b"ram_storage" && role.tag == b"RAMVOL" {
					return Some((memory_volume_request(b"RAMVOL", RAM_VOLUME_BYTES), 0));
				}
				if name == b"tmp_storage" && role.tag == b"TMPVOL" {
					return Some((memory_volume_request(b"TMPVOL", TMP_VOLUME_BYTES), 0));
				}
				// A BLOCK SERVICE COMES FROM A DRIVER, not from the service graph: DeviceManager
				// routes it up and this supervisor is holding it. The plan can say a device role
				// arrives under this tag and cannot say which local holds it.
				if name == b"media_storage" && role.tag == b"FATBLOCK" {
					return Some((role.tag.to_vec(), fat));
				}
				if name == b"iso_storage" && role.tag == b"ISOBLOCK" {
					return Some((role.tag.to_vec(), iso));
				}
				if name == b"udf_storage" && role.tag == b"UDFBLOCK" {
					return Some((role.tag.to_vec(), udf));
				}
				// USB IS THE ONE THAT NEEDS A STAND-IN RATHER THAN NOTHING. With no xhci driver the
				// other three instances take a zero handle and mount lazily, but this one talks to
				// its block service during bring-up: handed nothing it would wait, and handed a
				// channel whose far end is already closed it gets the refusal it can act on. An
				// absent device and a dead one are the same answer to a caller, which is the point.
				if name == b"usb_storage" && role.tag == b"USBBLOCK" {
					if usb != 0 {
						return Some((role.tag.to_vec(), usb));
					}
					let (dead_server, dead_client): (u64, u64) = channel()?;
					close(dead_server);
					return Some((role.tag.to_vec(), dead_client));
				}
				// THE DISK OR THE IMAGE, IN THE SAME POSITION. A live system serves its volume from a
				// filesystem image copied into memory and an installed one from the disk, and the
				// two arrive under different tags in the one place the plan has for them. Sending
				// an extra message instead would shift every read after it - the desyncs that cost
				// this milestone the most all came from exactly that.
				if name == b"storage_service" && role.tag == b"BLOCK" {
					return if live_volume != 0 { Some((b"LIVEVOL".to_vec(), live_volume)) } else { Some((role.tag.to_vec(), block)) };
				}
				if name == b"audio_service" && role.tag == b"SND" {
					return Some((role.tag.to_vec(), snd));
				}
				if name == b"network_service" && role.tag == b"FRAMES" {
					return Some((role.tag.to_vec(), frames));
				}
				if name == b"display_service" && role.tag == b"GPU" {
					return Some((role.tag.to_vec(), gpu));
				}
				// A PRIVILEGE IS THE KERNEL'S, HANDED ON. Duplicated rather than transferred,
				// because this supervisor keeps its own copy for whoever needs one next.
				//
				// AND THE TAG TRAVELS EVEN WHEN THE PRIVILEGE DOES NOT. `send_privilege` sent
				// NOTHING for a zero handle, while DisplayService reads this position with a
				// blocking tagged receive - a machine without the privilege would have waited here
				// for a message nobody was going to send. The plan's rule that the tag always
				// travels is what removes that.
				if name == b"display_service" && role.tag == b"DISPLAYCTL" {
					if display_ctl == 0 {
						return Some((role.tag.to_vec(), 0));
					}
					let copy: i64 = duplicate(display_ctl, RIGHT_TRANSFER | RIGHT_DUPLICATE);
					return if copy > 0 { Some((role.tag.to_vec(), copy as u64)) } else { None };
				}
				// THE RAW EVENT CHANNELS COME FROM DRIVERS, routed up by DeviceManager. A zero handle
				// is an absent pointer source, and InputService serves an empty stream rather than
				// refusing to start.
				if name == b"input_service" {
					if role.tag == b"INPUT" {
						return Some((role.tag.to_vec(), pointer));
					}
					if role.tag == b"INPUT2" {
						return Some((role.tag.to_vec(), pointer2));
					}
					if role.tag == b"KEYS" {
						return Some((role.tag.to_vec(), keys));
					}
				}
				// CONFIGSERVICE GETS A CLIENT SCOPED TO ONE DIRECTORY, minted from StorageService's
				// admin endpoint rather than duplicated from its public root. The plan can say the
				// role is a factory of that endpoint; the directory is the part it cannot say, and
				// this is the whole content of the branch it replaces.
				if name == b"config_service" && role.tag == CAP_STORAGE {
					if storage_root == 0 {
						return Some((role.tag.to_vec(), 0));
					}
					let scoped: u64 = open_storage_directory(storage_adm, "vol://system/libexec/config_service");
					return if scoped != 0 { Some((role.tag.to_vec(), scoped)) } else { None };
				}
				// THE INIT PACKAGE, under the rights a launcher needs: read it, map it, pass it on.
				// The message carries its length behind the tag because a memory object does not
				// say how much of itself is the archive.
				if role.tag == b"PACKAGE" {
					let dup: i64 = duplicate(pkg_handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
					if dup < 0 {
						return None;
					}
					let mut message: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
					message.extend_from_slice(b"PACKAGE");
					message.extend_from_slice(&(pkg_len as u64).to_le_bytes());
					return Some((message, dup as u64));
				}
				// DEVICEMANAGER CARRIES THE POWER PATH TO THE KEYBOARD DRIVERS, because the Power
				// key must keep working when this supervisor does not - the whole reason it is a
				// separate path from `!poweroff`. What travels is a SystemPower connection, not the
				// root Domain that used to.
				if name == b"device_manager" {
					if role.tag == b"SYSPOWER" {
						return Some((role.tag.to_vec(), service_connect(power)?));
					}
					if role.tag == b"CONSOLE" || role.tag == b"DEVPRIV" {
						let privilege: u64 = if role.tag == b"CONSOLE" { console_input } else { device_manager };
						if privilege == 0 {
							return Some((role.tag.to_vec(), 0));
						}
						let copy: i64 = duplicate(privilege, RIGHT_TRANSFER | RIGHT_DUPLICATE);
						return if copy > 0 { Some((role.tag.to_vec(), copy as u64)) } else { None };
					}
				}
				// PROCESSSERVICE HOLDS AN END NOBODY IS ON YET. The development agent is started
				// later, by DeviceManager, and takes the far end then; making the pair here is what
				// keeps ProcessService from having to learn about a capability that arrives after
				// it has begun serving.
				if name == b"process_service" && role.tag == b"REGISTRY" {
					let (far, near): (u64, u64) = channel()?;
					*registry_far = far;
					return Some((role.tag.to_vec(), near));
				}
				if name == b"console_service" {
					if role.tag == b"CONSOLESINK" {
						if console_sink == 0 {
							return Some((role.tag.to_vec(), 0));
						}
						let copy: i64 = duplicate(console_sink, RIGHT_TRANSFER | RIGHT_DUPLICATE);
						return if copy > 0 { Some((role.tag.to_vec(), copy as u64)) } else { None };
					}
					// The pointer-forward end InputService was given the other half of. It is a
					// handle this supervisor is holding for a service that starts later, which the
					// plan calls a device role because that is where it comes from.
					if role.tag == CAP_POINTER {
						return Some((role.tag.to_vec(), pointer_forward));
					}
				}
				None
			};
			if !deliver_roles(manager_side, index, kept, &mut external) {
				return (State::Failed, Reason::BootstrapRefused);
			}
			// THE ENDS THIS SUPERVISOR KEEPS, copied out of the plan's own table into the names the
			// remaining hand-written branches still read. Every line here goes when the branch that
			// reads it goes, so this block empties as the ladder does.
			match name {
				b"log_service" => *log_client = kept.end_of(name, CAP_SERVE),
				b"device_service" => *device_client = kept.end_of(name, CAP_SERVE),
				b"session_service" => *session_client = kept.end_of(name, CAP_SERVE),
				b"time_service" => *time_client = kept.end_of(name, CAP_SERVE),
				// The shell's own serve root is its ADMIN channel, which this supervisor answers on.
				b"shell" => *admin_server = kept.end_of(name, b"ADMIN"),
				b"ram_storage" => *ram_client = kept.end_of(name, CAP_SERVE),
				b"tmp_storage" => *tmp_client = kept.end_of(name, CAP_SERVE),
				b"media_storage" => *media_client = kept.end_of(name, CAP_SERVE),
				b"iso_storage" => *iso_client = kept.end_of(name, CAP_SERVE),
				b"udf_storage" => *udf_client = kept.end_of(name, CAP_SERVE),
				b"usb_storage" => *usb_client = kept.end_of(name, CAP_SERVE),
				b"storage_service" => {
					*storage_client = kept.end_of(name, CAP_SERVE);
					*storage_admin = kept.end_of(name, b"ADMIN");
				}
				b"audio_service" => {
					*audio_client = kept.end_of(name, CAP_SERVE);
					*audio_admin = kept.end_of(name, b"ADMIN");
				}
				b"network_service" => *net_client = kept.end_of(name, CAP_SERVE),
				b"display_service" => {
					*display_client = kept.end_of(name, CAP_SERVE);
					*display_admin = kept.end_of(name, b"ADMIN");
				}
				b"input_service" => {
					*input_client = kept.end_of(name, CAP_SERVE);
					*input_admin = kept.end_of(name, b"ADMIN");
					*input_focus = kept.end_of(name, b"FOCUS");
					*input_kill = kept.end_of(name, b"KILL");
					*pointer_console = kept.end_of(name, b"FORWARD");
				}
				b"config_service" => *config_client = kept.end_of(name, CAP_SERVE),
				b"resource_manager" => *res_client = kept.end_of(name, CAP_SERVE),
				b"process_service" => *process_client = kept.end_of(name, CAP_SERVE),
				b"console_service" => {
					*console_client = kept.end_of(name, CAP_CLIENT);
					*console_control = kept.end_of(name, CAP_CONTROL);
				}
				_ => {}
			}
		}
		// DeviceManager also carries the power capability, because it is what starts the
		// keyboard drivers and the Power key must keep working when this supervisor does not -
		// that is the whole reason the key exists as a separate path from `!poweroff`.
		// DEVPRIV is appended AFTER CONSOLE, and every launcher of device_manager owes it: the
		// bootstrap is read positionally, so `recv_tagged` checks the tag of the next message rather
		// than searching for one, and anything inserted in the middle shifts every read after it.
		if name == b"system_graph_service" && !bootstrap_system_graph_service(manager_side, procs, state, *device_client, graph_client, stats_server) {
			return (State::Failed, Reason::BootstrapRefused);
		}
		if name == b"permission_manager" && !bootstrap_permission_manager(manager_side, *storage_admin, *storage_client, *media_client, *iso_client, *udf_client, *usb_client, *ram_client, *tmp_client, *usbq_client, *log_client, *net_client, *time_client, *config_client, *device_client, *audio_client, *display_admin, *input_admin, *audio_admin, *res_client, *process_client, session_client, session1, perm_client, admin_server2, stats_server2) {
			return (State::Failed, Reason::BootstrapRefused);
		}
		match recv_blocking(manager_side, buf) {
			Received::Message { len, handle } => {
				// A service that could not complete a bootstrap step reports the failing step
				// and the reason (BOOTSTRAP_FAILURE) in place of its "online" report: record it
				// so the supervisor status and the journal explain the failure, instead of the
				// supervisor seeing an unexplained peer-close.
				if len >= BOOTSTRAP_FAILURE.len() && &buf[..BOOTSTRAP_FAILURE.len()] == BOOTSTRAP_FAILURE {
					let start: usize = (BOOTSTRAP_FAILURE.len() + 1).min(len);
					*failure_out = String::from_utf8_lossy(&buf[start..len]).into_owned();
					emit_event(*log_client, name, failure_out.as_bytes());
					return (State::Failed, Reason::BootstrapRefused);
				}
				// DeviceManager hands its block-read service channel up with its report;
				// keep it so StorageService can be bootstrapped against the disk.
				if name == b"device_manager" {
					*block_client = handle;
				}
				// Relay the service's own report up to SystemManager, in start order, and
				// keep its report channel as the control channel used to stop it later.
				send_blocking(up, &buf[..len], 0);
				*control = manager_side;
				// Record the lifecycle event in the journal (LogService is up by now).
				emit_event(*log_client, name, b"online");
				// The shell is reaped by its console channel closing when it logs out
				// (Ctrl+D) or exits; release our Process handle to it so a clean exit
				// drops its handle table - and thus that channel. A leaked handle would
				// pin the shell alive forever, so the console could never reap the VT.
				// (Every other service is meant to stand for the life of the system.)
				if name == b"shell" {
					close(proc as u64);
					*proc_out = 0;
				}
				// DeviceManager sends a follow-up "BLOCK2" message carrying the second disk's
				// block service channel, then "BLOCK3" and "BLOCK4" for the third and fourth
				// disks; keep them to bootstrap the media / iso / udf StorageService instances
				// (each handle is 0 when that disk is absent). The net / gpu / snd / input
				// driver channels arrive later, in DeviceManager's phase 2, once the volume they
				// load from is mounted (driven right after StorageService comes up, below).
				if name == b"device_manager" {
					if let Received::Message { handle: block2, .. } = recv_blocking(manager_side, buf) {
						*block2_client = block2;
					}
					if let Received::Message { handle: block3, .. } = recv_blocking(manager_side, buf) {
						*block3_client = block3;
					}
					if let Received::Message { handle: block4, .. } = recv_blocking(manager_side, buf) {
						*block4_client = block4;
					}
				}
				// PermissionManager follows its "online" report with the sandbox proof: the
				// bytes the sandboxed component read through its one granted capability, then a
				// decisions summary of exactly which capabilities it was and was not given.
				// These are the manager's internal verification (and are asserted by the
				// kernel's permission scenario); the live audit trail is served over the
				// Permission contract and read with `perm`, so they are drained here rather
				// than relayed into the boot chain, which carries only state reports.
				if name == b"permission_manager" {
					let _ = recv_blocking(manager_side, buf);
					let _ = recv_blocking(manager_side, buf);
				}
				// ResourceManager follows its "online" report with the budget proof: a summary of
				// the pages it granted under the cap, the over-budget refusal it contained, and the
				// pages it regranted after raising the budget at runtime. This is the manager's
				// internal verification (and is asserted by the kernel's resource scenario); the
				// live budgets are served over the resources contract and read with `usage`, so it
				// is drained here rather than relayed into the boot chain, which carries only state
				// reports.
				if name == b"resource_manager" {
					let _ = recv_blocking(manager_side, buf);
				}
				// THE BRIDGE, WHILE BOTH DESCRIPTIONS EXIST. A hand-written branch keeps its serve
				// roots in named locals; a migrated service resolves its clients out of `Kept`. So
				// every end a branch keeps is recorded here under the name and tag the plan calls
				// it, and a service can be migrated before the ones it is a client of.
				//
				// TEMPORARY BY CONSTRUCTION: each line goes with the branch that made it, and when
				// the ladder is empty this block is empty too.
				match name {
					b"system_graph_service" => kept.register(name, CAP_SERVE, *graph_client),
					b"permission_manager" => kept.register(name, CAP_SERVE, *perm_client),
					_ => {}
				}
				(State::Ready, Reason::ReportedReady)
			}
			Received::Closed => {
				// The service closed its bootstrap channel without reporting - it crashed during
				// bring-up before it could send a failure report. Record that so the status view
				// still carries a reason rather than a bare "failed".
				*failure_out = String::from("bootstrap channel closed without a report");
				(State::Failed, Reason::NoReport)
			}
		}
	}
}

// Emit one structured Entry to LogService over the `log_client` channel: an Info
// record tagged with the service `source` and an `event` field (e.g.
// "online"/"stopped"). A no-op until LogService is up (log_client == 0). The
// supervisor logs service lifecycle the way systemd journals unit start/stop.
pub(super) unsafe fn emit_event(log_client: u64, source: &[u8], event: &[u8]) {
	if log_client == 0 {
		return;
	}
	let entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from_utf8_lossy(source).into_owned(), fields: alloc::vec![Field { key: String::from("event"), value: String::from_utf8_lossy(event).into_owned() }] };
	// Emit the record through the generated Log client (a round-trip over the log
	// channel); best-effort, so the result is ignored.
	let mut client = log::Client::new(ChannelTransport { chan: log_client });
	let _ = client.emit(&entry);
}

// Mirror a runtime service transition to the debug console, so an operator watching
// the console sees a service stop, crash, or restart the moment it happens - the
// journal carries the same event for `log`, but a state change must never be silent.
// Bring-up reports are not mirrored here: the boot chain already prints those.
pub(super) unsafe fn console_report(source: &[u8], event: &[u8]) {
	let mut line: Vec<u8> = Vec::new();
	line.extend_from_slice(b"supervisor: ");
	line.extend_from_slice(source);
	line.push(b' ');
	line.extend_from_slice(event);
	line.push(b'\n');
	unsafe { print(&line) };
}

// Stop a running service over its control channel: send the "STOP" sentinel, then
// wait for the service's "stopped" acknowledgement and relay it up like its start
// report. Returns Stopped on a clean shutdown (or if the service was already gone).
pub(super) unsafe fn stop_service(control: u64, up: u64, buf: &mut [u8]) -> State {
	unsafe {
		if control == 0 || !send_blocking(control, b"STOP", 0) {
			return State::Failed;
		}
		if let Received::Message { len, .. } = recv_blocking(control, buf) {
			send_blocking(up, &buf[..len], 0);
		}
		State::Stopped
	}
}

// Register every observed component with SystemGraphService and hand it the live data
// sources for the graph: one "NODE" message per Running component (excluding the shell
// and SystemGraphService itself), carrying the component's name and its declared
// dependency edges as the payload and a read-only duplicate of that component's Process
// as the transferred handle (the source of its live counters and state), then a
// dedicated DeviceService connection ("DEVICE") for the device nodes, then a fresh
// "SUPERVISOR" channel (the supervisor keeps the server end in `*stats_server` and
// serves the supervisor interface on it, so SystemGraphService can merge restart /
// watchdog counters into the graph), and finally the channel its clients reach it on
// ("SERVE"), kept in `*graph_client` for the shell. SystemGraphService comes up after
// every component it observes, so their handles are all captured and their state is
// Running when its node set is built.
pub(super) unsafe fn bootstrap_system_graph_service(manager_side: u64, procs: &[u64; N], state: &[State; N], device_client: u64, graph_client: &mut u64, stats_server: &mut u64) -> bool {
	unsafe {
		let mut i: usize = 0;
		while i < N {
			let name: &[u8] = MANIFEST[i].name;
			if state[i] == State::Ready && procs[i] != 0 && name != b"shell" && name != b"system_graph_service" {
				let dup: i64 = duplicate(procs[i], RIGHT_READ | RIGHT_TRANSFER);
				if dup < 0 {
					return false;
				}
				let mut payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
				payload.extend_from_slice(b"NODE");
				payload.extend_from_slice(name);
				payload.push(b'\n');
				let mut first: bool = true;
				for &dep in MANIFEST[i].deps {
					if !first {
						payload.push(b',');
					}
					payload.extend_from_slice(dep);
					first = false;
				}
				if !send_blocking(manager_side, &payload, dup as u64) {
					return false;
				}
			}
			i += 1;
		}
		// A dedicated DeviceService connection for the device nodes, minted from the
		// supervisor's DeviceService client so it never races the shell's own connection.
		match service_connect(device_client) {
			Some(dev) => {
				if !send_blocking(manager_side, b"DEVICE", dev) {
					return false;
				}
			}
			None => return false,
		}
		// A fresh SUPERVISOR channel: the supervisor keeps the server end (in
		// `*stats_server`) and serves the supervisor interface on it, so SystemGraphService
		// can query restart / watchdog counters and merge them into the graph. Sent right
		// after DEVICE to match SystemGraphService's receive order.
		let (stats_srv, stats_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SUPERVISOR", stats_cli) {
			return false;
		}
		*stats_server = stats_srv;
		// The channel its clients (the shell) reach it on; the client end is kept in
		// `*graph_client` for the shell's own bootstrap.
		bootstrap_serve(manager_side, graph_client)
	}
}

// Hand PermissionManager the clients it may grant onward - a fresh StorageService
// connection, a duplicable LogService client, a fresh NetworkService connection (so it
// holds, and can be seen to withhold, a capability it possesses), a fresh TimeService
// connection (the one capability the governed `date` command may reach), then fresh
// ConfigService, DeviceService, and AudioService connections (the capabilities the governed
// `config` / `set`, `lsdev`, and `beep` commands may reach), then private display/input/audio
// admin clients for scoped graphical grants, followed by a fresh ProcessService
// connection (the loading mechanism it drives to start the components it governs) and the
// channel its clients reach it on ("SERVE", the client end kept in `*perm_client` for the
// shell's `perm` command). The order matches PermissionManager's receive order: STORAGE,
// LOG, NETWORK, TIME, CONFIG, DEVICE, AUDIO, DISPLAY_ADMIN, INPUT_ADMIN, AUDIO_ADMIN,
// RESOURCE, PROCESS_GRANT, PROCESS, SERVE. The
// grantable clients carry RIGHT_DUPLICATE so the manager can attenuate and hand a strictly
// narrower client to each component it sandboxes. (The grantable permission capability - a
// connection to the manager's own serve channel - is not passed here: the manager mints that
// self-connection itself.)
unsafe fn bootstrap_permission_manager(manager_side: u64, storage_admin: u64, storage_client: u64, media_client: u64, iso_client: u64, udf_client: u64, usb_client: u64, ram_client: u64, tmp_client: u64, usbq_client: u64, log_client: u64, net_client: u64, time_client: u64, config_client: u64, device_client: u64, audio_client: u64, display_admin: u64, input_admin: u64, audio_admin: u64, resource_client: u64, process_client: u64, session_client: &mut u64, session1: &mut u64, perm_client: &mut u64, admin_server2: &mut u64, stats_server2: &mut u64) -> bool {
	unsafe {
		// A fresh StorageService connection for the manager (independent of the shell's),
		// duplicable so the manager can grant a narrowed copy to a sandboxed component.
		let storage: u64 = match service_connect(storage_client) {
			Some(h) => h,
			None => return false,
		};
		// THE ADMIN ENDPOINT, DUPLICATED AND NEVER GRANTED ONWARD. PermissionManager mints
		// directory-confined clients from it for `app-assets`; it is the thing that hands out a
		// narrowed authority, so it must not itself be one of the things handed out - and the
		// manager's own grant table has no entry that would.
		//
		// A duplicate rather than the endpoint: the supervisor keeps minting from it too, for
		// ConfigService's persistence and the journal.
		let admin_dup: i64 = duplicate(storage_admin, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if admin_dup < 0 || !send_blocking(manager_side, CAP_STORAGE_ADMIN, admin_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, CAP_STORAGE, storage) {
			return false;
		}
		// A duplicable LogService client, so the manager can grant a narrowed copy.
		let log_dup: i64 = duplicate(log_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER | RIGHT_DUPLICATE);
		if log_dup < 0 || !send_blocking(manager_side, CAP_LOG, log_dup as u64) {
			return false;
		}
		// A fresh NetworkService connection the manager holds but withholds from the
		// sandboxed probe (whose manifest does not grant network) - the policy actively
		// declines to pass on a capability it possesses.
		let mut net = network::Client::new(ChannelTransport { chan: net_client });
		let perm_net: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		if !send_blocking(manager_side, CAP_NETWORK, perm_net) {
			return false;
		}
		// A fresh TimeService connection the manager grants to the governed `date` command
		// (whose manifest grants time) - the one capability that command is allowed to reach.
		let time_conn: u64 = match service_connect(time_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_TIME, time_conn) {
			return false;
		}
		// VT 1's SESSION, duplicated for the manager to grant to the governed `kill` command.
		//
		// The SAME session the shell holds, minted once and kept for the life of the system - so
		// `kill 2` names the job `jobs` printed. When a second VT session exists this becomes
		// wrong in a specific way: the launcher would have to pass the CALLER's session rather
		// than this one, because a job table belongs to a session and not to the machine. There is
		// one session today, and this comment is where that assumption is written down.
		//
		// OPTIONAL, and this is the important part. Every other grant here is one PermissionManager
		// cannot work without; this one is needed by a single governed command. Making it fatal
		// made the whole console chain depend on SessionService answering a connect at this
		// instant - PermissionManager fails, `perm_client` stays zero, and ConsoleService, the
		// system graph and the shell all fail after it, so the system boots to "no interactive
		// shell attached" because `kill` could not have been granted. A capability the manager can
		// live without must not be able to stop it starting.
		//
		// Absent is SAID rather than implied: the tag is sent with no handle (the form the
		// manager already reads as "not granted"), and the line below is what tells an operator
		// why `kill` refuses instead of leaving them to find out by running it.
		if *session1 == 0 {
			*session1 = service_connect(*session_client).unwrap_or(0);
		}
		let session_dup: u64 = if *session1 == 0 {
			0
		} else {
			match duplicate(*session1, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER) {
				handle if handle >= 0 => handle as u64,
				_ => 0,
			}
		};
		if session_dup == 0 {
			print(b"ServiceManager: no session client for PermissionManager; the `kill` command will not be grantable\n");
		}
		if !send_blocking(manager_side, CAP_SESSION, session_dup) {
			return false;
		}
		// A fresh ConfigService connection the manager grants to the governed `config` / `set`
		// commands (whose manifests grant config).
		let config_conn: u64 = match service_connect(config_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_CONFIG, config_conn) {
			return false;
		}
		// A fresh DeviceService connection the manager grants to the governed `dev` command
		// (whose manifest grants device).
		let device_conn: u64 = match service_connect(device_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_DEVICE, device_conn) {
			return false;
		}
		// A fresh AudioService connection the manager grants to the governed `beep` command
		// (whose manifest grants audio).
		let audio_conn: u64 = match service_connect(audio_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_AUDIO, audio_conn) {
			return false;
		}
		if !send_blocking(manager_side, CAP_DISPLAY_ADMIN, display_admin) {
			return false;
		}
		if !send_blocking(manager_side, CAP_INPUT_ADMIN, input_admin) {
			return false;
		}
		if !send_blocking(manager_side, CAP_AUDIO_ADMIN, audio_admin) {
			return false;
		}
		// A fresh ResourceManager connection the manager grants to the governed `usage` command
		// (whose manifest grants resource).
		let resource_conn: u64 = match service_connect(resource_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_RESOURCE, resource_conn) {
			return false;
		}
		// A fresh ProcessService connection the manager grants to the governed `ps` command
		// (whose manifest grants process) - a dedicated connection, separate from the launch
		// mechanism below, so a granted tool's queries never interleave with the manager's loads.
		let process_grant: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_PROCESS_GRANT, process_grant) {
			return false;
		}
		// A fresh admin channel the manager grants to the governed `stop` command (whose
		// manifest grants supervisor): the supervisor keeps the server end (in `*admin_server2`)
		// and stands on it in the supervise loop, while the client end is handed to the manager,
		// which duplicates a narrowed copy onto the sandboxed `stop` tool. A dedicated channel,
		// separate from the shell's own admin channel, so a granted tool's teardown requests
		// never race the shell's built-in `stop`.
		let (admin_srv2, admin_cli2): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_SUPERVISOR, admin_cli2) {
			return false;
		}
		*admin_server2 = admin_srv2;
		// Six fresh non-system volume StorageService connections the manager bundles with the
		// system `storage` client under the `volumes` capability it grants the governed `lsvol`
		// command: media (FAT/exFAT), iso (ISO9660), udf (UDF), usb (FAT off the USB stick), and
		// the two memory volumes ram (reserved) and tmp (capped).
		// Each is minted off the volume's own service factory; a volume whose disk is absent
		// has no factory (its client is 0) and is handed over as 0, which `lsvol` shows as
		// zero files.
		let media_conn: u64 = service_connect(media_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_MEDIA, media_conn) {
			return false;
		}
		let iso_conn: u64 = service_connect(iso_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_ISO, iso_conn) {
			return false;
		}
		let udf_conn: u64 = service_connect(udf_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_UDF, udf_conn) {
			return false;
		}
		let usb_conn: u64 = service_connect(usb_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_USB, usb_conn) {
			return false;
		}
		// The two memory volumes, bundled the same way. They have no disk to be absent, so a 0
		// here means only that their service did not come up.
		let ram_conn: u64 = service_connect(ram_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_RAM, ram_conn) {
			return false;
		}
		let tmp_conn: u64 = service_connect(tmp_client).unwrap_or(0);
		if !send_blocking(manager_side, CAP_STORAGE_TMP, tmp_conn) {
			return false;
		}
		// A fresh supervisor-status channel the manager grants to the governed `lssvc` command
		// (whose manifest grants services): the supervisor keeps the server end (in
		// `*stats_server2`) and serves the `supervisor` interface on it alongside
		// SystemGraphService's, while the client end is handed to the manager. A dedicated
		// channel, so a granted tool's queries never race the graph's.
		let (status_srv, status_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_SERVICES, status_cli) {
			return false;
		}
		*stats_server2 = status_srv;
		// The xHCI driver's USB bus query channel the manager grants to the governed `lsusb`
		// command (whose manifest grants usb): handed up by DeviceManager in phase 2, held by
		// the supervisor until here (0 when the driver never came up - the manager simply
		// cannot grant what it does not hold).
		if !send_blocking(manager_side, CAP_USBBUS, usbq_client) {
			return false;
		}
		// A fresh ProcessService connection the manager drives to load the components it
		// governs - the loading mechanism, kept separate from the granting policy.
		let proc_conn: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_PROCESS, proc_conn) {
			return false;
		}
		// The channel its clients reach it on; the client end kept for the shell.
		if !bootstrap_serve(manager_side, perm_client) {
			return false;
		}
		// END the run. Without it the receiver cannot tell "the parent sent everything" from
		// "the parent is not finished yet", which is what made a receive with no matching send
		// wait forever - or worse, take the next handoff in the sequence and read it as its own.
		send_ready(manager_side)
	}
}

// Hand ResourceManager a read-only view of the init package (to launch the component it
// governs from) and the channel its clients reach it on ("SERVE", the client end kept in
// `*res_client` for the shell's `usage` command), then a ProcessService client. The order
// matches ResourceManager's receive order: PACKAGE, SERVE, PROCESS.
//
// The ProcessService client is the one grantable client this manager holds, and it is held
// to read rather than to grant: every governed launch runs in a Domain of its own, those
// Domains are invisible to a manager that only knows the ones it created, and `accounting`
// answers with values. The manager still governs its own component's Domain through the
// kernel's resource syscalls, not by granting service connections.
//
// Hand a service the channel its clients reach it on: create a fresh service channel
// and transfer one end with the "SERVE" tag, keeping the other end in `*client` for
// the supervisor to later hand to the shell. The shared bootstrap for every SERVE-
// only service (Log, Device, Config) and the tail of Storage and Process.
pub(super) unsafe fn bootstrap_serve(manager_side: u64, client: &mut u64) -> bool {
	unsafe { serve_root(manager_side, CAP_SERVE, client) }
}

// Hand over one end of a fresh channel pair as a serve root, NARROWED TO WHAT SERVING NEEDS.
//
// A fresh channel end carries `RIGHTS_ALL`, and this used to transfer it exactly as the kernel
// minted it - so every service was handed MANAGE, DUPLICATE and REVOKE over the channel its
// clients reach it on. None of the three is used by serving, and REVOKE over one's own service
// channel is authority nobody asked for and nothing needs.
//
// Send, receive and wait are what a serve loop does. TRANSFER is unavoidable: a capability that
// cannot be transferred cannot be delivered, so it is in the grant by construction rather than by
// choice.
pub(super) unsafe fn serve_root(manager_side: u64, tag: &[u8], client: &mut u64) -> bool {
	unsafe {
		let (service_server, service_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		let narrowed: i64 = duplicate(service_server, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		close(service_server);
		if narrowed < 0 {
			close(service_client);
			return false;
		}
		if !send_blocking(manager_side, tag, narrowed as u64) {
			close(service_client);
			return false;
		}
		*client = service_client;
		true
	}
}

// Mint a client confined to one declared system-volume directory. The caller keeps the
// private admin endpoint; ordinary volume clients never receive it and can only mint
// another client with their existing scope.
pub(super) unsafe fn open_storage_directory(storage_admin: u64, path: &str) -> u64 {
	if storage_admin == 0 {
		return 0;
	}
	match volume_admin::Client::new(ChannelTransport { chan: storage_admin }).open_directory(path) {
		Some(Ok(client)) => client,
		_ => 0,
	}
}

// The two memory StorageService instances. Unlike every other volume there is no block service to
// hand over - the filesystem holds its files on the heap - so the tag carries the capacity in bytes
// instead of a handle.
//
// `vol://ram` is reserved: it takes its memory when it mounts, so a later write cannot fail because
// something else took it. `vol://tmp` is capped: it holds only what is stored and refuses the write
// that would cross the limit. One filesystem, two moments of charging.
const RAM_VOLUME_BYTES: usize = 4 * 1024 * 1024;
const TMP_VOLUME_BYTES: usize = 16 * 1024 * 1024;

// The bytes a memory volume is asked for: its tag followed by its size in decimal. A PAYLOAD role
// carries content, and content is the one thing the plan cannot state - a number in the manifest
// would be a policy nobody declared there.
fn memory_volume_request(tag: &[u8], bytes: usize) -> Vec<u8> {
	{
		let mut request: Vec<u8> = Vec::new();
		request.extend_from_slice(tag);
		let mut digits = [0u8; 20];
		let mut value = bytes;
		let mut at = digits.len();
		if value == 0 {
			at -= 1;
			digits[at] = b'0';
		}
		while value != 0 {
			at -= 1;
			digits[at] = b'0' + (value % 10) as u8;
			value /= 10;
		}
		request.extend_from_slice(&digits[at..]);
		request
	}
}
