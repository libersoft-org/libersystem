// THE ZONES AND THE FANS: every `thermal-zone` and `cooling-device` publication, adopted through the catalogue
// connection minted for those two kinds alone, and what the policy makes of their readings.
//
// A ZONE'S READING - at its own `_TZP`, at its `Notify`, and every `_TSP` while passive cooling is engaged, which this
// service asks its driver for - is checked against `_CRT` and `_HOT` first, then moves passive cooling - the equation run
// once per `_TSP`, however many readings arrive between - and then every fan. A zone or fan that goes takes what it held
// with it: a gone zone's passive limit is lifted, since nothing reads it any more, and its replacement is adopted afresh.

use super::{ASK_TICKS, say};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, FanDescription, FanPower, ProviderInfo, ProviderKind, ZoneCooling, ZonePower, cooling_device, provider_catalogue, thermal_zone};
use rt::*;
use service_logic::processor_policy::{self, Cooling, Critical, CurvePoint, FanControl, Passive, Trips};

// A zone without `_TSP` is sampled once a second while passive cooling is engaged.
const DEFAULT_TSP_TENTHS: u32 = 10;
// The fewest milliseconds `sample-every` takes.
const MIN_SAMPLE_MS: u32 = 100;

struct Zone {
	info: ProviderInfo,
	chan: u64,
	stream: u64,
	cooling: ZoneCooling,
	temperature: Option<u32>,
	passive: Cooling,
	trips: Trips,
	// When the equation last ran.
	evaluated_at: u64,
}

impl Zone {
	fn tsp_tenths(&self) -> u32 {
		if self.cooling.tsp == 0 { DEFAULT_TSP_TENTHS } else { self.cooling.tsp }
	}
}

struct Fan {
	info: ProviderInfo,
	chan: u64,
	description: FanDescription,
	control: FanControl,
	commanded: Option<u32>,
	speed_rpm: u32,
}

pub(crate) struct Thermal {
	zones_watch: u64,
	fans_watch: u64,
	zones: Vec<Zone>,
	fans: Vec<Fan>,
	// The owner's curves, by fan path, for this instance's life.
	curves: Vec<(String, Vec<CurvePoint>)>,
	mode: u8,
	// Whether a passive limit moved since the loop last asked, and the critical trips crossed, for the loop to act on.
	changed: bool,
	actions: Vec<(String, Critical)>,
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	(a.slot, a.provider_generation, a.binding_generation) == (b.slot, b.provider_generation, b.binding_generation)
}

fn celsius(tenths_kelvin: u32) -> String {
	let tenths = i64::from(tenths_kelvin) - 2732;
	format!("{}.{} C", tenths / 10, (tenths % 10).abs())
}

impl Thermal {
	pub(crate) fn new() -> Thermal {
		Thermal { zones_watch: 0, fans_watch: 0, zones: Vec::new(), fans: Vec::new(), curves: Vec::new(), mode: 0, changed: false, actions: Vec::new() }
	}

	pub(crate) fn subscribe(&mut self, catalogue: u64) {
		if catalogue == 0 {
			say("no catalogue connection - no zone and no fan is followed");
			return;
		}
		self.zones_watch = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::ThermalZone).unwrap_or(0);
		self.fans_watch = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::CoolingDevice).unwrap_or(0);
		if self.zones_watch == 0 || self.fans_watch == 0 {
			say("the catalogue refused a subscription - some zones or fans are not followed");
		}
	}

	pub(crate) fn handles(&self) -> Vec<u64> {
		let mut out: Vec<u64> = [self.zones_watch, self.fans_watch].into_iter().filter(|&handle| handle != 0).collect();
		out.extend(self.zones.iter().map(|zone| zone.stream).filter(|&stream| stream != 0));
		out
	}

	pub(crate) fn take_changed(&mut self) -> bool {
		core::mem::take(&mut self.changed)
	}

	pub(crate) fn take_actions(&mut self) -> Vec<(String, Critical)> {
		core::mem::take(&mut self.actions)
	}

	// THE STRICTEST PASSIVE LIMIT on the processor at `path`, from every engaged zone whose `_PSL` lists it.
	pub(crate) fn limit_of(&self, path: &str) -> u32 {
		processor_policy::strictest(self.zones.iter().filter(|zone| zone.passive.engaged && zone.cooling.passive_processors.iter().any(|listed| listed == path)).map(|zone| zone.passive.limit))
	}

	// `_SCP` on every zone that has it.
	pub(crate) fn cooling_policy(&mut self, mode: u8) {
		self.mode = mode;
		for zone in self.zones.iter().filter(|zone| zone.cooling.scp) {
			let _ = thermal_zone::Client::with_deadline(ChannelTransport { chan: zone.chan }, clock() + ASK_TICKS).cooling_policy(&mode);
		}
	}

	// ONE READY HANDLE, if it is a zone's or a fan's.
	pub(crate) fn serve(&mut self, handle: u64, catalogue: u64, buf: &mut [u8]) -> bool {
		if handle == self.zones_watch {
			self.drain_watch(true, catalogue, buf);
			return true;
		}
		if handle == self.fans_watch {
			self.drain_watch(false, catalogue, buf);
			return true;
		}
		if let Some(at) = self.zones.iter().position(|zone| zone.stream == handle) {
			self.drain_readings(at, buf);
			return true;
		}
		false
	}

	fn drain_watch(&mut self, zones: bool, catalogue: u64, buf: &mut [u8]) {
		let watch = if zones { self.zones_watch } else { self.fans_watch };
		loop {
			let (len, handles) = match try_recv_caps(watch, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(watch);
					if zones {
						self.zones_watch = 0;
					} else {
						self.fans_watch = 0;
					}
					say("a catalogue subscription ended - what it brought stays followed");
					return;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame = wire::Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame) else { continue };
			match (zones, info.live) {
				(true, true) => self.adopt_zone(catalogue, info),
				(false, true) => self.adopt_fan(catalogue, info),
				(true, false) => {
					if let Some(at) = self.zones.iter().position(|zone| same(&zone.info, &info)) {
						self.drop_zone(at, "its publication was withdrawn");
					}
				}
				(false, false) => {
					if let Some(at) = self.fans.iter().position(|fan| same(&fan.info, &info)) {
						let fan = self.fans.remove(at);
						close(fan.chan);
						say(&format!("{} is no longer followed - its publication was withdrawn", fan.description.path));
					}
				}
			}
		}
	}

	fn adopt_zone(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.zones.iter().any(|zone| same(&zone.info, &info)) {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			say("a thermal zone's publication could not be opened");
			return;
		};
		let cooling = match thermal_zone::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).describe() {
			Some(Ok(cooling)) => cooling,
			_ => {
				say("a thermal zone's driver did not describe it");
				close(chan);
				return;
			}
		};
		let Some(stream) = thermal_zone::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).readings() else {
			say(&format!("{}'s readings could not be opened", cooling.zone));
			close(chan);
			return;
		};
		let trip = |value: Option<u32>| value.map_or(String::from("none"), celsius);
		say(&format!("follows {} - _PSV {}, _HOT {}, _CRT {}, {} active trip(s), {} passive processor(s)", cooling.zone, trip(cooling.passive), trip(cooling.hot), trip(cooling.critical), cooling.active.len(), cooling.passive_processors.len()));
		if cooling.scp {
			let _ = thermal_zone::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).cooling_policy(&self.mode);
		}
		self.zones.push(Zone { info, chan, stream, cooling, temperature: None, passive: Cooling::default(), trips: Trips::default(), evaluated_at: 0 });
	}

	fn drop_zone(&mut self, at: usize, why: &str) {
		let zone = self.zones.remove(at);
		close(zone.stream);
		close(zone.chan);
		if zone.passive.engaged {
			self.changed = true;
		}
		say(&format!("{} is no longer followed - {why}", zone.cooling.zone));
		self.update_fans();
	}

	fn drain_readings(&mut self, at: usize, buf: &mut [u8]) {
		let stream = self.zones[at].stream;
		loop {
			match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let Some(reading) = thermal_zone::readings_read(&buf[..len], &mut frame) else { continue };
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					self.reading(at, reading.temperature);
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					self.drop_zone(at, "its driver ended");
					return;
				}
			}
		}
		self.update_fans();
	}

	// ONE READING: the critical trips, then passive cooling.
	fn reading(&mut self, at: usize, temperature: u32) {
		let now = clock();
		let zone = &mut self.zones[at];
		zone.temperature = Some(temperature);
		match zone.trips.reading(temperature, zone.cooling.critical, zone.cooling.hot) {
			Critical::Nothing => {}
			action => self.actions.push((zone.cooling.zone.clone(), action)),
		}
		let Some(target) = zone.cooling.passive else { return };
		let passive = Passive { tc1: zone.cooling.tc1, tc2: zone.cooling.tc2, target };
		let tsp_ticks = u64::from(zone.tsp_tenths()) * TICKS_PER_SECOND / 10;
		let was = zone.passive;
		// ONCE PER `_TSP`: a reading the zone's own period or a `Notify` brought in between moves nothing but the trips.
		if was.engaged && now.saturating_sub(zone.evaluated_at) < tsp_ticks / 2 {
			return;
		}
		let now_cooling = zone.passive.sample(passive, temperature);
		if now_cooling.engaged {
			zone.evaluated_at = now;
		}
		if now_cooling.limit != was.limit || now_cooling.engaged != was.engaged {
			self.changed = true;
		}
		if now_cooling.engaged != was.engaged {
			let milliseconds = if now_cooling.engaged { (zone.tsp_tenths() * 100).max(MIN_SAMPLE_MS) } else { 0 };
			let asked = thermal_zone::Client::with_deadline(ChannelTransport { chan: zone.chan }, clock() + ASK_TICKS).sample_every(&milliseconds);
			if now_cooling.engaged {
				say(&format!("{} is past _PSV at {} - passive cooling engaged, sampled every {milliseconds} ms{}", zone.cooling.zone, celsius(temperature), if matches!(asked, Some(Ok(()))) { "" } else { " (the driver did not take the period)" }));
			} else {
				say(&format!("{} is back under _PSV at {} - passive cooling released", zone.cooling.zone, celsius(temperature)));
			}
		}
		if now_cooling.engaged && now_cooling.limit != was.limit {
			say(&format!("{} at {}: its processors are held to {}.{} %", zone.cooling.zone, celsius(temperature), now_cooling.limit / 10, now_cooling.limit % 10));
		}
	}

	// ------------------------------------------------------------------ the fans

	fn adopt_fan(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.fans.iter().any(|fan| same(&fan.info, &info)) {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			say("a fan's publication could not be opened");
			return;
		};
		let description = match cooling_device::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).describe() {
			Some(Ok(description)) => description,
			_ => {
				say("a fan's driver did not describe it");
				close(chan);
				return;
			}
		};
		let control = if description.by_power_state {
			FanControl::PowerState
		} else if description.fine_grain {
			FanControl::FineGrain { step: description.step_size }
		} else {
			FanControl::Levels(description.levels.iter().map(|level| (level.control, level.speed_rpm)).collect())
		};
		say(&format!(
			"follows the fan {} - {}",
			description.path,
			match &control {
				FanControl::PowerState => String::from("switched by its device power state"),
				FanControl::FineGrain { step } => format!("fine-grain control in steps of {step} %, the owner's curve applies"),
				FanControl::Levels(levels) => format!("{} level(s)", levels.len()),
			}
		));
		self.fans.push(Fan { info, chan, description, control, commanded: None, speed_rpm: 0 });
		self.update_fans();
	}

	// THE TEMPERATURE THE CURVE READS for a fan: the hottest zone whose trips list it, or the hottest zone at all.
	fn curve_temperature(&self, path: &str) -> Option<u32> {
		let listing = self.zones.iter().filter(|zone| zone.cooling.active.iter().any(|trip| trip.devices.iter().any(|device| device == path))).filter_map(|zone| zone.temperature).max();
		listing.or_else(|| self.zones.iter().filter_map(|zone| zone.temperature).max())
	}

	fn curve_of(&self, path: &str) -> Vec<CurvePoint> {
		self.curves.iter().find(|(fan, _)| fan == path).map(|(_, curve)| curve.clone()).unwrap_or_else(processor_policy::default_curve)
	}

	// EVERY FAN'S LEVEL from the active trips and, for a fan the platform leaves to the operating system, its curve -
	// commanded only where it changed.
	fn update_fans(&mut self) {
		for at in 0..self.fans.len() {
			let path = self.fans[at].description.path.clone();
			let mut percent = 0u8;
			for zone in &self.zones {
				let Some(temperature) = zone.temperature else { continue };
				let trips: Vec<u32> = zone.cooling.active.iter().filter(|trip| trip.devices.iter().any(|device| *device == path)).map(|trip| trip.temperature).collect();
				percent = percent.max(processor_policy::active_percent(temperature, &trips));
			}
			if matches!(self.fans[at].control, FanControl::FineGrain { .. })
				&& let Some(temperature) = self.curve_temperature(&path)
			{
				percent = percent.max(processor_policy::curve_percent(&self.curve_of(&path), temperature));
			}
			let fan = &mut self.fans[at];
			let control = processor_policy::fan_control(&fan.control, percent);
			if fan.commanded == Some(control) {
				continue;
			}
			match cooling_device::Client::with_deadline(ChannelTransport { chan: fan.chan }, clock() + ASK_TICKS).set_level(&control) {
				Some(Ok(status)) => {
					say(&format!("the fan {path} is set to {control} ({percent} % of its range) - it reports {} rpm", status.speed_rpm));
					fan.commanded = Some(control);
					fan.speed_rpm = status.speed_rpm;
				}
				Some(Err(error)) => say(&format!("the fan {path} refused level {control} - {error:?}")),
				None => say(&format!("the fan {path} did not answer")),
			}
		}
	}

	// THE OWNER'S CURVE for a fan the platform leaves to the operating system.
	pub(crate) fn set_curve(&mut self, path: &str, curve: Vec<CurvePoint>) -> Result<(), Error> {
		processor_policy::check_curve(&curve).map_err(|_| Error::Invalid)?;
		let fan = self.fans.iter().find(|fan| fan.description.path == path).ok_or(Error::NotFound)?;
		if !matches!(fan.control, FanControl::FineGrain { .. }) {
			return Err(Error::Unsupported);
		}
		self.curves.retain(|(fan, _)| fan != path);
		self.curves.push((String::from(path), curve));
		say(&format!("the fan {path} follows the owner's curve"));
		self.update_fans();
		Ok(())
	}

	// ------------------------------------------------------------------ the status

	pub(crate) fn zone_status(&self) -> Vec<ZonePower> {
		self.zones.iter().take(16).map(|zone| ZonePower { zone: zone.cooling.zone.clone(), temperature: zone.temperature.unwrap_or(0), passive: zone.cooling.passive, critical: zone.cooling.critical, hot: zone.cooling.hot, passive_engaged: zone.passive.engaged, cap_percent: zone.passive.limit / 10 }).collect()
	}

	pub(crate) fn fan_status(&self) -> Vec<FanPower> {
		self.fans
			.iter()
			.take(16)
			.map(|fan| {
				let curve = if matches!(fan.control, FanControl::FineGrain { .. }) { self.curve_of(&fan.description.path).iter().map(|point| proto::system::CurvePoint { temperature: point.temperature, percent: point.percent }).collect() } else { Vec::new() };
				FanPower { path: fan.description.path.clone(), control: fan.commanded.unwrap_or(0), speed_rpm: fan.speed_rpm, curve }
			})
			.collect()
	}
}
