use super::*;
use alloc::vec;

// THE EQUATION'S CASES, worked by hand from ACPI 6.5's 11.1.5.1 - `ΔP[%] = _TC1 * (Tn - Tn-1) + _TC2 * (Tn - Tt)` and
// `Pn = Pn-1 - ΔP` held between 0 and 100 % - which states the equation and no numeric example. Temperatures in tenths
// of a kelvin make ΔP come out in tenths of a percent: the limit's own unit.
const ZONE: Passive = Passive { tc1: 4, tc2: 3, target: 3582 };

#[test]
fn passive_cooling_follows_the_equation_sample_by_sample() {
	let mut cooling = Cooling::default();
	assert_eq!(cooling.sample(ZONE, 3570), Cooling { engaged: false, limit: FULL, last: 3570 }, "under _PSV nothing is engaged");
	// 358.4 K, 0.2 K past the target, the episode's first sample: ΔP = 4 * 0 + 3 * 2 = 6 tenths of a percent.
	assert_eq!(cooling.sample(ZONE, 3584), Cooling { engaged: true, limit: 994, last: 3584 });
	// 359.4 K: ΔP = 4 * (3594 - 3584) + 3 * (3594 - 3582) = 40 + 36 = 76 - the limit falls from 99.4 % to 91.8 %.
	assert_eq!(cooling.sample(ZONE, 3594).limit, 918);
	// 360.0 K: ΔP = 4 * 6 + 3 * 18 = 78 - to 84.0 %.
	assert_eq!(cooling.sample(ZONE, 3600).limit, 840);
	// Cooling, still past the target: 359.0 K - ΔP = 4 * -10 + 3 * 8 = -16 - the limit climbs to 85.6 %.
	assert_eq!(cooling.sample(ZONE, 3590).limit, 856);
	// Under the target: 357.0 K - ΔP = 4 * -20 + 3 * -12 = -116 - to 97.2 %, still engaged until it is back at full.
	let under = cooling.sample(ZONE, 3570);
	assert_eq!((under.engaged, under.limit), (true, 972));
	// 356.0 K - ΔP = 4 * -10 + 3 * -22 = -106 - capped at 100 %, and the zone, under _PSV, is released.
	let released = cooling.sample(ZONE, 3560);
	assert_eq!((released.engaged, released.limit), (false, FULL));
}

#[test]
fn the_limit_is_held_between_nothing_and_full() {
	let mut cooling = Cooling::default();
	cooling.sample(Passive { tc1: 50, tc2: 50, target: 3500 }, 3500);
	// ΔP = 50 * 100 + 50 * 100 = 10000 tenths - far past the whole range: the limit stops at zero.
	assert_eq!(cooling.sample(Passive { tc1: 50, tc2: 50, target: 3500 }, 3600).limit, 0);
}

#[test]
fn a_limit_becomes_the_fastest_level_it_allows_and_idle_past_the_slowest() {
	// Four states - 3000, 2400, 1800, 1200 MHz - and two throttling levels at 75 % and 50 % of the slowest.
	let capacities = [1000, 800, 600, 400, 300, 200];
	assert_eq!(limit_level(1000, &capacities), (0, 0));
	assert_eq!(limit_level(840, &capacities), (1, 0), "84 % allows 2400 MHz, not 3000");
	assert_eq!(limit_level(400, &capacities), (3, 0));
	assert_eq!(limit_level(350, &capacities), (4, 0), "past the performance states: throttling");
	assert_eq!(limit_level(150, &capacities), (5, 250), "past the slowest level: a quarter injected for 15 % of 20 %");
	assert_eq!(limit_level(0, &capacities), (5, MAX_INJECT_PERMILLE), "never more than a half");
	assert_eq!(limit_level(700, &[]), (0, 300), "a core without a table is cooled by idle alone");
}

#[test]
fn ppc_and_the_thermal_cap_are_obeyed_above_any_profile() {
	// Eight performance states and two throttling levels.
	assert_eq!(window(Profile::Performance, 10, 8, 0, 0), Window { cap: 0, floor: 0 });
	assert_eq!(window(Profile::Balanced, 10, 8, 0, 0), Window { cap: 0, floor: 7 });
	assert_eq!(window(Profile::PowerSaving, 10, 8, 0, 0), Window { cap: 3, floor: 7 });
	assert_eq!(window(Profile::Performance, 10, 8, 2, 0), Window { cap: 2, floor: 2 }, "_PPC caps even the performance profile");
	assert_eq!(window(Profile::Balanced, 10, 8, 0, 9), Window { cap: 9, floor: 9 }, "the thermal cap reaches the throttling levels");
	assert_eq!(window(Profile::Balanced, 10, 8, 20, 0), Window { cap: 7, floor: 7 }, "a _PPC past the table is its slowest state");
	assert_eq!(window(Profile::Balanced, 1, 1, 0, 0), Window { cap: 0, floor: 0 });
}

#[test]
fn the_default_profile_follows_the_power_source() {
	assert_eq!(default_profile(false), Profile::Balanced);
	assert_eq!(default_profile(true), Profile::PowerSaving);
	assert_eq!(cooling_mode(Profile::PowerSaving), 1);
	assert_eq!(cooling_mode(Profile::Performance), 0);
	assert!(energy_preference(Profile::Performance) < energy_preference(Profile::Balanced) && energy_preference(Profile::Balanced) < energy_preference(Profile::PowerSaving));
}

#[test]
fn a_fan_runs_faster_with_each_active_trip_passed() {
	// _AC0 at 70 C, _AC1 at 60 C, both listing the fan.
	let trips = [3432, 3332];
	assert_eq!(active_percent(3300, &trips), 0);
	assert_eq!(active_percent(3332, &trips), 50, "at a trip is past it");
	assert_eq!(active_percent(3500, &trips), 100);
	assert_eq!(active_percent(3500, &[]), 0, "a fan no trip lists is the curve's alone");
}

#[test]
fn a_curve_is_checked_and_read_as_steps() {
	let curve = default_curve();
	assert_eq!(check_curve(&curve), Ok(()));
	assert_eq!(curve_percent(&curve, 3000), 0);
	assert_eq!(curve_percent(&curve, 3332), 50);
	assert_eq!(curve_percent(&curve, 3400), 50);
	assert_eq!(curve_percent(&curve, 3600), 100);
	assert_eq!(check_curve(&[]), Err(CurveRefusal::Size));
	assert_eq!(check_curve(&[CurvePoint { temperature: 3300, percent: 50 }, CurvePoint { temperature: 3300, percent: 60 }]), Err(CurveRefusal::Order));
	assert_eq!(check_curve(&[CurvePoint { temperature: 3300, percent: 101 }]), Err(CurveRefusal::Order));
	assert_eq!(check_curve(&[CurvePoint { temperature: 3300, percent: 60 }, CurvePoint { temperature: 3400, percent: 40 }]), Err(CurveRefusal::Falling));
}

#[test]
fn a_share_becomes_the_value_the_fan_takes() {
	assert_eq!(fan_control(&FanControl::PowerState, 0), 0);
	assert_eq!(fan_control(&FanControl::PowerState, 1), 1);
	assert_eq!(fan_control(&FanControl::FineGrain { step: 10 }, 41), 50, "rounded up to the step, never slower");
	assert_eq!(fan_control(&FanControl::FineGrain { step: 0 }, 41), 41);
	// _FPS levels, listed out of order: control 2 is off, 3 is 1500 rpm, 1 is 3000 rpm.
	let levels = FanControl::Levels(vec![(1, 3000), (2, 0), (3, 1500)]);
	assert_eq!(fan_control(&levels, 0), 2);
	assert_eq!(fan_control(&levels, 30), 3);
	assert_eq!(fan_control(&levels, 50), 3);
	assert_eq!(fan_control(&levels, 51), 1);
	assert_eq!(fan_control(&levels, 100), 1);
}

#[test]
fn critical_trips_act_once_per_crossing_crt_before_hot() {
	let mut trips = Trips::default();
	let (critical, hot) = (Some(3732), Some(3682));
	assert_eq!(trips.reading(3600, critical, hot), Critical::Nothing);
	assert_eq!(trips.reading(3690, critical, hot), Critical::Hibernate);
	assert_eq!(trips.reading(3700, critical, hot), Critical::Nothing, "once per crossing");
	assert_eq!(trips.reading(3740, critical, hot), Critical::PowerOff);
	assert_eq!(trips.reading(3750, critical, hot), Critical::Nothing);
	let mut straight = Trips::default();
	assert_eq!(straight.reading(3800, critical, hot), Critical::PowerOff, "past both at once: the graver alone");
	assert_eq!(straight.reading(3600, critical, hot), Critical::Nothing);
	assert_eq!(straight.reading(3740, critical, hot), Critical::PowerOff, "a new crossing acts again");
	assert_eq!(Trips::default().reading(9999, None, None), Critical::Nothing);
}
