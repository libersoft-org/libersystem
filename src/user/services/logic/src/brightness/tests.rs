use super::*;
use alloc::vec;

fn list(levels: &[u32]) -> Scale {
	Scale::new(levels.to_vec()).expect("a list")
}

#[test]
fn a_list_is_sorted_and_deduplicated_and_an_empty_one_or_an_inverted_range_is_refused() {
	assert_eq!(Scale::new(vec![50, 10, 10, 100]), Some(Scale::Levels(vec![10, 50, 100])));
	assert_eq!(Scale::new(vec![]), None);
	assert_eq!(Scale::range(10, 5), None);
	assert_eq!(Scale::range(0, 100), Some(Scale::Range { minimum: 0, maximum: 100 }));
}

#[test]
fn the_floor_is_the_lowest_level_at_or_above_five_percent_of_the_span() {
	assert_eq!(list(&[0, 2, 10, 50, 100]).floor(), 10, "five percent of 0..100 is 5; 10 is the lowest listed at or above it");
	assert_eq!(list(&[0, 5, 100]).floor(), 5, "exactly at it");
	assert_eq!(Scale::range(0, 100).unwrap().floor(), 5);
	assert_eq!(Scale::range(0, 255).unwrap().floor(), 13, "a fraction rounds up: zero is never the floor of a span");
	assert_eq!(list(&[40]).floor(), 40, "a single level is its own floor");
}

#[test]
fn a_step_takes_the_next_listed_level_or_a_sixteenth_of_a_range_and_stops_at_either_end() {
	let levels = list(&[0, 10, 20, 40, 100]);
	assert_eq!(levels.step(20, 1), 40);
	assert_eq!(levels.step(20, -2), 0);
	assert_eq!(levels.step(100, 3), 100, "stops at the top");
	assert_eq!(levels.step(23, 1), 40, "a level off the list starts from the nearest");
	let range = Scale::range(0, 160).unwrap();
	assert_eq!(range.step(80, 1), 90);
	assert_eq!(range.step(5, -1), 0);
	assert_eq!(Scale::range(0, 10).unwrap().step(3, 1), 4, "a span under sixteen steps by one unit");
}

#[test]
fn a_percent_maps_to_the_nearest_level() {
	let levels = list(&[0, 10, 20, 40, 100]);
	assert_eq!(levels.percent(50), 40);
	assert_eq!(levels.percent(100), 100);
	assert_eq!(Scale::range(10, 110).unwrap().percent(50), 60);
}

#[test]
fn a_set_lands_at_the_floor_unless_zero_is_asked_for_explicitly() {
	let levels = list(&[0, 2, 10, 50, 100]);
	assert_eq!(resolve(&levels, Some(50), Target::Level(2), false), Ok(10), "an ordinary set stops at the floor");
	assert_eq!(resolve(&levels, Some(50), Target::Level(0), true), Ok(0), "an explicit zero reaches zero");
	assert_eq!(resolve(&levels, Some(50), Target::Percent(0), true), Ok(0));
	assert_eq!(resolve(&levels, Some(50), Target::Percent(0), false), Ok(10));
	assert_eq!(resolve(&levels, Some(10), Target::Steps(-1), true), Ok(10), "a step never goes below the floor, zero allowed or not");
	assert_eq!(resolve(&levels, Some(50), Target::Level(7), false), Err(Refusal::Invalid), "a level off the scale");
	assert_eq!(resolve(&levels, Some(50), Target::Percent(101), false), Err(Refusal::Invalid));
	assert_eq!(resolve(&levels, None, Target::Steps(1), false), Err(Refusal::Unknown), "a step from a level nobody knows");
}

#[test]
fn firmware_hotkeys_step_cycle_and_zero_to_the_floor_not_below() {
	let levels = list(&[0, 2, 10, 50, 100]);
	assert_eq!(hotkey(&levels, Some(50), Hotkey::Up), Some(100));
	assert_eq!(hotkey(&levels, Some(10), Hotkey::Down), Some(10), "down from the floor stays at it");
	assert_eq!(hotkey(&levels, Some(100), Hotkey::Cycle), Some(10), "cycle from the top goes to the floor");
	assert_eq!(hotkey(&levels, Some(50), Hotkey::Cycle), Some(100));
	assert_eq!(hotkey(&levels, Some(50), Hotkey::Zero), Some(10), "zero is the floor, never black");
	assert_eq!(hotkey(&levels, None, Hotkey::Zero), Some(10));
	assert_eq!(hotkey(&levels, None, Hotkey::Up), None);
}

#[test]
fn one_press_is_one_step_whichever_sources_report_it() {
	let mut last = LastStep::default();
	assert!(!last.duplicate(Source::Key, true, 100));
	assert!(last.duplicate(Source::Hotkey, true, 105), "the firmware's copy of the same press");
	assert!(!last.duplicate(Source::Key, true, 120), "a second press from the key is a press");
	assert!(!last.duplicate(Source::Hotkey, false, 121), "the other direction is never a copy");
	assert!(!last.duplicate(Source::Key, false, 200), "too late to be the same press");
}

const GPU: Function = Function { bus: 0, dev: 1, func: 0 };
const OTHER: Function = Function { bus: 0, dev: 9, func: 0 };
const MONITOR: Monitor = Monitor { manufacturer: 0x10ac, product: 0x4050, serial: 7 };

fn candidate(kind: Kind, target: BacklightTarget, publisher: Function) -> Candidate<'static> {
	Candidate { kind, target, publisher, failed: false, firmware_display_id: None, stable_key: "" }
}

#[test]
fn the_join_reasons_and_their_precedence() {
	let output = Output { source: OutputSource::Provider(GPU), edid: Some(MONITOR) };
	let candidates = [
		candidate(Kind::UsbMonitor, BacklightTarget::Monitor(MONITOR), OTHER),
		candidate(Kind::Firmware, BacklightTarget::Function(GPU), OTHER),
		candidate(Kind::Native, BacklightTarget::None, GPU),
	];
	let places = join(&output, &candidates);
	assert_eq!(places[0], Place { reason: Some(Reason::Edid), standing: Standing::Shadowed(2) });
	assert_eq!(places[1], Place { reason: Some(Reason::FirmwareAdapter), standing: Standing::Shadowed(2) });
	assert_eq!(places[2], Place { reason: Some(Reason::Native), standing: Standing::Active }, "native before firmware before EDID");
}

#[test]
fn the_boot_framebuffers_decoder_joins_the_firmware_backlight_and_none_leaves_it_unjoined() {
	let firmware = [candidate(Kind::Firmware, BacklightTarget::Function(GPU), OTHER)];
	let decoded = Output { source: OutputSource::BootFramebuffer(Some(GPU)), edid: None };
	assert_eq!(join(&decoded, &firmware)[0], Place { reason: Some(Reason::FirmwareAdapter), standing: Standing::Active });
	let undecoded = Output { source: OutputSource::BootFramebuffer(None), edid: None };
	assert_eq!(join(&undecoded, &firmware)[0], Place { reason: None, standing: Standing::Unjoined });
}

#[test]
fn a_lone_usb_backlight_joins_a_single_output_and_two_or_another_join_do_not() {
	let output = Output { source: OutputSource::BootFramebuffer(None), edid: None };
	let one = [candidate(Kind::UsbMonitor, BacklightTarget::None, OTHER)];
	assert_eq!(join(&output, &one)[0], Place { reason: Some(Reason::SingleOutput), standing: Standing::Active });
	let two = [candidate(Kind::UsbMonitor, BacklightTarget::None, OTHER), candidate(Kind::UsbMonitor, BacklightTarget::None, GPU)];
	assert!(join(&output, &two).iter().all(|place| place.standing == Standing::Unjoined), "which of two is not guessed");
	let with_firmware = Output { source: OutputSource::BootFramebuffer(Some(GPU)), edid: None };
	let mixed = [candidate(Kind::UsbMonitor, BacklightTarget::None, OTHER), candidate(Kind::Firmware, BacklightTarget::Function(GPU), OTHER)];
	let places = join(&with_firmware, &mixed);
	assert_eq!(places[0].standing, Standing::Unjoined, "single-output is only for an output nothing else joined");
	assert_eq!(places[1].standing, Standing::Active);
}

#[test]
fn a_failed_provider_is_left_out_and_a_tie_goes_to_the_earlier() {
	let output = Output { source: OutputSource::Provider(GPU), edid: None };
	let mut candidates = [candidate(Kind::Firmware, BacklightTarget::Function(GPU), OTHER), candidate(Kind::Firmware, BacklightTarget::Function(GPU), OTHER)];
	let places = join(&output, &candidates);
	assert_eq!((places[0].standing, places[1].standing), (Standing::Active, Standing::Shadowed(0)));
	candidates[0].failed = true;
	let places = join(&output, &candidates);
	assert_eq!((places[0].standing, places[1].standing), (Standing::Unjoined, Standing::Active));
}

#[test]
fn idle_dimming_takes_a_fraction_never_below_the_floor_or_above_the_level() {
	let range = Scale::range(0, 100).unwrap();
	assert_eq!(dimmed(&range, 80), 24);
	assert_eq!(dimmed(&range, 10), 5, "thirty percent of ten is under the floor");
	assert_eq!(dimmed(&range, 3), 5, "a level under the floor dims to the floor");
	let levels = list(&[0, 10, 20, 40, 100]);
	assert_eq!(dimmed(&levels, 100), 20, "thirty sits between twenty and forty: a tie goes down");
}

#[test]
fn the_curve_interpolates_between_its_points_and_holds_its_ends() {
	let curve = [CurvePoint { percent: 20, lux: 0 }, CurvePoint { percent: 100, lux: 400 }];
	assert_eq!(curve_percent(&curve, 0), 20);
	assert_eq!(curve_percent(&curve, 200_000), 60, "halfway in lux is halfway in percent");
	assert_eq!(curve_percent(&curve, 9_000_000), 100);
	assert_eq!(curve_percent(&[], 0), 10, "no curve is the default one");
	assert_eq!(curve_percent(&[], 10_000_000), 100);
}

#[test]
fn automatic_brightness_moves_only_past_its_hysteresis() {
	assert!(worth_moving(None, 50));
	assert!(!worth_moving(Some(50), 53));
	assert!(worth_moving(Some(50), 55));
}

#[test]
fn the_settings_parse_and_print_and_a_bad_value_is_refused() {
	assert_eq!(parse_automatic("on"), Some(true));
	assert_eq!(parse_automatic(" off\n"), Some(false));
	assert_eq!(parse_automatic("maybe"), None);
	assert_eq!(parse_idle("120"), Some(Some(120)));
	assert_eq!(parse_idle("off"), Some(None));
	assert_eq!(parse_idle("0"), None, "zero seconds is not a timeout");
	assert_eq!(parse_idle("-5"), None);
	assert_eq!(idle_text(Some(60)), "60");
	assert_eq!(idle_text(None), "off");
	assert_eq!(level_key("acpi:\\_SB.PCI0.GFX0.DD1F"), "display.brightness.level.acpi:\\_SB.PCI0.GFX0.DD1F");
	assert_eq!(Settings::default(), Settings { automatic: false, idle_seconds: Some(DEFAULT_IDLE_SECONDS) });
}

fn appearing(standing: Standing) -> Appearing {
	Appearing { standing, scale: list(&[0, 10, 20, 40, 70, 100]), level: Some(100), touched: false, ac_default: Some(100), battery_default: Some(40), stored: None }
}

#[test]
fn a_stored_level_is_restored_snapped_and_raised_to_the_floor_to_an_active_or_unjoined_backlight() {
	let mut backlight = appearing(Standing::Active);
	backlight.stored = Some(37);
	assert_eq!(first_level(&backlight, false), Some(40), "snapped to the nearest level it has now");
	backlight.stored = Some(1);
	assert_eq!(first_level(&backlight, false), Some(10), "raised to the floor");
	backlight.standing = Standing::Unjoined;
	assert_eq!(first_level(&backlight, false), Some(10), "an unjoined backlight is restored too");
	backlight.standing = Standing::Shadowed(0);
	assert_eq!(first_level(&backlight, false), None, "a shadowed one never is");
	backlight.standing = Standing::Active;
	backlight.touched = true;
	assert_eq!(first_level(&backlight, false), None, "nor one anything moved since it appeared");
	let mut same = appearing(Standing::Active);
	same.stored = Some(100);
	assert_eq!(first_level(&same, false), None, "nothing to do where it already is");
}

#[test]
fn without_a_stored_level_the_firmware_default_follows_the_power_source_for_the_active_backlight_alone() {
	let backlight = appearing(Standing::Active);
	assert_eq!(first_level(&backlight, true), Some(40), "the battery default on battery");
	assert_eq!(first_level(&backlight, false), None, "the AC default is where it came up");
	let mut level_low = appearing(Standing::Active);
	level_low.level = Some(20);
	assert_eq!(first_level(&level_low, false), Some(100));
	assert_eq!(first_level(&appearing(Standing::Unjoined), true), None, "no default acts on an unjoined backlight");
	let mut none = appearing(Standing::Active);
	none.ac_default = None;
	none.battery_default = None;
	assert_eq!(first_level(&none, true), None, "without defaults the level it came up with stands");
}

#[test]
fn a_change_settles_before_it_is_stored_and_the_policys_own_never_is() {
	let mut policy = Policy::new(Settings::default());
	policy.changed("usb:a", Some(40), 7, 100, 200);
	policy.changed("usb:a", Some(70), 8, 150, 200);
	assert_eq!(policy.next_due(), Some(350), "the second change restarts the wait");
	assert!(policy.settled(349).is_empty());
	assert_eq!(policy.settled(350), vec![Action::Store { key: "usb:a".into(), level: 70 }]);
	assert_eq!(policy.next_due(), None);
	policy.answered(9);
	policy.changed("usb:a", Some(20), 9, 400, 200);
	assert_eq!(policy.next_due(), None, "the policy's own change is not stored");
	policy.changed("usb:a", Some(10), 10, 500, 200);
	policy.left("usb:a");
	assert_eq!(policy.next_due(), None, "a backlight that left stores nothing");
}

#[test]
fn idle_dims_the_active_backlight_and_active_undoes_it_unless_the_level_was_moved_meanwhile() {
	let scale = Scale::range(0, 100).unwrap();
	let mut policy = Policy::new(Settings::default());
	let action = policy.idle(Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(80) }));
	assert_eq!(action, Some(Action::Set { key: "usb:a".into(), level: 24 }));
	policy.answered(3);
	policy.changed("usb:a", Some(24), 3, 0, 200);
	assert!(policy.dimmed(), "its own dim leaves the dim in force");
	assert_eq!(policy.active(), Some(Action::Set { key: "usb:a".into(), level: 80 }));
	assert_eq!(policy.active(), None, "undone once");
	policy.idle(Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(80) }));
	policy.changed("usb:a", Some(60), 4, 0, 200);
	assert_eq!(policy.active(), None, "a level moved during the dim stays where it was moved");
	let mut off = Policy::new(Settings { automatic: false, idle_seconds: None });
	assert_eq!(off.idle(Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(80) })), None, "no timeout, no dim");
	assert_eq!(policy.idle(Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(5) })), None, "nothing lower than the floor to go to");
	assert_eq!(policy.idle(None), None, "no active backlight");
}

#[test]
fn turning_the_idle_timeout_off_undoes_a_dim_in_force() {
	let scale = Scale::range(0, 100).unwrap();
	let mut policy = Policy::new(Settings::default());
	policy.idle(Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(100) }));
	assert_eq!(policy.set_idle(None), Some(Action::Set { key: "usb:a".into(), level: 100 }));
	assert_eq!(policy.set_idle(Some(60)), None);
}

#[test]
fn automatic_brightness_follows_the_curve_and_a_hand_on_the_level_pauses_it_until_the_next_idle() {
	let scale = Scale::range(0, 100).unwrap();
	let active = || Some(ActiveBacklight { key: "usb:a", scale: &scale, level: Some(50) });
	let curve = [CurvePoint { percent: 20, lux: 0 }, CurvePoint { percent: 100, lux: 400 }];
	let mut policy = Policy::new(Settings { automatic: false, idle_seconds: None });
	assert_eq!(policy.illuminance(active(), 200_000, &curve), None, "off");
	policy.set_automatic(true);
	assert_eq!(policy.illuminance(active(), 200_000, &curve), Some(Action::Percent { key: "usb:a".into(), percent: 60 }));
	assert_eq!(policy.illuminance(active(), 210_000, &curve), None, "inside the hysteresis");
	assert_eq!(policy.illuminance(active(), 400_000, &curve), Some(Action::Percent { key: "usb:a".into(), percent: 100 }));
	policy.changed("usb:a", Some(30), 99, 0, 200);
	assert_eq!(policy.illuminance(active(), 0, &curve), None, "a key or a set pauses it");
	policy.idle(active());
	assert_eq!(policy.illuminance(active(), 0, &curve), Some(Action::Percent { key: "usb:a".into(), percent: 20 }), "the next idle ends the pause");
}

#[test]
fn the_firmware_curve_is_sorted_and_capped_at_the_top_of_the_range() {
	let curve = firmware_curve([(150, 1000), (70, 0), (100, 300), (73, 10), (99, 300)].into_iter());
	assert_eq!(curve, vec![CurvePoint { percent: 70, lux: 0 }, CurvePoint { percent: 73, lux: 10 }, CurvePoint { percent: 100, lux: 300 }, CurvePoint { percent: 100, lux: 1000 }]);
}

#[test]
fn firmware_ties_use_internal_panel_then_namespace_not_publication_order() {
	let function = Function { bus: 0, dev: 1, func: 0 };
	let output = Output { source: OutputSource::BootFramebuffer(Some(function)), edid: None };
	let mut external = candidate(Kind::Firmware, BacklightTarget::Function(function), function);
	external.firmware_display_id = Some(0x100);
	external.stable_key = "acpi:AAA0";
	let mut last_internal = external;
	last_internal.firmware_display_id = Some(0x80010400);
	last_internal.stable_key = "acpi:LCD1";
	let mut first_internal = last_internal;
	first_internal.stable_key = "acpi:LCD0";
	let places = join(&output, &[external, last_internal, first_internal]);
	assert_eq!(places[2].standing, Standing::Active);
	assert_eq!(places[0].standing, Standing::Shadowed(2));
	assert_eq!(places[1].standing, Standing::Shadowed(2));
	let places = join(&output, &[first_internal, external, last_internal]);
	assert_eq!(places[0].standing, Standing::Active);
	first_internal.firmware_display_id = None;
	last_internal.firmware_display_id = None;
	assert_eq!(join(&output, &[last_internal, first_internal])[1].standing, Standing::Active);
}
