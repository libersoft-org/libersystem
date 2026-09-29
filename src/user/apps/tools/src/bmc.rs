// bmc - the baseboard management controllers the system's IPMI interfaces reach: who each is, its sensors, its event
// log, its FRU inventory, its chassis and its LAN configuration.
//
//   bmc                                    every BMC: its name, its interfaces, a pair reached twice
//   bmc info [BMC]                         the device, the firmware and the interfaces
//   bmc sensors [BMC]                      every sensor the repository declares, read now
//   bmc sel [BMC] [--follow]               the event log - and, with --follow, each entry it gains from now on
//   bmc sel clear [BMC]                    ask for the log to be erased, counting what it holds now
//   bmc fru [BMC]                          the FRU inventory
//   bmc chassis [BMC]                      the chassis status
//   bmc chassis identify [BMC] SECONDS|off the identify light
//   bmc chassis power-down|power-cycle|hard-reset|soft-shutdown [BMC]
//   bmc lan [BMC]                          each LAN channel's configuration
//   bmc users [BMC]                        each LAN channel's users
//
// BMC is the name `bmc` prints - `bmc:` and a GUID's hex digits, or its fallback - and may be left out where there is
// one. READING IS THIS PROGRAM'S; ERASING AND STOPPING ARE NOT. `sel clear` and the chassis operations are REQUESTS: one
// is put to AdminService on the request connection PermissionManager minted for this launch - the `bmc` scope, those
// two actions on `bmc:` targets - and only a person's confirmation on the protected screen turns it into one attempt.
// A BMC that is unavailable, or refuses, is said to be, with its completion code.

#![no_std]
#![no_main]

extern crate alloc;

use admin_client::{AdminAuthorityClient, AdminRequestClient};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use bmc_client::BmcClient;
use proto::generated::liber::bmc::v1::{BmcSummary, Ours, Outcome, ReadingState, Status};
use proto::system::{AdminAction, AdminAnswer, AdminRequestArgs, AdminResult, LaunchContext};
use rt::*;

// A followed log is read this often.
const FOLLOW_TICKS: u64 = 5 * TICKS_PER_SECOND;

fn out(line: &str) {
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	eprint(b"bmc: ");
	eprint(line.as_bytes());
	eprint(b"\n");
	exit();
}

// WHAT THE BMC DID, when it did not answer: unavailable, refused with its code, or answered with something refused.
fn check(name: &str, status: &Status) -> bool {
	match status.outcome {
		Outcome::Answered => true,
		Outcome::Refused => {
			out(&format!("bmc: {name}: the BMC refused it - completion code {:#04x}", status.cc));
			false
		}
		Outcome::Unavailable => {
			out(&format!("bmc: {name}: the BMC is unavailable"));
			false
		}
		Outcome::Malformed => {
			out(&format!("bmc: {name}: the BMC's answer was malformed and refused"));
			false
		}
	}
}

fn milli(value: i64) -> String {
	let sign = if value < 0 { "-" } else { "" };
	let magnitude = value.unsigned_abs();
	format!("{sign}{}.{:03}", magnitude / 1000, magnitude % 1000)
}

// THE UNIT a sensor's base unit code names, for the ones a server's sensors are in; the code otherwise.
fn unit(code: u8) -> String {
	String::from(match code {
		1 => "C",
		2 => "F",
		3 => "K",
		4 => "V",
		5 => "A",
		6 => "W",
		7 => "J",
		18 => "RPM",
		19 => "Hz",
		20 => "us",
		21 => "ms",
		22 => "s",
		_ => return format!("unit {code}"),
	})
}

fn ipv4(bytes: &[u8]) -> String {
	if bytes.len() == 4 { format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]) } else { String::from("-") }
}

// The BMC a command names, or the only one there is.
fn choose(bmcs: &[BmcSummary], named: Option<&str>) -> BmcSummary {
	match named {
		Some(name) => match bmcs.iter().find(|bmc| bmc.name == name) {
			Some(bmc) => bmc.clone(),
			None => fail(&format!("{name}: no such BMC - `bmc` lists them")),
		},
		None => match bmcs {
			[only] => only.clone(),
			[] => fail("this machine has no BMC this system reaches"),
			_ => fail("there are several BMCs - name one, as `bmc` prints it"),
		},
	}
}

fn print_summary(bmc: &BmcSummary) {
	let device = &bmc.device;
	out(&format!("{}{}{}", bmc.name, if bmc.available { "" } else { " (unavailable)" }, if bmc.administrable { "" } else { " - its name is past the administrative selector's 64 bytes, so it cannot be administered" }));
	out(&format!("  device {:#04x} revision {}, firmware {}.{:02x}, IPMI {}.{}, manufacturer {:#07x} product {:#06x}", device.device_id, device.device_revision, device.firmware_major, device.firmware_minor, device.ipmi_major, device.ipmi_minor, device.manufacturer, device.product));
	for binding in bmc.bindings.iter() {
		out(&format!("  through {:?} at {}{}{}", binding.interface, binding.binding, if binding.watchdog { ", its watchdog" } else { "" }, if binding.available { "" } else { " (unavailable)" }));
	}
	if bmc.paired {
		out("  REACHED TWICE: one BMC through two interfaces - DeviceManager's `disable` removes one if it should not be");
	}
}

// ASK FOR ONE ADMINISTRATIVE OPERATION on `bmc`: the parameters it takes, and the BMC's name as the payload.
fn request(connection: u64, bmc: &BmcSummary, action: AdminAction, parameters: Vec<u8>, label: String) {
	if connection == 0 {
		fail("this launch holds no administrative request connection, so nothing can be asked");
	}
	if !bmc.administrable {
		fail(&format!("{}: its name is past the administrative selector's 64 bytes, so it cannot be administered", bmc.name));
	}
	let payload = bmc.name.as_bytes();
	let object = memory_object_create(payload.len() as u64);
	if object < 0 {
		fail("no memory for the request");
	}
	let object = object as u64;
	let Some(at) = (unsafe { map_object(object) }) else { fail("the request cannot be written") };
	unsafe { core::ptr::copy_nonoverlapping(payload.as_ptr(), at as *mut u8, payload.len()) };
	unmap_object(object);
	let shared = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
	close(object);
	if shared < 0 {
		fail("the request cannot be handed over");
	}
	let args = AdminRequestArgs { action, target: bmc.name.clone(), parameters, payload_length: payload.len() as u32, label };
	out(&format!("bmc: {} - confirm it on the protected screen", bmc.name));
	match AdminRequestClient::new(connection).request(&args, shared as u64) {
		Some(Ok(AdminAnswer::Granted(grant))) => {
			out("bmc: confirmed - asking the BMC");
			let outcome = AdminAuthorityClient::new(grant.grant).execute();
			close(grant.grant);
			match outcome {
				Some(Ok(AdminResult::Completed)) => out("bmc: completed"),
				Some(Ok(AdminResult::Failed)) => out("bmc: failed - the BMC refused it, and nothing was done"),
				Some(Ok(AdminResult::OutcomeUnknown)) => out("bmc: the end was not observed - it is not retried"),
				Some(Err(error)) => out(&format!("bmc: not started: {error:?}")),
				None => out("bmc: the attempt went unanswered"),
			}
		}
		Some(Ok(AdminAnswer::Declined)) => out("bmc: declined - nothing was done"),
		Some(Err(error)) => out(&format!("bmc: refused: {error:?}")),
		None => out("bmc: the request went unanswered - nothing was done"),
	}
}

fn print_sel(client: &mut BmcClient, name: &str, sent: &mut Vec<u16>, quiet: bool) {
	let mut first = 0u16;
	loop {
		let page = match client.sel_page(name, first) {
			Some(Ok(page)) => page,
			other => fail(&format!("the log could not be read - {other:?}")),
		};
		if !check(name, &page.status) {
			return;
		}
		for entry in page.entries.iter() {
			if sent.contains(&entry.id) {
				continue;
			}
			sent.push(entry.id);
			if quiet {
				continue;
			}
			let data: Vec<String> = entry.data.iter().map(|byte| format!("{byte:02x}")).collect();
			out(&format!(
				"{:#06x} type {:#04x} at {} generator {:#06x} sensor type {:#04x} sensor {:#04x} {} event {:#04x} data {}{}",
				entry.id,
				entry.record_type,
				entry.timestamp,
				entry.generator,
				entry.sensor_type,
				entry.sensor,
				if entry.deassertion { "deasserted" } else { "asserted" },
				entry.event_type,
				data.join(" "),
				match entry.ours {
					Ours::Boot => " - this system's boot completed",
					Ours::Shutdown => " - this system's orderly shutdown",
					Ours::None => "",
				}
			));
		}
		for refusal in page.refused.iter() {
			out(&format!("{:#06x} refused - record type {:#04x}, which the specification does not define", refusal.id, refusal.record_type));
		}
		if page.next == 0xFFFF {
			return;
		}
		first = page.next;
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	// The grants, in the order PermissionManager walks its vocabulary.
	let connection = recv_tagged(bootstrap, &mut buf, b"ADMINREQUEST").unwrap_or(0);
	let service = recv_tagged(bootstrap, &mut buf, b"BMC").unwrap_or(0);
	if service == 0 {
		fail("this launch holds no connection to the BMC service");
	}
	let mut client = BmcClient::new(service);
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	let named = words.iter().copied().find(|word| word.starts_with("bmc:"));
	let rest: Vec<&str> = words.iter().copied().filter(|word| !word.starts_with("bmc:")).collect();
	let bmcs = match client.list() {
		Some(Ok(bmcs)) => bmcs,
		other => fail(&format!("the BMC service did not answer - {other:?}")),
	};
	match rest.as_slice() {
		[] => {
			if bmcs.is_empty() {
				out("bmc: this machine has no BMC this system reaches");
			}
			for bmc in &bmcs {
				print_summary(bmc);
			}
		}
		["info"] => print_summary(&choose(&bmcs, named)),
		["sensors"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(sensors)) = client.sensors(&bmc.name) else { fail("the sensors could not be read") };
			if !check(&bmc.name, &sensors.status) {
				exit();
			}
			for sensor in sensors.sensors.iter() {
				let reading = match sensor.state {
					ReadingState::Known => format!("{} {}", milli(sensor.value), unit(sensor.base_unit)),
					ReadingState::Unknown => String::from("unknown"),
					ReadingState::Unsupported => format!("states {:#06x}", sensor.bits),
					ReadingState::Invalid => String::from("invalid"),
					ReadingState::EventOnly => String::from("event-only"),
					ReadingState::NotOwned => String::from("another controller's"),
				};
				let mut thresholds = String::new();
				for (label, value) in [("unc", sensor.upper_non_critical), ("uc", sensor.upper_critical), ("unr", sensor.upper_non_recoverable)] {
					if let Some(value) = value {
						thresholds.push_str(&format!(" {label} {}", milli(value)));
					}
				}
				out(&format!("{:#04x} {:<16} type {:#04x} {reading}{thresholds}", sensor.number, sensor.name, sensor.sensor_type));
			}
			if sensors.truncated {
				out("bmc: the repository was cut at its bound: 512 records, 256 sensors");
			}
			if sensors.refused > 0 {
				out(&format!("bmc: {} record(s) refused as malformed", sensors.refused));
			}
		}
		["sel"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(info)) = client.sel_info(&bmc.name) else { fail("the log could not be read") };
			if !check(&bmc.name, &info.status) {
				exit();
			}
			out(&format!("{} entries, {} bytes free{}", info.entries, info.free, if info.overflow { ", overflowed" } else { "" }));
			print_sel(&mut client, &bmc.name, &mut Vec::new(), false);
		}
		["sel", "--follow"] => {
			let bmc = choose(&bmcs, named);
			let mut sent = Vec::new();
			print_sel(&mut client, &bmc.name, &mut sent, true);
			out(&format!("bmc: following {}'s log", bmc.name));
			let mut last = client.sel_info(&bmc.name).and_then(Result::ok).map_or(0, |info| info.last_addition);
			loop {
				sleep_until(clock() + FOLLOW_TICKS);
				let Some(Ok(info)) = client.sel_info(&bmc.name) else { continue };
				if info.last_addition != last {
					last = info.last_addition;
					print_sel(&mut client, &bmc.name, &mut sent, false);
				}
			}
		}
		["sel", "clear"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(info)) = client.sel_info(&bmc.name) else { fail("the log could not be read") };
			if !check(&bmc.name, &info.status) {
				exit();
			}
			if !info.reserve {
				fail(&format!("{}: the BMC reports no Reserve SEL, so its log cannot be cleared from here", bmc.name));
			}
			request(connection, &bmc, AdminAction::BmcSelClear, info.entries.to_le_bytes().to_vec(), format!("erase the {} records of {}'s event log", info.entries, bmc.name));
		}
		["fru"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(fru)) = client.fru(&bmc.name) else { fail("the inventory could not be read") };
			if !check(&bmc.name, &fru.status) {
				exit();
			}
			for device in fru.devices.iter() {
				out(&format!("FRU {} {}", device.device, device.name));
				if !check(&bmc.name, &device.status) {
					continue;
				}
				let field = |label: &str, value: &Option<String>| {
					if let Some(value) = value {
						out(&format!("  {label:<21}{value}"));
					}
				};
				field("chassis part", &device.chassis_part);
				field("chassis serial", &device.chassis_serial);
				field("board manufacturer", &device.board_manufacturer);
				field("board product", &device.board_product);
				field("board serial", &device.board_serial);
				field("board part", &device.board_part);
				field("product manufacturer", &device.product_manufacturer);
				field("product name", &device.product_name);
				field("product part", &device.product_part);
				field("product version", &device.product_version);
				field("product serial", &device.product_serial);
				field("product asset", &device.product_asset);
				if device.multirecord {
					out("  a multi-record area is present");
				}
				for refused in device.refused.iter() {
					out(&format!("  refused: {refused}"));
				}
			}
		}
		["chassis"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(chassis)) = client.chassis(&bmc.name) else { fail("the chassis could not be read") };
			if !check(&bmc.name, &chassis.status) {
				exit();
			}
			out(&format!("power {}{}{}{}{}, restore policy {}", if chassis.power_on { "on" } else { "off" }, if chassis.overload { ", overload" } else { "" }, if chassis.interlock { ", interlock" } else { "" }, if chassis.power_fault { ", power fault" } else { "" }, if chassis.control_fault { ", control fault" } else { "" }, chassis.restore_policy));
			out(&format!(
				"intrusion {}, drive fault {}, cooling fault {}, identify {}",
				chassis.intrusion,
				chassis.drive_fault,
				chassis.cooling_fault,
				match chassis.identify {
					Some(0) => "off",
					Some(1) => "on, timed",
					Some(2) => "on",
					_ => "not reported",
				}
			));
		}
		["chassis", "identify", seconds] => {
			let bmc = choose(&bmcs, named);
			let seconds: u8 = if *seconds == "off" { 0 } else { seconds.parse().unwrap_or_else(|_| fail("identify takes 1 to 255 seconds, or off")) };
			let Some(Ok(status)) = client.identify(&bmc.name, seconds) else { fail("the BMC service did not answer") };
			if check(&bmc.name, &status) {
				out(&format!("bmc: {}: identify {}", bmc.name, if seconds == 0 { String::from("off") } else { format!("on for {seconds} s") }));
			}
		}
		["chassis", operation] => {
			let bmc = choose(&bmcs, named);
			let (byte, words) = match *operation {
				"power-down" => (0u8, "POWER DOWN"),
				"power-cycle" => (2, "POWER CYCLE"),
				"hard-reset" => (3, "HARD RESET"),
				"soft-shutdown" => (5, "SOFT SHUTDOWN"),
				_ => fail("a chassis operation is power-down, power-cycle, hard-reset or soft-shutdown"),
			};
			request(connection, &bmc, AdminAction::BmcChassisControl, alloc::vec![byte], format!("{words} of {}", bmc.name));
		}
		["lan"] | ["users"] => {
			let bmc = choose(&bmcs, named);
			let Some(Ok(lan)) = client.lan(&bmc.name) else { fail("the LAN configuration could not be read") };
			if !check(&bmc.name, &lan.status) {
				exit();
			}
			if lan.channels.is_empty() {
				out("bmc: the BMC reports no LAN channel");
			}
			for channel in lan.channels.iter() {
				if rest[0] == "lan" {
					let mac: Vec<String> = channel.mac.iter().map(|byte| format!("{byte:02x}")).collect();
					out(&format!(
						"channel {}: {} address {} mask {} gateway {} mac {}{}",
						channel.channel,
						match channel.source {
							Some(1) => "static",
							Some(2) => "DHCP",
							Some(3) => "BIOS",
							Some(_) => "other",
							None => "-",
						},
						ipv4(&channel.address),
						ipv4(&channel.mask),
						ipv4(&channel.gateway),
						if mac.is_empty() { String::from("-") } else { mac.join(":") },
						match channel.vlan {
							Some(id) => format!(" vlan {id}"),
							None => String::new(),
						}
					));
				} else {
					for user in channel.users.iter() {
						out(&format!("channel {} user {} {} {} privilege {}", channel.channel, user.id, if user.name.is_empty() { "(unnamed)" } else { &user.name }, if user.enabled { "enabled" } else { "disabled" }, user.privilege));
					}
				}
			}
		}
		_ => fail("usage: bmc [info | sensors | sel [--follow | clear] | fru | chassis [identify SECONDS|off | power-down | power-cycle | hard-reset | soft-shutdown] | lan | users] [BMC]"),
	}
	exit();
}
