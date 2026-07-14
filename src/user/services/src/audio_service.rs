// AudioService entrypoint. The event-driven PCM engine lives in audio_engine.rs.
//
// ServiceManager starts this program and hands it, over its bootstrap channel, the
// virtio-snd driver's control channel ("SND" - a 0 handle when no sound device is
// present, e.g. under test) and the channel its clients reach it on ("SERVE"). Over
// the service channel clients speak the generated Audio and PcmStream bindings.
// Independent mono/stereo source queues are rate-converted and saturating-mixed with
// queued beep tones into 48 kHz stereo periods; driver ACKs pace the bounded queues
// without blocking unrelated client RPC.
//
// Sound is a capability, not ambient authority: a component reaches audio only
// through the channel this interface is served on. With no device the service still
// reports in and serves, answering playback requests with a not-found error.

#![no_std]
#![no_main]

mod audio_engine;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	audio_engine::run(bootstrap)
}
