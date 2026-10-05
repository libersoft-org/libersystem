// AudioService entrypoint. The device model and its engine live in audio_engine.rs.
//
// ServiceManager starts this program and hands it, over its bootstrap channel, its roots - the service channel
// clients reach it on ("SERVE"), the launcher's minting root ("ADMIN"), the provider catalogue its sound cards are
// found through ("CATALOGUE"), the idle-latency privilege, the System Graph's observation root ("STATS") and the
// operator's root ("CONTROL"). Bluetooth's audio endpoints are resolved by name through the broker afterwards.
//
// Sound is a capability, not ambient authority: a component reaches audio only through the channel this interface is
// served on. With no device the service still reports in and serves: a stream plays into silence, counted, until a
// device arrives.

#![no_std]
#![no_main]

mod audio_engine;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	audio_engine::run(bootstrap)
}
