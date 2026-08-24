//! The secondary-start cases QEMU has no switch for.
//!
//! Every one of these is a thing firmware is allowed to do and a machine here has never done: refuse
//! a call, take a call and release the core late, name the running core twice, describe more cores
//! than the kernel holds. They are driven through a scripted [`Firmware`] rather than a machine.

use core::sync::atomic::AtomicU32;
use smpboot::{Bringup, Event, Firmware, SLOT_ABANDONED, SLOT_FREE, SLOT_ONLINE, Topology};

/// What the scripted firmware does with one target.
#[derive(Clone, Copy)]
enum Behaviour {
	/// Takes the call, reports in during its own wait.
	Arrives,
	/// Will not take the call.
	Refuses(i64),
	/// Takes the call and reports in `n` waits later - so during another core's attempt, holding
	/// the id it was given.
	ArrivesLate(usize),
	/// Takes the call and is never heard from.
	Never,
	/// Takes the call, reports in, and then reports in a second time.
	ArrivesTwice,
}

struct Fake<'a> {
	script: Vec<(u64, Behaviour)>,
	bringup: &'a Bringup<'a>,
	/// Secondaries that have claimed an id, which is what the kernel's `SMP_ONLINE` counts.
	online: u32,
	waits: usize,
	/// (logical id, the wait it reports in on, whether it reports twice).
	inbound: Vec<(u64, usize, bool)>,
	events: Vec<String>,
	/// Cores that arrived and found their id was not theirs.
	turned_away: usize,
}

impl<'a> Fake<'a> {
	fn new(bringup: &'a Bringup<'a>, script: &[(u64, Behaviour)]) -> Self {
		Self { script: script.to_vec(), bringup, online: 0, waits: 0, inbound: Vec::new(), events: Vec::new(), turned_away: 0 }
	}

	fn behaviour(&self, target: u64) -> Behaviour {
		self.script.iter().find(|(t, _)| *t == target).map(|(_, b)| *b).expect("target not in script")
	}

	/// One core reaching the kernel's secondary entry: it claims the id it was handed, and only a
	/// core whose claim succeeds counts itself online. This is the arriving core's whole protocol.
	fn arrive(&mut self, logical_id: u64) {
		if self.bringup.claim(logical_id) {
			self.online += 1;
		} else {
			self.turned_away += 1;
		}
	}
}

impl Firmware for Fake<'_> {
	fn start(&mut self, target: u64, logical_id: u64) -> i64 {
		match self.behaviour(target) {
			Behaviour::Refuses(status) => return status,
			Behaviour::Arrives => self.inbound.push((logical_id, self.waits + 1, false)),
			Behaviour::ArrivesTwice => self.inbound.push((logical_id, self.waits + 1, true)),
			Behaviour::ArrivesLate(n) => self.inbound.push((logical_id, self.waits + 1 + n, false)),
			Behaviour::Never => {}
		}
		0
	}

	fn await_report(&mut self, reported: u32) -> bool {
		self.waits += 1;
		let due: Vec<(u64, bool)> = self.inbound.iter().filter(|(_, w, _)| *w == self.waits).map(|(id, _, twice)| (*id, *twice)).collect();
		for (id, twice) in due {
			self.arrive(id);
			if twice {
				self.arrive(id);
			}
		}
		self.online >= reported
	}

	fn note(&mut self, event: Event) {
		self.events.push(match event {
			Event::Refused { target, logical_id, status } => format!("refused {target} id {logical_id} status {status}"),
			Event::Abandoned { target, logical_id } => format!("abandoned {target} id {logical_id}"),
			Event::Online { target, logical_id } => format!("online {target} id {logical_id}"),
			Event::PoolExhausted { target } => format!("exhausted {target}"),
		});
	}
}

fn slots(n: usize) -> Vec<AtomicU32> {
	(0..n).map(|_| AtomicU32::new(SLOT_FREE)).collect()
}

// ---- the topology, which is what an array is sized from -------------------------------------

#[test]
fn the_running_core_is_not_a_core_to_start() {
	let mut buf = [0u64; 8];
	let t = Topology::resolve(&[0, 1, 2, 3], 0, &mut buf);
	assert_eq!(t.secondaries(), &[1, 2, 3]);
	assert!(t.boot_declared());
	assert_eq!(t.slots(), 4);
	assert_eq!(t.declared(), 4);
}

#[test]
fn the_boot_core_is_skipped_wherever_the_tree_puts_it() {
	// OpenSBI boots on hart 1 as often as hart 0, and the id list is in tree order, not id order.
	let mut buf = [0u64; 8];
	let t = Topology::resolve(&[0, 1, 2, 3], 1, &mut buf);
	assert_eq!(t.secondaries(), &[0, 2, 3]);
	assert_eq!(t.slots(), 4);
}

#[test]
fn a_tree_that_omits_the_running_core_still_sizes_for_it() {
	// THE DEFECT THIS ANSWERS: four declared cores, none of them this one, is four secondaries and
	// five logical ids. Sizing a per-CPU table or a stack block from the DECLARED count leaves the
	// last core writing one entry past the end.
	let mut buf = [0u64; 8];
	let t = Topology::resolve(&[4, 5, 6, 7], 0, &mut buf);
	assert_eq!(t.secondaries(), &[4, 5, 6, 7]);
	assert!(!t.boot_declared());
	assert_eq!(t.slots(), 5);
}

#[test]
fn a_repeated_id_is_dropped_and_counted() {
	let mut buf = [0u64; 8];
	let t = Topology::resolve(&[0, 1, 1, 2], 0, &mut buf);
	assert_eq!(t.secondaries(), &[1, 2]);
	assert_eq!(t.duplicates(), 1);
	assert_eq!(t.slots(), 3);
}

#[test]
fn the_running_core_named_twice_is_started_neither_time() {
	let mut buf = [0u64; 8];
	let t = Topology::resolve(&[0, 1, 0], 0, &mut buf);
	assert_eq!(t.secondaries(), &[1]);
	assert!(t.boot_declared());
	assert_eq!(t.duplicates(), 0);
}

#[test]
fn more_cores_than_the_pool_holds_park_the_remainder() {
	let mut buf = [0u64; 4];
	let t = Topology::resolve(&[0, 1, 2, 3, 4, 5], 0, &mut buf);
	assert_eq!(t.secondaries(), &[1, 2, 3]);
	assert_eq!(t.parked(), 2);
	assert_eq!(t.slots(), 4);
	assert_eq!(t.declared(), 6);
}

#[test]
fn a_pool_that_omits_the_boot_core_leaves_room_for_it() {
	// Four slots, four declared cores, none of them this one: three get started and the fourth is
	// parked, because the running core's id is not one of the four to give away.
	let mut buf = [0u64; 4];
	let t = Topology::resolve(&[4, 5, 6, 7], 0, &mut buf);
	assert_eq!(t.secondaries(), &[4, 5, 6]);
	assert_eq!(t.parked(), 1);
	assert_eq!(t.slots(), 4);
}

// ---- the attempts ----------------------------------------------------------------------------

#[test]
fn every_core_that_answers_gets_the_next_id() {
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::Arrives), (2, Behaviour::Arrives), (3, Behaviour::Arrives)]);
	let out = b.run(&[1, 2, 3], &mut fw);
	assert_eq!(out.online, 3);
	assert_eq!(out.refused, 0);
	assert_eq!(out.abandoned, 0);
	assert_eq!(out.ids_used, 4);
	assert_eq!(fw.events, ["online 1 id 1", "online 2 id 2", "online 3 id 3"]);
}

#[test]
fn a_refused_call_gives_its_id_to_the_next_core() {
	// Nothing was released, so nothing can arrive holding this id.
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::Refuses(-3)), (2, Behaviour::Arrives)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!((out.online, out.refused, out.abandoned), (1, 1, 0));
	assert_eq!(fw.events, ["refused 1 id 1 status -3", "online 2 id 1"]);
	assert_eq!(out.ids_used, 2, "a refused attempt takes no id with it");
	assert_eq!(b.state(1), SLOT_ONLINE);
}

#[test]
fn a_timeout_abandons_its_id_and_the_next_core_gets_the_following_one() {
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::Never), (2, Behaviour::Arrives)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!((out.online, out.refused, out.abandoned), (1, 0, 1));
	assert_eq!(fw.events, ["abandoned 1 id 1", "online 2 id 2"]);
	assert_eq!(b.state(1), SLOT_ABANDONED);
	assert_eq!(b.state(2), SLOT_ONLINE);
	// TWO CORES ONLINE, THREE IDS IN USE, AND THE WORKING SECONDARY HOLDS ID 2. Anything sized from
	// the online count would be indexed out of range by the core that came up.
	assert_eq!(out.ids_used, 3);
}

#[test]
fn a_core_that_arrives_after_its_attempt_was_abandoned_is_turned_away() {
	// THE COLLISION THIS WHOLE RULE EXISTS FOR. Core 1 takes the call and is released two waits
	// later - by which time id 1 has been abandoned. It must not initialize per-CPU state under an
	// id somebody else may hold, and it must not be counted as the arrival the kernel is waiting
	// for: core 2 is answered by core 2 or by nobody.
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::ArrivesLate(1)), (2, Behaviour::Arrives)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!((out.online, out.abandoned), (1, 1));
	assert_eq!(fw.turned_away, 1, "the late core claimed an id that was no longer its own");
	assert_eq!(fw.events, ["abandoned 1 id 1", "online 2 id 2"]);
	assert_eq!(b.state(1), SLOT_ABANDONED);
}

#[test]
fn a_late_core_cannot_stand_in_for_the_core_that_never_came() {
	// Same shape, with nothing behind it: core 1 arrives during core 2's wait and core 2 never
	// does. A bare tally would have called that "core 2 up".
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::ArrivesLate(1)), (2, Behaviour::Never)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!((out.online, out.abandoned), (0, 2));
	assert_eq!(fw.turned_away, 1);
	assert_eq!(fw.events, ["abandoned 1 id 1", "abandoned 2 id 2"]);
}

#[test]
fn an_id_is_claimed_once() {
	let s = slots(4);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::ArrivesTwice), (2, Behaviour::Arrives)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!((out.online, out.abandoned), (2, 0));
	assert_eq!(fw.turned_away, 1, "the second report on the same id was accepted");
	assert_eq!(fw.events, ["online 1 id 1", "online 2 id 2"]);
}

#[test]
fn an_id_the_pool_does_not_have_is_never_offered() {
	// Defensive: `Topology::resolve` parks the remainder, so a caller reaching this sized something
	// from a different number. It refuses rather than indexing past the table.
	let s = slots(2);
	let b = Bringup::new(&s);
	let mut fw = Fake::new(&b, &[(1, Behaviour::Arrives), (2, Behaviour::Arrives)]);
	let out = b.run(&[1, 2], &mut fw);
	assert_eq!(out.online, 1);
	assert_eq!(fw.events, ["online 1 id 1", "exhausted 2"]);
}

#[test]
fn an_id_outside_the_table_cannot_be_claimed() {
	let s = slots(2);
	let b = Bringup::new(&s);
	assert!(!b.claim(2));
	assert!(!b.claim(u64::MAX));
}

#[test]
fn an_id_nobody_was_offered_cannot_be_claimed() {
	// A core released by firmware nobody asked - or one arriving with a stale argument - finds its
	// id free rather than pending, and free is not claimable.
	let s = slots(4);
	let b = Bringup::new(&s);
	assert!(!b.claim(1));
	assert_eq!(b.state(1), SLOT_FREE);
}
