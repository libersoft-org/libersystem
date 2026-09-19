// driver.hda - the userspace Intel High Definition Audio controller driver.
//
// DeviceManager launches this program with a `BIND` naming the controller and a DeviceMemory
// capability to BAR 0. The driver takes the controller out of reset, builds the command and response
// rings, finds the first codec that answers, walks its widget graph for a pin complex that can reach
// an audio output converter, configures that route and one output stream, and then serves the SAME
// PCM contract `driver.virtio-snd` serves: each message on its service channel is one period of
// signed 16-bit stereo at 48 kHz, and an empty message ends the stream.
//
// THE SAME CONTRACT AND NOT A SECOND ONE. The roadmap says so in as many words, and the block wire's
// history is the argument: three programs wrote that one out privately and the two servers ended up
// disagreeing about what a refusal means. AudioService should not be able to tell which driver is
// underneath it.
//
// MOST OF THE DECISIONS ARE IN `drivers::hda` AND ARE HOST-TESTED, because most of an HDA driver is
// not register access: a verb packed into a word, two rings, a graph walk, and a format word whose
// fields are indices rather than the numbers they name.

#![no_std]
#![no_main]

use driver_protocol::audio;
use drivers::hda::{self, Widget};
use drivers::{common, hda as decisions};
use rt::*;

const PAGE: u64 = 4096;

// TWO HUNDRED AND FIFTY-SIX, WHICH IS THE SIZE EVERY CONTROLLER MUST SUPPORT.
//
// The size register carries capability bits saying which of two, sixteen and 256 a given controller
// offers, and only 256 is required of all of them. Asking for sixteen is asking for an optional
// size: a controller that does not offer it keeps the size it had, and then the driver wraps its
// pointers at sixteen while the controller wraps at 256. The first response still arrives, which is
// what makes it confusing - they do not disagree until the rings have been used a little.
const RING_ENTRIES: u16 = 256;

// The format AudioService produces, which this driver configures and does not negotiate.
const RATE_HZ: u32 = 48000;
const BITS: u8 = 16;
const CHANNELS: u8 = 2;

// Bounded waits.
// BOUNDED IN TICKS AND NOT IN SPINS, and that correction is worth the comment because the first two
// attempts at it were both wrong in the same way.
//
// A SPIN COUNT IS NOT A TIME. One iteration here is an MMIO read, which on real silicon is tens of
// nanoseconds and under emulation is a trap into the hypervisor - three orders of magnitude apart.
// A budget tuned to either is meaningless on the other, and the first version's ten million spins
// took long enough that DeviceManager killed this driver for silence before it could say what it was
// waiting for, and long enough that the whole boot ran out of ITS window: nine of twenty-four
// services came up.
//
// THE MACHINE ALREADY HAS THE UNIT. The timer runs at 100 Hz everywhere, READY is due 200 ticks
// after BIND, and every driver on this machine binds inside 44. So bring-up takes a deadline in
// ticks, a fraction of that window, and every wait inside it shares one - which means a bring-up
// where everything times out still finishes in time to SAY so, which is the whole point.
const BRINGUP_TICKS: u64 = 60;
// How long one captured period is waited for, in the 100 Hz ticks READY is measured in. A period is
// about eleven milliseconds at this format, so this is far more than one and still bounded: a
// controller that stops filling must not hold the consumer.
const CAPTURE_TICKS: u64 = 50;

use drivers::common::Deadline;
unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}
unsafe fn w8(addr: u64, v: u8) {
	unsafe { (addr as *mut u8).write_volatile(v) }
}
unsafe fn r16(addr: u64) -> u16 {
	unsafe { (addr as *const u16).read_volatile() }
}
unsafe fn w16(addr: u64, v: u16) {
	unsafe { (addr as *mut u16).write_volatile(v) }
}
unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}

struct Dma {
	handle: u64,
	virt: u64,
	phys: u64,
	bytes: u64,
}

fn dma(device: u64, bytes: u64) -> Option<Dma> {
	let handle: i64 = dma_buffer_create_for(device, bytes);
	if handle < 0 {
		return None;
	}
	let virt: i64 = unsafe { dma_buffer_map(handle as u64) };
	if sys_is_err(virt as u64) {
		return None;
	}
	Some(Dma { handle: handle as u64, virt: virt as u64, phys: unsafe { dma_buffer_phys(handle as u64) }, bytes })
}

unsafe fn zero(virt: u64, bytes: u64) {
	unsafe { core::ptr::write_bytes(virt as *mut u8, 0, bytes as usize) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bringup {
	// THE TWO HALVES OF THE RESET ARE DIFFERENT FAILURES and were one message, which is the mistake
	// the NVMe driver already paid for once. Entering reset and never reading back as reset is a
	// register that is not answering at all - a window pointing at nothing reads as all ones and
	// never clears. Leaving reset and never reading back as running is a controller that is there
	// and is not coming up. They want different next questions.
	ResetEnter { gctl: u32 },
	ResetLeave { gctl: u32 },
	// AND THE COMMAND RING'S POINTER RESET IS A THIRD ONE. It was reported as "did not come out of
	// reset" WITH GCTL's value attached, which read 1 - a message that contradicted itself and sent
	// the reader to the wrong register. A failure that borrows another failure's sentence is worse
	// than one with no sentence at all.
	CorbPointer { corbrp: u16 },
	NoDma,
	NoCodec { statests: u16 },
	NoAudioGroup,
	NoRoute,
	TooManyNodes,
	NoStream,
	Format,
	// A verb went unanswered, and WHICH one is carried: the walk sends verbs to a dozen nodes and
	// "the codec did not answer" names all of them.
	Unanswered { node: u8, command: u32 },
}

impl Bringup {
	fn name(self) -> &'static [u8] {
		match self {
			Bringup::ResetEnter { .. } => b"the controller never read back as being in reset - its register window may not be answering",
			Bringup::ResetLeave { .. } => b"the controller did not come out of reset",
			Bringup::CorbPointer { .. } => b"the command ring's read pointer never cleared its reset bit",
			Bringup::NoDma => b"the command and response rings could not be taken",
			Bringup::NoCodec { .. } => b"no codec answered its identity verb",
			Bringup::NoAudioGroup => b"the codec reports no audio function group",
			Bringup::NoRoute => b"no pin complex reaches an audio output converter",
			Bringup::TooManyNodes => b"the codec describes more nodes than this driver's bounded walk follows",
			Bringup::NoStream => b"the controller reports no output stream descriptor",
			Bringup::Format => b"the format this driver configures is not one it can express",
			Bringup::Unanswered { .. } => b"a verb went unanswered",
		}
	}

	// A codec that is absent or describes nothing this driver can route is a final answer; a reset
	// that did not finish and memory that could not be taken are states a retry can find differently.
	fn retryable(self) -> bool {
		matches!(self, Bringup::ResetEnter { .. } | Bringup::ResetLeave { .. } | Bringup::CorbPointer { .. } | Bringup::NoDma | Bringup::Unanswered { .. })
	}
}

struct Controller {
	base: u64,
	device: u64,
	// The command ring, the response ring and the buffer descriptor list are carved from one region
	// whose handle is deliberately not kept: nothing here releases it, because the region must stay
	// mapped for as long as the controller has its addresses, which is for the life of this process.
	// When the process ends the kernel holds those frames until the device is confirmed stopped,
	// which is the whole reason `dma_buffer_create_for` names the device.
	corb_virt: u64,
	rirb_virt: u64,
	bdl_virt: u64,
	bdl_phys: u64,
	// Where this driver last read the response ring.
	rirb_read: u16,
	// The whole bring-up shares one deadline, so a chain of waits cannot add up past it.
	deadline: Deadline,
	codec: u8,
	/// How many codecs answered their identity verb on this link. Reported because "one codec
	/// answered" and "this driver looks at one codec" are different facts and the log said neither.
	codecs: u8,
	converter: u8,
	stream: u64,
	// The audio buffer, grown to twice the largest period seen.
	audio: Dma,
	period: u64,
	running: bool,
	// THE CAPTURE SIDE, WHICH A CODEC NEED NOT HAVE. A machine with an output-only codec serves
	// playback exactly as before and answers every capture command with a refusal - which is what
	// the wire's empty reply means, and is why the refusal is a length rather than a status.
	capture_stream: u64,
	capture_bdl_virt: u64,
	capture_bdl_phys: u64,
	adc: Option<u8>,
	capture: Dma,
	capturing: bool,
	// Which half of the capture buffer is the next one to hand over.
	capture_half: u64,
}

/// What a bring-up found on one codec: the audio function group and the widgets the driver drives.
struct Route {
	group: u8,
	converter: u8,
	pin: u8,
	adc: Option<u8>,
	input_pin: Option<u8>,
}

impl Controller {
	// The audio function group on THIS codec, and the output and capture routes inside it.
	//
	// EVERY REFUSAL IS THIS CODEC'S AND NOT THE LINK'S, which is what lets the caller try the next
	// one: a codec with no audio group is a codec that does something else, and a codec with no pin
	// that can drive a converter is one whose outputs are not wired here.
	unsafe fn route(&mut self) -> Result<Route, Bringup> {
		unsafe {
			let (first_group, groups) = decisions::node_range(self.parameter(0, hda::PARAM_NODE_COUNT).ok_or(Bringup::Unanswered { node: 0, command: hda::PARAM_NODE_COUNT })?);
			if groups > hda::MAX_NODES {
				return Err(Bringup::TooManyNodes);
			}
			let mut audio_group: Option<u8> = None;
			for offset in 0..groups {
				let node = first_group + offset;
				let kind = self.parameter(node, hda::PARAM_FUNCTION_TYPE).unwrap_or(0);
				if kind & 0x7F == hda::FUNCTION_AUDIO {
					audio_group = Some(node);
					break;
				}
			}
			let group = audio_group.ok_or(Bringup::NoAudioGroup)?;

			let (first_widget, widgets) = decisions::node_range(self.parameter(group, hda::PARAM_NODE_COUNT).ok_or(Bringup::NoAudioGroup)?);
			if widgets > hda::MAX_NODES {
				return Err(Bringup::TooManyNodes);
			}
			// THE FIRST OUTPUT CONVERTER AND THE FIRST PIN THAT CAN DRIVE ONE, which is the bounded
			// deterministic route the item asks for rather than a policy about jacks.
			let mut converter: Option<u8> = None;
			let mut pin: Option<u8> = None;
			// AND THE SAME WALK FINDS THE CAPTURE ROUTE, which is the same shape with every question
			// reversed: an audio INPUT converter and a pin that can be driven INTO. It is optional -
			// a codec with no input is a machine with no microphone, not a bring-up failure.
			let mut adc: Option<u8> = None;
			let mut input_pin: Option<u8> = None;
			for offset in 0..widgets {
				let node = first_widget + offset;
				let caps = self.parameter(node, hda::PARAM_WIDGET_CAPS).unwrap_or(0);
				match decisions::widget_kind(caps) {
					Widget::AudioOutput if converter.is_none() => converter = Some(node),
					Widget::AudioInput if adc.is_none() => adc = Some(node),
					Widget::PinComplex => {
						let pin_caps = self.parameter(node, hda::PARAM_PIN_CAPS).unwrap_or(0);
						if pin.is_none() && decisions::pin_can_output(pin_caps) {
							pin = Some(node);
						} else if input_pin.is_none() && decisions::pin_can_input(pin_caps) {
							input_pin = Some(node);
						}
					}
					_ => {}
				}
			}
			Ok(Route { group, converter: converter.ok_or(Bringup::NoRoute)?, pin: pin.ok_or(Bringup::NoRoute)?, adc, input_pin })
		}
	}

	// Send one verb and wait for its answer.
	//
	// THE RESPONSE RING'S WRITE POINTER IS THE LAST ENTRY WRITTEN, not the next slot, which is the
	// opposite of every other ring here. `drivers::hda` owns that arithmetic so this function cannot
	// get it subtly wrong in two places.
	unsafe fn verb(&mut self, node: u8, command: u32, payload: u32, wide: bool) -> Option<u32> {
		let mut deadline = self.deadline;
		unsafe {
			let word = if wide { decisions::verb_wide(self.codec, node, command, payload) } else { decisions::verb(self.codec, node, command, payload) };
			let write = r16(self.base + hda::REG_CORBWP) & 0xFF;
			let next = decisions::ring_next(write, RING_ENTRIES);
			w32(self.corb_virt + (next as u64) * hda::CORB_ENTRY_LEN as u64, word);
			core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
			w16(self.base + hda::REG_CORBWP, next);

			loop {
				let pointer = r16(self.base + hda::REG_RIRBWP) & 0xFF;
				if decisions::ring_has(pointer, self.rirb_read) {
					core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
					self.rirb_read = decisions::ring_next(self.rirb_read, RING_ENTRIES);
					let entry = self.rirb_virt + (self.rirb_read as u64) * hda::RIRB_ENTRY_LEN as u64;
					let answer = r32(entry);
					// AND THE STATUS IS ACKNOWLEDGED, which is what lets the controller carry on
					// fetching. Both bits are write-one-to-clear and both stall it: the response
					// count and the overrun.
					w8(self.base + hda::REG_RIRBSTS, hda::RIRBSTS_RESPONSE | hda::RIRBSTS_OVERRUN);
					return Some(answer);
				}
				if !deadline.waiting() {
					return None;
				}
			}
		}
	}

	unsafe fn parameter(&mut self, node: u8, parameter: u32) -> Option<u32> {
		unsafe { self.verb(node, hda::VERB_GET_PARAMETER, parameter, false) }
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: u64 = resources.device;
		if device == 0 {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device, 0, 0, 0);
		if sys_is_err(base) {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let mut controller = match bring_up(base, device) {
			Ok(controller) => controller,
			Err(why) => {
				let mut line: common::Bounded<160> = common::Bounded::new();
				line.push(b"driver.hda: bring-up gave up at ");
				line.push(&common::hex2(bind.info.bus));
				line.push(b":");
				line.push(&common::hex2(bind.info.dev));
				line.push(b".");
				line.push(&[b'0' + (bind.info.func % 10)]);
				line.push(b" - ");
				line.push(why.name());
				match why {
					Bringup::ResetEnter { gctl } | Bringup::ResetLeave { gctl } => {
						line.push(b", GCTL reads ");
						line.push(&common::hex2((gctl >> 24) as u8));
						line.push(&common::hex2((gctl >> 16) as u8));
						line.push(&common::hex2((gctl >> 8) as u8));
						line.push(&common::hex2(gctl as u8));
					}
					Bringup::CorbPointer { corbrp } => {
						line.push(b", CORBRP reads ");
						line.push(&common::hex2((corbrp >> 8) as u8));
						line.push(&common::hex2(corbrp as u8));
					}
					Bringup::NoCodec { statests } => {
						line.push(b", STATESTS reads ");
						line.push(&common::hex2((statests >> 8) as u8));
						line.push(&common::hex2(statests as u8));
					}
					Bringup::Unanswered { node, command } => {
						line.push(b": node ");
						line.decimal(node as u64);
						line.push(b" command ");
						line.push(&common::hex2((command >> 8) as u8));
						line.push(&common::hex2(command as u8));
					}
					_ => {}
				}
				line.push(b"\n");
				print(line.as_bytes());
				let code = if why.retryable() { driver_protocol::DriverFailureCode::DeviceNotResponding } else { driver_protocol::DriverFailureCode::UnsupportedDevice };
				common::failed(bootstrap, &bind, code);
			}
		};
		// We stand on the SERVICE channel, not the bootstrap one, exactly as `driver.virtio-snd`
		// does: DeviceManager being stopped is not this driver's client going away.
		let (service, far): (u64, u64) = channel().unwrap_or_else(|| exit());
		let mut report: common::Bounded<96> = common::Bounded::new();
		report.push(b"driver.hda: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", codec ");
		report.decimal(controller.codec as u64);
		// HOW MANY ANSWERED, BESIDE WHICH ONE IS DRIVEN. "codec 0" alone cannot distinguish a link
		// with one codec from a link with three whose first one happened to work, and those are
		// different machines to anybody reading the log after a device goes quiet.
		if controller.codecs > 1 {
			report.push(b" of ");
			report.decimal(controller.codecs as u64);
		}
		report.push(b", converter ");
		report.decimal(controller.converter as u64);
		report.push(b")");
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::AUDIO, far)]);
		serve(bootstrap, &bind, &mut controller, service)
	}
}

unsafe fn bring_up(base: u64, device: u64) -> Result<Controller, Bringup> {
	unsafe {
		// OUT OF RESET AND THEN INTO IT: the controller is held in reset while the bit is clear, so
		// leaving reset is setting the bit and waiting for it to READ BACK set. A driver that wrote
		// it and carried on talks to a controller that is still resetting.
		let mut deadline = Deadline::ticks(BRINGUP_TICKS);
		w32(base + hda::REG_GCTL, 0);
		while r32(base + hda::REG_GCTL) & hda::GCTL_RESET != 0 {
			if !deadline.waiting() {
				return Err(Bringup::ResetEnter { gctl: r32(base + hda::REG_GCTL) });
			}
		}
		w32(base + hda::REG_GCTL, hda::GCTL_RESET);
		while r32(base + hda::REG_GCTL) & hda::GCTL_RESET == 0 {
			if !deadline.waiting() {
				return Err(Bringup::ResetLeave { gctl: r32(base + hda::REG_GCTL) });
			}
		}
		// The codecs need a moment to report themselves after the link comes up; `STATESTS` is read
		// after a bounded settle rather than immediately.
		let settle = clock() + 1;
		while clock() < settle {
			core::hint::spin_loop();
		}

		// One region: the command ring, the response ring and the buffer descriptor list.
		let rings = dma(device, 4 * PAGE).ok_or(Bringup::NoDma)?;
		zero(rings.virt, rings.bytes);
		let corb_virt = rings.virt;
		let corb_phys = rings.phys;
		let rirb_virt = rings.virt + PAGE;
		let rirb_phys = rings.phys + PAGE;
		let bdl_virt = rings.virt + 3 * PAGE;
		let bdl_phys = rings.phys + 3 * PAGE;
		// The third page of the region was unused; the capture stream's descriptor list goes there
		// rather than in a second allocation.
		let capture_bdl_virt = rings.virt + 2 * PAGE;
		let capture_bdl_phys = rings.phys + 2 * PAGE;

		let size = decisions::ring_size_code(RING_ENTRIES).ok_or(Bringup::NoDma)?;
		w8(base + hda::REG_CORBCTL, 0);
		w8(base + hda::REG_RIRBCTL, 0);
		w8(base + hda::REG_CORBSIZE, size);
		w8(base + hda::REG_RIRBSIZE, size);
		w32(base + hda::REG_CORBLBASE, corb_phys as u32);
		w32(base + hda::REG_CORBUBASE, (corb_phys >> 32) as u32);
		w32(base + hda::REG_RIRBLBASE, rirb_phys as u32);
		w32(base + hda::REG_RIRBUBASE, (rirb_phys >> 32) as u32);
		// RESET THE COMMAND RING'S READ POINTER, WHICH IS A FOUR-STEP HANDSHAKE AND NOT A WRITE.
		//
		// The bit is written as one, and the specification then requires software to READ IT BACK AS
		// ONE - that read is what confirms the reset happened - and only then write zero and read
		// zero. The obvious sequence, write one and wait for it to clear, waits for something the
		// controller is specified never to do on its own: the register sits at 0x8000 for ever, which
		// is exactly what it read here before this was corrected.
		w16(base + hda::REG_CORBRP, 1 << 15);
		while r16(base + hda::REG_CORBRP) & (1 << 15) == 0 {
			if !deadline.waiting() {
				return Err(Bringup::CorbPointer { corbrp: r16(base + hda::REG_CORBRP) });
			}
		}
		w16(base + hda::REG_CORBRP, 0);
		while r16(base + hda::REG_CORBRP) & (1 << 15) != 0 {
			if !deadline.waiting() {
				return Err(Bringup::CorbPointer { corbrp: r16(base + hda::REG_CORBRP) });
			}
		}
		w16(base + hda::REG_CORBWP, 0);
		w16(base + hda::REG_RIRBWP, 1 << 15);
		// AS HIGH AS THE FIELD GOES, BECAUSE THIS DRIVER POLLS. `RINTCNT` is how many responses the
		// controller writes before it raises the response interrupt AND STOPS FETCHING COMMANDS until
		// the status bit is cleared. At one - which is what an interrupt-driven driver wants - a
		// polling driver gets its first answer and then nothing, with every pointer looking sane:
		// CORBWP moved, CORBRP did not. Measured exactly that way before this line was understood.
		w16(base + hda::REG_RINTCNT, 0xFF);
		w8(base + hda::REG_CORBCTL, hda::RING_RUN);
		w8(base + hda::REG_RIRBCTL, hda::RING_RUN);

		// GCAP: output streams are bits 15:12, input streams bits 11:8. The output descriptors come
		// AFTER the input ones in the register file, which is the trap here - a driver that used
		// descriptor zero for output would be programming an input stream on any controller with one.
		let gcap = r16(base + hda::REG_GCAP);
		let inputs = ((gcap >> 8) & 0x0F) as u64;
		let outputs = ((gcap >> 12) & 0x0F) as u64;
		if outputs == 0 {
			return Err(Bringup::NoStream);
		}
		let stream = base + hda::REG_STREAM_BASE + inputs * hda::REG_STREAM_STRIDE;
		// AND THE INPUT DESCRIPTORS ARE THE ONES BEFORE IT, which is the same trap read the other
		// way round: input stream zero is descriptor zero, and a controller reporting none has no
		// capture path whatever its codec says.
		let capture_stream = if inputs > 0 { base + hda::REG_STREAM_BASE } else { 0 };

		let statests = r16(base + hda::REG_STATESTS);
		let mut controller = Controller { base, device, corb_virt, rirb_virt, bdl_virt, bdl_phys, rirb_read: 0, deadline, codec: 0, codecs: 0, converter: 0, stream, audio: Dma { handle: 0, virt: 0, phys: 0, bytes: 0 }, period: 0, running: false, capture_stream, capture_bdl_virt, capture_bdl_phys, adc: None, capture: Dma { handle: 0, virt: 0, phys: 0, bytes: 0 }, capturing: false, capture_half: 0 };

		// EVERY CODEC THAT ANSWERS, COUNTED FIRST. A link can carry several, and with one attached
		// "take the first that answers" and "walk them until one has a route" are the same code -
		// which is why this driver did the first and called it the second for as long as the harness
		// presented one codec.
		let mut answered: [u8; hda::MAX_CODECS] = [0; hda::MAX_CODECS];
		let mut codecs: usize = 0;
		for address in decisions::codecs_present(statests) {
			// THE RIRB READ POINTER IS NOT RESET HERE, AND RESETTING IT WAS A LATENT DEFECT. The
			// response ring is ONE ring shared by every codec on the link, and this pointer is where
			// the driver has read up to in it - compared against the controller's write pointer. Putting
			// it back to zero when switching codecs makes the driver re-read entries it has already
			// consumed and take an old answer for a new one. It was harmless for exactly as long as one
			// codec was probed and the loop broke out of it.
			controller.codec = address;
			match controller.parameter(0, hda::PARAM_VENDOR_ID) {
				Some(vendor) if decisions::codec_answered(vendor) => {
					if codecs < answered.len() {
						answered[codecs] = address;
						codecs += 1;
					}
				}
				// The codec is there and answered with the absence: try the next address.
				Some(_) => continue,
				// NOTHING CAME BACK AT ALL, which is the rings rather than the codec, and is a
				// different problem from an address with nothing behind it.
				None => return Err(Bringup::Unanswered { node: 0, command: hda::PARAM_VENDOR_ID }),
			}
		}
		if codecs == 0 {
			// STATESTS IS THE HALF THAT DISTINGUISHES TWO DIFFERENT PROBLEMS. Zero means no codec
			// announced itself on the link at all, which is a settle or a link question; non-zero
			// means one did and then answered its identity with the absence, which is a verb
			// question. The sentence is the same either way and the number is not.
			return Err(Bringup::NoCodec { statests });
		}
		controller.codecs = codecs as u8;

		// AND THE ROUTE IS LOOKED FOR ON EACH OF THEM IN TURN. A codec that answers its identity and
		// then has no audio group, or no converter, or no pin that can drive one, is not a bring-up
		// failure while another codec on the same link is fine - it is the first of several, and
		// stopping there is how a machine with a modem codec at address zero ends up with no sound.
		let mut chosen: Option<(u8, Route)> = None;
		let mut refusal = Bringup::NoRoute;
		for &address in answered.iter().take(codecs) {
			controller.codec = address;
			match controller.route() {
				Ok(route) => {
					chosen = Some((address, route));
					break;
				}
				// REMEMBERED AND NOT RETURNED. The last codec's reason is the one worth reporting
				// when none of them worked, and returning the first would name a codec the driver
				// went on to look past.
				Err(why) => refusal = why,
			}
		}
		let Some((address, route)) = chosen else { return Err(refusal) };
		controller.codec = address;
		let Route { group, converter, pin, adc, input_pin } = route;
		controller.converter = converter;

		// Power both up, unmute the pin and its amplifier, and point the pin at the converter's
		// output. The amplifier payload sets output, left and right, at the top of its range.
		for node in [group, converter, pin] {
			controller.verb(node, hda::VERB_SET_POWER_STATE, 0, false);
		}
		controller.verb(pin, hda::VERB_SET_PIN_CONTROL, 0x40, false);
		controller.verb(pin, hda::VERB_SET_AMP_GAIN, 0xB000 | 0x7F, true);
		controller.verb(converter, hda::VERB_SET_AMP_GAIN, 0xB000 | 0x7F, true);

		let format = decisions::format(RATE_HZ, BITS, CHANNELS).ok_or(Bringup::Format)?;
		controller.verb(converter, hda::VERB_SET_STREAM_FORMAT, format as u32, true);
		// Stream 1, channel 0: the tag the stream descriptor below carries.
		controller.verb(converter, hda::VERB_SET_STREAM_CHANNEL, 1 << 4, false);
		w16(stream + hda::SD_FMT, format);

		// THE CAPTURE ROUTE, ON STREAM TAG TWO. A tag is what ties a converter to a stream
		// descriptor, and two converters sharing one tag is two streams into one buffer.
		if let (Some(adc), Some(input_pin), true) = (adc, input_pin, controller.capture_stream != 0) {
			for node in [adc, input_pin] {
				controller.verb(node, hda::VERB_SET_POWER_STATE, 0, false);
			}
			// PIN CONTROL 0x20 IS *IN* ENABLE and 0x40 is OUT. Setting the output bit on a
			// microphone pin drives the jack instead of listening to it.
			controller.verb(input_pin, hda::VERB_SET_PIN_CONTROL, 0x20, false);
			// The amplifier payload for an INPUT amp sets bit 14 where the output amp sets 15.
			controller.verb(input_pin, hda::VERB_SET_AMP_GAIN, 0x7000 | 0x7F, true);
			controller.verb(adc, hda::VERB_SET_AMP_GAIN, 0x7000 | 0x7F, true);
			controller.verb(adc, hda::VERB_SET_STREAM_FORMAT, format as u32, true);
			controller.verb(adc, hda::VERB_SET_STREAM_CHANNEL, 2 << 4, false);
			w16(controller.capture_stream + hda::SD_FMT, format);
			controller.adc = Some(adc);
		}

		Ok(controller)
	}
}

// Make the audio buffer hold two periods of `bytes` and point the stream at it.
unsafe fn configure(controller: &mut Controller, bytes: u64) -> bool {
	unsafe {
		if controller.period == bytes && controller.audio.virt != 0 {
			return true;
		}
		stop(controller);
		if controller.audio.handle != 0 {
			dma_buffer_unmap(controller.audio.handle);
			close(controller.audio.handle);
			controller.audio = Dma { handle: 0, virt: 0, phys: 0, bytes: 0 };
		}
		let Some(audio) = dma(controller.device, (bytes * 2).next_multiple_of(PAGE)) else { return false };
		zero(audio.virt, audio.bytes);
		// TWO DESCRIPTORS, WHICH IS THE MINIMUM THE SPECIFICATION ALLOWS and is also what makes the
		// buffer usable: the controller plays them in a cycle, so one can be refilled while the
		// other is playing.
		for half in 0..2u64 {
			let entry = controller.bdl_virt + half * hda::BDL_ENTRY_LEN as u64;
			let at = audio.phys + half * bytes;
			w32(entry, at as u32);
			w32(entry + 4, (at >> 32) as u32);
			w32(entry + 8, bytes as u32);
			w32(entry + 12, 0);
		}
		controller.audio = audio;
		controller.period = bytes;
		w32(controller.stream + hda::SD_BDLPL, controller.bdl_phys as u32);
		w32(controller.stream + hda::SD_BDLPU, (controller.bdl_phys >> 32) as u32);
		w32(controller.stream + hda::SD_CBL, (bytes * 2) as u32);
		w16(controller.stream + hda::SD_LVI, 1);
		true
	}
}

unsafe fn start(controller: &mut Controller) {
	unsafe {
		if controller.running {
			return;
		}
		// The stream tag goes in the top nibble of the third control byte; stream 1 matches the
		// channel the converter was told.
		w8(controller.stream + hda::SD_CTL + 2, 1 << 4);
		let ctl = r8(controller.stream + hda::SD_CTL);
		w8(controller.stream + hda::SD_CTL, ctl | hda::SD_RUN);
		controller.running = true;
	}
}

unsafe fn stop(controller: &mut Controller) {
	unsafe {
		if !controller.running {
			return;
		}
		let ctl = r8(controller.stream + hda::SD_CTL);
		w8(controller.stream + hda::SD_CTL, ctl & !hda::SD_RUN);
		controller.running = false;
	}
}

// Make the capture buffer hold two periods and point the input stream at it.
//
// THE DESCRIPTORS ASK FOR A COMPLETION STATUS, which the playback ones do not need: playback is
// paced by the consumer handing over the next period, and capture is paced by the DEVICE filling
// one. `SD_BUFFER_COMPLETE` is how "a period is ready" is asked without an interrupt.
unsafe fn configure_capture(controller: &mut Controller, bytes: u64) -> bool {
	unsafe {
		if controller.capture.virt != 0 && controller.capture.bytes >= bytes * 2 {
			return true;
		}
		if controller.capture.handle != 0 {
			dma_buffer_unmap(controller.capture.handle);
			close(controller.capture.handle);
			controller.capture = Dma { handle: 0, virt: 0, phys: 0, bytes: 0 };
		}
		let Some(buffer) = dma(controller.device, (bytes * 2).next_multiple_of(PAGE)) else { return false };
		zero(buffer.virt, buffer.bytes);
		for half in 0..2u64 {
			let entry = controller.capture_bdl_virt + half * hda::BDL_ENTRY_LEN as u64;
			let at = buffer.phys + half * bytes;
			w32(entry, at as u32);
			w32(entry + 4, (at >> 32) as u32);
			w32(entry + 8, bytes as u32);
			w32(entry + 12, hda::BDL_INTERRUPT_ON_COMPLETION);
		}
		controller.capture = buffer;
		w32(controller.capture_stream + hda::SD_BDLPL, controller.capture_bdl_phys as u32);
		w32(controller.capture_stream + hda::SD_BDLPU, (controller.capture_bdl_phys >> 32) as u32);
		w32(controller.capture_stream + hda::SD_CBL, (bytes * 2) as u32);
		w16(controller.capture_stream + hda::SD_LVI, 1);
		true
	}
}

// Fill `out` with one captured period, or answer false.
//
// THE STREAM RUNS BETWEEN REQUESTS. Starting and stopping it per period would throw away the
// samples that arrived while nobody was asking, which is a recording with holes in it wherever the
// consumer was busy - and the consumer is what paces a capture stream least.
unsafe fn capture_period(controller: &mut Controller, out: &mut [u8]) -> bool {
	unsafe {
		if controller.adc.is_none() || controller.capture_stream == 0 {
			return false;
		}
		let bytes = out.len() as u64;
		if !configure_capture(controller, bytes) {
			return false;
		}
		if !controller.capturing {
			// Clear whatever the last session left, then run with the tag the converter was told.
			w8(controller.capture_stream + hda::SD_STS, hda::SD_BUFFER_COMPLETE);
			w8(controller.capture_stream + hda::SD_CTL + 2, 2 << 4);
			let ctl = r8(controller.capture_stream + hda::SD_CTL);
			w8(controller.capture_stream + hda::SD_CTL, ctl | hda::SD_RUN);
			controller.capturing = true;
			controller.capture_half = 0;
		}
		// WAIT FOR A PERIOD RATHER THAN FOR A TIME. A deadline in ticks would be a guess about the
		// sample rate; the completion bit is the device saying a descriptor finished.
		let mut deadline = Deadline::ticks(CAPTURE_TICKS);
		loop {
			let status = r8(controller.capture_stream + hda::SD_STS);
			if status & hda::SD_BUFFER_COMPLETE != 0 {
				// STICKY AND WRITE-ONE-TO-CLEAR: a reader that only read would see the first
				// period's completion for ever and hand back the same half every time.
				w8(controller.capture_stream + hda::SD_STS, hda::SD_BUFFER_COMPLETE);
				break;
			}
			if !deadline.waiting() {
				return false;
			}
		}
		core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
		let at = controller.capture.virt + controller.capture_half * bytes;
		core::ptr::copy_nonoverlapping(at as *const u8, out.as_mut_ptr(), out.len());
		controller.capture_half ^= 1;
		true
	}
}

unsafe fn stop_capture(controller: &mut Controller) {
	unsafe {
		if !controller.capturing {
			return;
		}
		let ctl = r8(controller.capture_stream + hda::SD_CTL);
		w8(controller.capture_stream + hda::SD_CTL, ctl & !hda::SD_RUN);
		w8(controller.capture_stream + hda::SD_STS, hda::SD_BUFFER_COMPLETE);
		controller.capturing = false;
	}
}

// Serve AudioService over `driver_protocol::audio`.
//
// THE WIRE IS THE SHARED ONE NOW, AND IT WAS NOT BEFORE (2026-09-19). This driver read a length and
// treated anything non-empty as a period, so the ONE-BYTE COMMANDS that the other server of this
// same provider kind answers - `CMD_CAPTURE` and `CMD_CAPTURE_STOP` - were played as one-byte
// periods. A consumer that asked this machine for a microphone got a click.
unsafe fn serve(bootstrap: u64, bind: &common::Bind, controller: &mut Controller, service: u64) -> ! {
	unsafe {
		// The largest period AudioService is expected to hand over. A longer one is refused rather
		// than truncated, because half a period played is a click rather than an error.
		let mut period = [0u8; 16384];
		let mut half: u64 = 0;
		loop {
			if !common::serve_or_answer(bootstrap, bind, service) {
				stop(controller);
				stop_capture(controller);
				common::finish_stop(bootstrap, bind, controller.device, true);
				exit();
			}
			let Received::Message { len, .. } = recv_blocking(service, &mut period) else {
				stop(controller);
				stop_capture(controller);
				exit();
			};
			match audio::message(&period[..len]) {
				audio::Message::EndPlayback => {
					stop(controller);
					send_blocking(service, audio::OK, 0);
				}
				audio::Message::Play => {
					if !configure(controller, len as u64) {
						send_blocking(service, audio::REFUSED, 0);
						continue;
					}
					// Into the half the controller is not playing, then run.
					let at = controller.audio.virt + half * controller.period;
					core::ptr::copy_nonoverlapping(period.as_ptr(), at as *mut u8, len);
					core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
					half ^= 1;
					start(controller);
					send_blocking(service, audio::OK, 0);
				}
				audio::Message::Capture => {
					// A REFUSAL IS AN EMPTY REPLY, which is how "this codec has no input" reaches
					// the consumer without a second message shape - and why it cannot be mistaken
					// for samples: a period is never empty.
					let mut filled = [0u8; audio::PERIOD_BYTES as usize];
					if capture_period(controller, &mut filled) {
						send_blocking(service, &filled, 0);
					} else {
						send_blocking(service, audio::REFUSED, 0);
					}
				}
				audio::Message::EndCapture => {
					stop_capture(controller);
					send_blocking(service, audio::OK, 0);
				}
				// A LENGTH THIS WIRE HAS NO SHAPE FOR IS ANSWERED AND NOT PLAYED. Playing a short
				// message as a period is part of a period played as though it were whole.
				audio::Message::Unknown => {
					send_blocking(service, audio::REFUSED, 0);
				}
			}
		}
	}
}
