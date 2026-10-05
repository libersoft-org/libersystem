use super::*;
use alloc::vec;
use alloc::vec::Vec;

#[test]
fn what_this_host_declares_follows_the_watcher() {
	assert_eq!(declared(Radio::Classic, true), Declared::Classic(IoCapability::DisplayYesNo));
	assert_eq!(declared(Radio::Classic, false), Declared::Classic(IoCapability::NoInputNoOutput));
	assert_eq!(declared(Radio::Le, true), Declared::Le(LE_KEYBOARD_DISPLAY));
	assert_eq!(declared(Radio::Le, false), Declared::Le(LE_NO_INPUT_NO_OUTPUT));
	assert!(pairable(true) && !pairable(false));
}

#[test]
fn a_bond_is_never_downgraded_on_either_axis() {
	let sc_auth = Level::of_link_key(KeyType::AuthenticatedP256).unwrap();
	let sc_just = Level::of_link_key(KeyType::UnauthenticatedP256).unwrap();
	let p192_auth = Level::of_link_key(KeyType::AuthenticatedP192).unwrap();
	let legacy = Level::of_link_key(KeyType::Combination).unwrap();
	assert!(sc_auth.may_replace(&sc_auth));
	assert!(!p192_auth.may_replace(&sc_auth), "a P-256 bond offered P-192");
	assert!(!sc_just.may_replace(&sc_auth), "an authenticated bond offered Just Works");
	assert!(!legacy.may_replace(&p192_auth), "a Secure Simple Pairing bond offered legacy");
	assert!(sc_auth.may_replace(&p192_auth) && sc_auth.may_replace(&legacy), "up is always allowed");
	assert_eq!(Level::of_link_key(KeyType::DebugCombination), None, "a debug key is no key");
	assert!(sc_just.derives_across() && !p192_auth.derives_across(), "only Secure Connections crosses transports");
}

#[test]
fn legacy_pairing_only_when_asked_and_never_over_a_better_bond() {
	assert!(!legacy_allowed(false, None));
	assert!(legacy_allowed(true, None));
	let held = Level { agreement: Agreement::P192, authenticated: false };
	assert!(!legacy_allowed(true, Some(&held)));
}

#[test]
fn a_confirmation_is_numeric_comparison_only_when_both_can_show_and_confirm() {
	assert_eq!(classic_confirmation(IoCapability::DisplayYesNo, IoCapability::DisplayYesNo, 1_234_567, true), Some(Question::Compare(234_567)));
	assert_eq!(classic_confirmation(IoCapability::DisplayYesNo, IoCapability::NoInputNoOutput, 5, true), None, "an outgoing Just Works this host started");
	assert_eq!(classic_confirmation(IoCapability::DisplayYesNo, IoCapability::KeyboardOnly, 5, false), Some(Question::Consent), "an incoming one is consented");
}

#[test]
fn a_prompt_expires_at_twenty_five_seconds_and_takes_only_answers_that_fit() {
	let prompt = Prompt::new(Question::EnterPasskey, 1_000);
	assert!(!prompt.expired(25_999) && prompt.expired(26_000));
	assert!(prompt.fits(&Reply::Passkey(123_456)) && !prompt.fits(&Reply::Passkey(1_000_000)));
	assert!(!prompt.fits(&Reply::Yes), "a passkey question is not answered yes");
	assert!(prompt.fits(&Reply::No), "no answers anything");
	let pin = Prompt::new(Question::EnterPin, 0);
	assert!(pin.fits(&Reply::Pin(vec![b'0'; 4])) && !pin.fits(&Reply::Pin(Vec::new())));
	assert!(Prompt::new(Question::Compare(7), 0).fits(&Reply::Yes));
}
