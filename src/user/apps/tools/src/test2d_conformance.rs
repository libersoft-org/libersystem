// test2d-conformance-sw - `Render2D Core Profile 1`, walked entry by entry inside a booted guest.
//
// WHY THIS IS A PROGRAM AND NOT ONLY A HOST TEST. "The profile is implemented on all three
// architectures" has no executable meaning on the machine that built the image: a host test runs on
// one architecture, once, with the host's own floating point. What the claim is ABOUT is the same
// rasteriser arithmetic running on x86_64, aarch64 and riscv64, and the only way to make it checkable
// is to run the same scenes there. So the suite is a library and this is the few lines around it.
//
// WHAT IT NEEDS, AND IT IS NOTHING. No display, no input, no volume, no font catalogue: every scene
// draws into memory it allocated itself and reads the pixels back. A conformance run that needed a
// service would be a test of the service too, and would fail for reasons that are not about the
// profile.
//
// THE RESULT FORMAT IS ONE LINE PER FEATURE, printed AS IT FINISHES. This runs inside a guest whose
// output is a serial log, so a suite that collected its results and printed them at the end would
// print nothing at all when a scene hung - and which scene it was is the whole of what a reader
// needs. The last line is the verdict a harness greps for.
//
// AND `Unsupported` IS A FAILURE HERE. The profile is a CLOSED list: a backend that refuses a
// Profile 1 drawing is not a backend with a gap, it is a backend that does not conform. The two are
// reported separately because a reader needs to tell "this is wrong" from "this is missing", and the
// run fails for either.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use render2d_conformance::{Summary, Verdict, run};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	print(b"test2d-conformance: start\n");

	let summary: Summary = run(|name, group, verdict| {
		let line = match verdict {
			Verdict::Pass => format!("test2d-conformance: pass {group}/{name}\n"),
			Verdict::Fail(why) => format!("test2d-conformance: FAIL {group}/{name}: {why}\n"),
			Verdict::Unsupported(why) => format!("test2d-conformance: UNSUPPORTED {group}/{name}: {why}\n"),
		};
		print(line.as_bytes());
	});

	// A FEATURE WITH NO SCENE IS A FAILURE OF THE SUITE, reported by name. Without this a feature
	// added to the profile is a feature nobody tests, and every run still says everything passed.
	for name in &summary.untested {
		print(format!("test2d-conformance: UNTESTED {name}\n").as_bytes());
	}
	print(format!("test2d-conformance: {} passed, {} failed, {} unsupported, {} untested\n", summary.passed, summary.failed, summary.unsupported, summary.untested.len()).as_bytes());
	print(if summary.complete() { b"test2d-conformance: conforms\n".as_slice() } else { b"test2d-conformance: DOES NOT CONFORM\n".as_slice() });
	exit()
}
