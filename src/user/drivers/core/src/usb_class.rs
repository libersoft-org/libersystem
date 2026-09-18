// THE IN-CONTROLLER CLASS MODULE: the execution model, decided, and the budget that makes it safe.
//
// THE DECISION. A USB class driver in this system is a MODULE INSIDE THE CONTROLLER'S PROCESS AND
// DOMAIN. It holds no claim of its own, binds no device of its own, and has no Domain of its own; it
// reaches the bus only through the controller that claimed it. The alternative - one USB-interface
// binding unit per interface, with its own endpoint capabilities, its own generation and composite
// devices - is a SUB-FUNCTION IDENTITY, which is the same cross-cutting problem as the firmware node
// and belongs to no class driver; P02M0163 refused it once already, as "splitting a driver into
// several and giving each its own authority". Choosing it here would block every USB class item
// behind an unowned prerequisite. So: class modules stay inside, and the per-driver registry entry,
// claim and transactional bind that a DRIVER owes do not apply to them.
//
// WHAT A MODULE MAY INVOKE, WHICH IS THE OTHER HALF OF "IT HOLDS NO AUTHORITY". The controller owns
// the interface and this is its list: address and configure a device, bring an endpoint up and take
// it down, queue a transfer on a ring it was given, wait for a completion, and read a descriptor. A
// module may NOT touch the controller's registers, allocate from the Domain directly, enable or
// disable a slot, or reach any device other than the one it was attached to. A module that needs an
// operation this list does not have extends the interface in the CONTROLLER, which is one place,
// rather than reaching around it - and the controller item is that interface's only owner.
//
// AND THE PART THAT IS NOT A SENTENCE. Two class modules inside one Domain share the controller's
// endpoints, its DMA pages and its in-flight transfers, so one of them can starve the other and both
// can starve the controller. Today that is not hypothetical: the HID module's device list is an
// unbounded `Vec` that a hostile tier of hubs fills, and the mass-storage module's data buffer grows
// to the largest request anybody makes and never shrinks. A budget per class, charged when a device
// is admitted and given back when it detaches, is what turns "a module cannot starve the controller"
// from a claim into a number - and the refusal at the bound is typed, so a controller that refuses a
// ninth keyboard says so rather than failing somewhere later.

/// Which class module a device was handed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassKind {
	Hid,
	Storage,
	Network,
	/// USB Attached SCSI: the same command set as `Storage` over four pipes instead of two, and a
	/// SEPARATE module because a controller may carry one of each - a Bulk-Only stick and a UAS
	/// disk are two devices, two budgets and two block providers.
	Uas,
	/// A CDC-ACM serial adapter: a bulk pair carrying bytes, and a notification endpoint that says
	/// what the line is doing. A SEPARATE module from `Network` although both are communications
	/// class, because what comes out of them is different - a byte stream and a frame transport -
	/// and a controller may carry one of each.
	Serial,
}

/// One endpoint ring is one DMA page, which is the unit both class modules allocate in.
pub const RING_BYTES: u64 = 4096;

/// The largest data buffer the mass-storage module may hold.
///
/// IT GROWS TO THE LARGEST REQUEST AND NEVER SHRINKS, so without a ceiling the bound on it is
/// whatever the largest transfer anybody ever asked for - inside the controller's Domain, charged to
/// the controller, for the life of the process. A megabyte is two hundred and fifty-six sectors,
/// which is larger than any request this system's block path makes.
pub const STORAGE_MAX_DATA_BYTES: u64 = 1 << 20;

/// What admitting one device to a class module costs the controller's Domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cost {
	pub endpoints: u32,
	/// What the module may hold for this device: its rings, plus any buffer it is allowed to grow.
	/// CHARGED AT ADMISSION AND NOT AT GROWTH, which is what lets the growth path be a bound check
	/// against a number already reserved rather than a second accounting nobody keeps in step.
	pub dma_bytes: u64,
	pub in_flight: u32,
}

/// What one HID device costs: one interrupt IN endpoint with its ring, and one report in flight.
pub const HID_COST: Cost = Cost { endpoints: 1, dma_bytes: RING_BYTES, in_flight: 1 };

/// What one mass-storage device costs: a bulk IN and a bulk OUT endpoint with their rings, the data
/// buffer it may grow to, and one transfer in flight.
pub const STORAGE_COST: Cost = Cost { endpoints: 2, dma_bytes: 2 * RING_BYTES + STORAGE_MAX_DATA_BYTES, in_flight: 1 };

/// What one CDC network adapter costs: a bulk pair with their rings, a receive page and a transmit
/// page, and one transfer in flight. The receive transfer is STANDING - one is outstanding whenever
/// the adapter is bound - which is what the in-flight count is for.
pub const NETWORK_COST: Cost = Cost { endpoints: 2, dma_bytes: 2 * RING_BYTES + 2 * 4096, in_flight: 1 };

/// ONE NETWORK ADAPTER, for the reason the storage module admits one disk: a second NIC is a second
/// link with its own MAC and its own stack above it, and this controller publishes one provider.
/// Stated as a budget so a second adapter is REFUSED and says so, rather than being ignored.
pub const NETWORK_LIMITS: Limits = Limits { devices: 1, endpoints: 2, dma_bytes: 2 * RING_BYTES + 2 * 4096, in_flight: 1 };

/// What one UAS device costs: four pipes with their rings, a control page, a data page, and one
/// command in flight - which is the slice this transport implements and the reason one stream is
/// enough. The stream context arrays are a page each for the three answering pipes.
pub const UAS_COST: Cost = Cost { endpoints: 4, dma_bytes: 4 * RING_BYTES + 5 * 4096, in_flight: 1 };

/// ONE UAS DEVICE, for the reason the storage module admits one disk: the transport is one command
/// outstanding under one tag, and a second device would need a second of everything.
pub const UAS_LIMITS: Limits = Limits { devices: 1, endpoints: 4, dma_bytes: 4 * RING_BYTES + 5 * 4096, in_flight: 1 };

/// What one CDC-ACM adapter costs: the bulk pair and the notification endpoint with their rings, a
/// receive page and a transmit page, and one standing receive in flight - the same shape as the
/// network adapter, with one more endpoint and no frame buffer.
pub const SERIAL_COST: Cost = Cost { endpoints: 3, dma_bytes: 3 * RING_BYTES + 2 * 4096, in_flight: 1 };

/// TWO SERIAL ADAPTERS, and the number is the item's own requirement rather than a round figure:
/// the item says "several simultaneous adapters", and two is what makes "several" a case the budget
/// has a refusal for. Each publishes its own byte stream under its own name, so a consumer asking
/// for a console stream cannot be handed whichever answered first.
pub const SERIAL_LIMITS: Limits = Limits { devices: 2, endpoints: 6, dma_bytes: 2 * (3 * RING_BYTES + 2 * 4096), in_flight: 2 };

/// The ceilings one class module may reach inside the controller's Domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
	pub devices: u32,
	pub endpoints: u32,
	pub dma_bytes: u64,
	pub in_flight: u32,
}

/// EIGHT HID DEVICES, which is a keyboard, a pointer and six more behind hubs.
///
/// The number is a bound on a hostile hub tree rather than a guess at a desk: a USB bus admits 127
/// addresses, every one of them can be a keyboard, and each one this module configures takes a DMA
/// page and an endpoint of the controller's for as long as it stays plugged in.
pub const HID_LIMITS: Limits = Limits { devices: 8, endpoints: 8, dma_bytes: 8 * RING_BYTES, in_flight: 8 };

/// ONE MASS-STORAGE DEVICE, which is what this controller has always served.
///
/// It is stated as a budget rather than left as an `Option` because the two say different things: an
/// `Option` is a structure that cannot hold a second device, and a budget is a controller that
/// REFUSES one and says why. The second is what a user plugging in two disks needs to be told.
pub const STORAGE_LIMITS: Limits = Limits { devices: 1, endpoints: 2, dma_bytes: 2 * RING_BYTES + STORAGE_MAX_DATA_BYTES, in_flight: 1 };

/// Which dimension of a class module's budget refused a device.
///
/// FOUR ANSWERS AND NOT ONE `false`. A controller that refuses a device is telling somebody with a
/// cable in their hand why nothing happened, and "the keyboard budget is full" and "this controller
/// serves one disk" are different things to be told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Devices,
	Endpoints,
	DmaBytes,
	InFlight,
}

impl Refusal {
	/// The refusal as a line a driver prints, so two call sites do not word it differently.
	pub const fn describe(self) -> &'static [u8] {
		match self {
			Refusal::Devices => b"device count",
			Refusal::Endpoints => b"endpoints",
			Refusal::DmaBytes => b"DMA bytes",
			Refusal::InFlight => b"transfers in flight",
		}
	}
}

/// What a class module is holding right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Usage {
	pub devices: u32,
	pub endpoints: u32,
	pub dma_bytes: u64,
	pub in_flight: u32,
}

/// The controller's per-class accounting.
///
/// THE CONTROLLER HOLDS IT AND THE MODULES DO NOT, which is the lifecycle half of the decision above:
/// attach, detach and teardown are driven by the controller, so the controller is where what they
/// cost is counted. A module that kept its own count would be a module that could forget to give
/// something back.
#[derive(Clone, Copy, Debug, Default)]
pub struct Budget {
	hid: Usage,
	storage: Usage,
	network: Usage,
	uas: Usage,
	serial: Usage,
}

impl Budget {
	pub const fn new() -> Budget {
		Budget { hid: Usage { devices: 0, endpoints: 0, dma_bytes: 0, in_flight: 0 }, storage: Usage { devices: 0, endpoints: 0, dma_bytes: 0, in_flight: 0 }, network: Usage { devices: 0, endpoints: 0, dma_bytes: 0, in_flight: 0 }, uas: Usage { devices: 0, endpoints: 0, dma_bytes: 0, in_flight: 0 }, serial: Usage { devices: 0, endpoints: 0, dma_bytes: 0, in_flight: 0 } }
	}

	pub const fn limits(kind: ClassKind) -> Limits {
		match kind {
			ClassKind::Hid => HID_LIMITS,
			ClassKind::Storage => STORAGE_LIMITS,
			ClassKind::Network => NETWORK_LIMITS,
			ClassKind::Uas => UAS_LIMITS,
			ClassKind::Serial => SERIAL_LIMITS,
		}
	}

	pub const fn cost(kind: ClassKind) -> Cost {
		match kind {
			ClassKind::Hid => HID_COST,
			ClassKind::Storage => STORAGE_COST,
			ClassKind::Network => NETWORK_COST,
			ClassKind::Uas => UAS_COST,
			ClassKind::Serial => SERIAL_COST,
		}
	}

	pub const fn usage(&self, kind: ClassKind) -> Usage {
		match kind {
			ClassKind::Hid => self.hid,
			ClassKind::Storage => self.storage,
			ClassKind::Network => self.network,
			ClassKind::Uas => self.uas,
			ClassKind::Serial => self.serial,
		}
	}

	/// Charge one device to a class module, or refuse and say which ceiling stopped it.
	///
	/// NOTHING IS CHARGED BY A REFUSED ADMISSION. Every dimension is checked before any of them is
	/// written, because a partial charge is a leak that only shows up as a controller that refuses
	/// devices it has room for.
	pub fn admit(&mut self, kind: ClassKind) -> Result<(), Refusal> {
		let limits = Self::limits(kind);
		let cost = Self::cost(kind);
		let usage = self.usage(kind);
		// CHECKED ARITHMETIC, because these are sums of a device count and a per-device cost and both
		// are bounded by what the bus can present rather than by anything this process chose.
		let devices = usage.devices.checked_add(1).ok_or(Refusal::Devices)?;
		let endpoints = usage.endpoints.checked_add(cost.endpoints).ok_or(Refusal::Endpoints)?;
		let dma_bytes = usage.dma_bytes.checked_add(cost.dma_bytes).ok_or(Refusal::DmaBytes)?;
		let in_flight = usage.in_flight.checked_add(cost.in_flight).ok_or(Refusal::InFlight)?;
		if devices > limits.devices {
			return Err(Refusal::Devices);
		}
		if endpoints > limits.endpoints {
			return Err(Refusal::Endpoints);
		}
		if dma_bytes > limits.dma_bytes {
			return Err(Refusal::DmaBytes);
		}
		if in_flight > limits.in_flight {
			return Err(Refusal::InFlight);
		}
		let slot = self.slot(kind);
		*slot = Usage { devices, endpoints, dma_bytes, in_flight };
		Ok(())
	}

	/// Give one device's charge back, on detach or on a teardown.
	///
	/// SATURATING AND NOT CHECKED, deliberately: a release that underflowed would panic a driver
	/// during a teardown, which is the one moment it must not, and the honest floor for "how much is
	/// this module holding" after too many releases is zero.
	pub fn release(&mut self, kind: ClassKind) {
		let cost = Self::cost(kind);
		let slot = self.slot(kind);
		slot.devices = slot.devices.saturating_sub(1);
		slot.endpoints = slot.endpoints.saturating_sub(cost.endpoints);
		slot.dma_bytes = slot.dma_bytes.saturating_sub(cost.dma_bytes);
		slot.in_flight = slot.in_flight.saturating_sub(cost.in_flight);
	}

	/// Whether a buffer of this many bytes is inside what the class was charged for.
	///
	/// THE GROWTH PATH ASKS THIS RATHER THAN CHARGING AGAIN. The data buffer was reserved at
	/// admission, so growing it is a question about a number already counted - and asking it here is
	/// what stops the one caller that grows a buffer from being the one place with no bound.
	pub const fn buffer_within(kind: ClassKind, bytes: u64) -> bool {
		match kind {
			ClassKind::Hid => bytes <= RING_BYTES,
			ClassKind::Storage => bytes <= STORAGE_MAX_DATA_BYTES,
			// The network module's buffers are the two pages it was charged for and it never grows
			// them: a frame that does not fit one is a frame this module refuses to move.
			ClassKind::Network => bytes <= 4096,
			// The UAS module's data buffer is the one page it was charged for and it never grows it.
			ClassKind::Uas => bytes <= 4096,
			ClassKind::Serial => bytes <= 4096,
		}
	}

	fn slot(&mut self, kind: ClassKind) -> &mut Usage {
		match kind {
			ClassKind::Hid => &mut self.hid,
			ClassKind::Storage => &mut self.storage,
			ClassKind::Network => &mut self.network,
			ClassKind::Uas => &mut self.uas,
			ClassKind::Serial => &mut self.serial,
		}
	}
}

#[cfg(test)]
mod tests;
