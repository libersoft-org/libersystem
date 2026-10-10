#!/usr/bin/env bash
# check-qemu-3d-demo.sh - the LIVE half of the 3D demo's proof: frames off a real screen.
#
# WHAT THE GUEST TEST CANNOT SHOW. `kernel.applications.the_3d_demo_renders_a_lit_scene_and_survives_a_resize`
# reads rendered/animated pixels against stand-in services and exercises surface rebuilding. This
# gate additionally reads the live scanout while the real driver and DisplayService resize it.
# What needs a screen is the picture: that there is a lit object standing in front of a horizon, that
# the ground's texture REPEATS across it, that the translucent panel is a mix of what is in front and
# what is behind rather than either alone, that the 2D overlay reached the same frame the 3D scene
# did, and that successive frames differ.
#
# FOUR RUNS, EACH ANSWERING ONE QUESTION. A live animated run for the frames that must differ; a run
# at each STATED POSE for the checks that need to know what is in front of the camera; one native
# surface resized live through two aspect ratios; and the console, which `q` must give back.
#
# CORE NEEDS ITS OWN GUEST because changing another owner's display is not this gate's authority.
# Extended may reuse a checked guest. Both take down only what they started.

SCRIPT_NAME=check-qemu-3d-demo.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

# The Extended phase has its own registered gate. It shares the guest ownership and measurement
# protocol, but its three rows cannot determine whether the Core gate passes.
phase=core
if (($#)); then
	[[ "$#" == 1 && "$1" == --extended ]] || die "usage: check-qemu-3d-demo.sh [--extended]"
	phase=extended
	SCRIPT_NAME=check-qemu-3d-extended.sh
fi

FRAMES_DIR="$(mktemp -d "${TMPDIR:-/tmp}/3d-demo-frames.XXXXXX")"
mkdir -p "$REPO_ROOT/.build/logs"
PROOF_NAME="${SCRIPT_NAME#check-}"
PROOF_DIR="$(mktemp -d "$REPO_ROOT/.build/logs/${PROOF_NAME%.sh}.XXXXXX")"
BOOTED=0
VNC_DIR=""
VNC_CHANGED=0
RESIZE="$REPO_ROOT/src/tools/3d-demo-resize.py"
LAB_STATE="${LIBER_DEV_STATE:-$REPO_ROOT/.build/boot}"
cleanup() {
	local status=$? cleanup_failed=0 may_quit=1 keep_vnc=0
	trap - EXIT
	if ((BOOTED)); then
		if [[ "$phase" == core ]]; then
			if ! python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --owner-state "$LAB_STATE" >"$FRAMES_DIR/owner-before-cleanup.json" 2>"$FRAMES_DIR/owner-before-cleanup.log"; then
				may_quit=0
				cleanup_failed=1
				printf '%s\n' 'the current lab guest no longer matches this invocation; refusing to quit it' >"$FRAMES_DIR/cleanup-error.log"
			fi
		fi
		if ((may_quit)); then
			if ((VNC_CHANGED)); then
				python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --width "$ORIGINAL_WIDTH" --height "$ORIGINAL_HEIGHT" --timeout 10 >"$FRAMES_DIR/resize-cleanup.json" 2>"$FRAMES_DIR/resize-cleanup.log" || cleanup_failed=1
			fi
			"$REPO_ROOT/lab.sh" quit >"$FRAMES_DIR/quit.log" 2>&1 || cleanup_failed=1
		fi
		if [[ "$phase" == core ]] && python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --owner-state "$FRAMES_DIR/owned-state" >"$FRAMES_DIR/owner-after-cleanup.json" 2>"$FRAMES_DIR/owner-after-cleanup.log"; then
			cleanup_failed=1
			keep_vnc=1
			printf '%s\n' 'the proven-owned QEMU remains alive; its private VNC endpoint is retained' >>"$FRAMES_DIR/cleanup-error.log"
		fi
	fi
	if [[ -n "$VNC_DIR" ]] && ((keep_vnc == 0)); then rm -rf "$VNC_DIR"; fi
	cp -a "$FRAMES_DIR/." "$PROOF_DIR/" || cleanup_failed=1
	rm -rf "$FRAMES_DIR"
	note "3D frame and performance proof: $PROOF_DIR"
	if ((status == 0 && cleanup_failed)); then status=1; fi
	exit "$status"
}
trap cleanup EXIT

CHECK="$REPO_ROOT/src/tools/check-3d-demo-frames.py"

# THE PACKAGE AUDIT FIRST, because it needs no guest and answers a different question: what the
# ARTIFACT is. A program carrying its own copy of the rasteriser draws the same picture as one using
# the shared library, and a program holding capabilities it has no business with draws it too.
python3 "$REPO_ROOT/src/tools/check-3d-demo-package.py" || die "the demo's package is not what its manifest says it is"

# This gate's documented reference machine has 32 vCPUs. Scope the override to its owned
# boot so ordinary verification can retain SMP=4. A reused guest is checked as it actually
# exists; changing the caller's environment cannot change a running machine's topology.
# A foreground application or broken broker can hide the prompt while its QEMU remains
# alive. lab boot replaces that recorded group, so establish absence without shell I/O.
refuse_live_guest() {
	local status=0
	python3 "$RESIZE" --live-state "$LAB_STATE" >"$FRAMES_DIR/existing-guest.json" 2>"$FRAMES_DIR/existing-guest.log" || status=$?
	case "$status" in
	0) die "an existing live lab guest was left unchanged; this gate will not replace it" ;;
	1) ;;
	*) die "existing guest ownership could not be established; refusing to boot" ;;
	esac
}
if [[ "$phase" == core ]]; then refuse_live_guest; fi
if ! "$REPO_ROOT/lab.sh" sh uname >/dev/null 2>&1; then
	# Recheck after a failed shell request, including Extended's supported reuse path.
	refuse_live_guest
	note "no instance is up - booting the 32-vCPU reference machine"
	if [[ "$phase" == core ]]; then
		# A short private UNIX path avoids a public VNC listener and AF_UNIX path truncation.
		VNC_DIR="$(mktemp -d /tmp/liber-3d-vnc.XXXXXX)"
		printf '%s\n' "$VNC_DIR/display.sock" >"$FRAMES_DIR/vnc-endpoint.log"
		boot_status=0
		SMP=32 VNC_ADDR="unix:$VNC_DIR/display.sock" "$REPO_ROOT/lab.sh" boot --vnc >/dev/null || boot_status=$?
		if python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --owner-state "$LAB_STATE" >"$FRAMES_DIR/owned-guest.json" 2>"$FRAMES_DIR/owned-guest.log"; then
			BOOTED=1
			mkdir -p "$FRAMES_DIR/owned-state"
			python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pgid"])' "$FRAMES_DIR/owned-guest.json" >"$FRAMES_DIR/owned-state/lab-guest.pgid"
		fi
		((boot_status == 0 && BOOTED == 1)) || die "the guest did not boot with this invocation's proven private VNC ownership"
	else
		SMP=32 "$REPO_ROOT/lab.sh" boot >/dev/null || die "the guest did not boot"
		BOOTED=1
	fi
elif [[ "$phase" == core ]]; then
	die "live resize needs a gate-owned guest and private VNC endpoint; the existing instance was left unchanged"
fi
"$REPO_ROOT/lab.sh" monitor "info cpus" >"$FRAMES_DIR/cpus.log" || die "the guest's CPU topology could not be read"
"$REPO_ROOT/lab.sh" monitor "info kvm" >"$FRAMES_DIR/kvm.log" || die "the guest's accelerator could not be read"
if python3 - "$FRAMES_DIR/cpus.log" "$FRAMES_DIR/kvm.log" <<'TOPOLOGY'; then :; else
import pathlib,re,sys
cpus=pathlib.Path(sys.argv[1]).read_text()
kvm=pathlib.Path(sys.argv[2]).read_text()
ids=[int(value) for value in re.findall(r"^\s*\*?\s*CPU #(\d+):",cpus,re.M)]
assert sorted(ids)==list(range(32)), f"expected exactly 32 live vCPUs, observed CPU IDs {ids}"
assert re.search(r"^kvm support: enabled\s*$",kvm,re.M), "the documented reference guest requires KVM enabled"
TOPOLOGY
	die "this gate requires the documented 32-vCPU KVM guest; a reused instance was left unchanged"
fi

if [[ "$phase" == core ]]; then
	python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --query >"$FRAMES_DIR/resize-original.json" || die "the owned VNC extent could not be read"
	read -r ORIGINAL_WIDTH ORIGINAL_HEIGHT < <(python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); print(v["width"],v["height"])' "$FRAMES_DIR/resize-original.json")
	# Animation/pose fixtures keep their existing small scene. The live-resize run below omits
	# both overrides, so the native surface's new physical extent changes the projection.
	# Performance rows retain their fixed physical/render extents.
	SCENE="--scene-width 320 --scene-height 240"
	VISUAL_RUN=0

	# Run the demo in the background and wait until the screen is its. WAITED FOR RATHER THAN SLEPT
	# THROUGH: a capture taken before the first present shows the CONSOLE, which is not a blank screen -
	# it is white text on black, and a checker that read it would blame the renderer for its own timing.
	#
	# NO FRAME LIMIT, AND THE KEY ENDS IT. A frame count would have to be chosen against a rate nobody
	# measured on the machine the gate happens to run on: too few and the demo has left before the
	# captures, too many and the run outlives the harness's patience. What the captures need is a demo
	# that is still drawing, and what ends it is the same `q` a person presses - which is also the exit
	# the last check reads.
	start_demo() {
		local arguments="$1" scene="${2-$SCENE}"
		VISUAL_RUN=$((VISUAL_RUN + 1))
		DEMO_LOG="$FRAMES_DIR/demo-$VISUAL_RUN.log"
		printf 'test3d-sw %s %s\n' "$scene" "$arguments" >"$DEMO_LOG"
		"$REPO_ROOT/lab.sh" sh --timeout 900 "test3d-sw $scene $arguments" >>"$DEMO_LOG" 2>&1 &
		DEMO_PID=$!
		local ready=0
		for _ in $(seq 1 90); do
			if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/ready.ppm" >/dev/null 2>&1 && python3 "$CHECK" --ready "$FRAMES_DIR/ready.ppm"; then
				ready=1
				break
			fi
			sleep 2
		done
		((ready == 1)) || die "the demo never reached the screen: its output so far was $(tail -n 3 "$DEMO_LOG" 2>/dev/null)"
	}

	finish_demo() {
		"$REPO_ROOT/lab.sh" key q >/dev/null 2>&1 || die "the quit key was not delivered"
		wait "$DEMO_PID" 2>/dev/null || true
		grep -q "test3d-sw: presented" "$DEMO_LOG" || die "the demo did not run to completion; its output was: $(tail -n 3 "$DEMO_LOG")"
	}

	# 1. THE LIVE RUN: three timed frames, and the first thing asked of them is that they DIFFER. One
	#    frame proves a drawing; three seconds apart prove a drawing that is running.
	start_demo "--width 1280 --height 800"
	captured=0
	for index in 1 2 3; do
		if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/frame-$index.ppm" >/dev/null 2>&1; then
			captured=$((captured + 1))
		fi
		sleep 3
	done
	finish_demo
	((captured == 3)) || die "only $captured of three live frames were captured"
	python3 "$CHECK" "$FRAMES_DIR/frame-1.ppm" "$FRAMES_DIR/frame-2.ppm" "$FRAMES_DIR/frame-3.ppm" || die "the live frames do not show the scene the demo draws"

	# 2. THE DETERMINISTIC POSE. A live capture cannot say which face is in front, because which faces a
	#    rotation shows depends on the rotation - so the rotation is STATED, and the checker computes
	#    what that pose must show from the same camera the demo uses.
	for pose in 0 90; do
		start_demo "--pose $pose --width 1280 --height 800"
		"$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/pose-$pose.ppm" >/dev/null 2>&1 || die "the pose $pose frame was not captured"
		finish_demo
		python3 "$CHECK" --pose "$pose" "$FRAMES_DIR/pose-$pose.ppm" || die "the frame at pose $pose is not what that pose puts in front of the camera"
	done

	# 3. ONE LIVE NATIVE SURFACE. VNC's SetDesktopSize reaches virtio-gpu's existing UIInfo
	# event; DisplayService then changes the surface generation. A VNC acknowledgement alone
	# cannot pass: the screenshot must have the new dimensions and the same camera rays/pose.
	start_demo "--pose 0 --report --width 0 --height 0" ""
	cp "$FRAMES_DIR/ready.ppm" "$FRAMES_DIR/native-original.ppm"
	expected_rebuilds=0
	resize_capture() {
		local width="$1" height="$2" name="$3" ready=0 changed
		VNC_CHANGED=1
		python3 "$RESIZE" --socket "$VNC_DIR/display.sock" --width "$width" --height "$height" --timeout 30 >"$FRAMES_DIR/resize-$name.json" 2>"$FRAMES_DIR/resize-$name.log" || die "the $name resize was refused or did not complete"
		changed="$(python3 -c 'import json,sys; print(int(json.load(open(sys.argv[1]))["changed"]))' "$FRAMES_DIR/resize-$name.json")"
		expected_rebuilds=$((expected_rebuilds + changed))
		for _ in $(seq 1 90); do
			if "$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/$name.ppm" >/dev/null 2>&1 && python3 "$CHECK" --resize "$width" "$height" "$FRAMES_DIR/native-original.ppm" "$FRAMES_DIR/$name.ppm" >"$FRAMES_DIR/resize-$name-pixels.log" 2>&1; then
				ready=1
				break
			fi
			sleep 2
		done
		((ready == 1)) || die "the native scene never reached the $name extent/projection; see resize-$name-pixels.log"
		cat "$FRAMES_DIR/resize-$name-pixels.log"
	}
	resize_capture 1024 768 desktop
	resize_capture 480 800 mobile
	# Restore the reference scanout before the fixed performance workload below.
	resize_capture "$ORIGINAL_WIDTH" "$ORIGINAL_HEIGHT" restored
	VNC_CHANGED=0
	finish_demo
	python3 - "$DEMO_LOG" "$expected_rebuilds" "$ORIGINAL_WIDTH" "$ORIGINAL_HEIGHT" <<'RESIZED'
import pathlib,re,sys
log=pathlib.Path(sys.argv[1]).read_text()
expected,width,height=map(int,sys.argv[2:])
match=re.search(r"presented \d+ frame\(s\) rebuilt=(\d+)",log)
assert expected>=2 and match and int(match[1])>=expected, "the same live demo must rebuild for the requested aspect changes"
assert f"scene {width}x{height}" in log, "the scene's final render extent must follow the restored native surface"
RESIZED

	# 4. AND `q` GAVE THE SCREEN BACK. An application that took the console and left it holding a
	#    rendered frame is a machine a person sees as dead. Every run above ended on that key; this reads
	#    what the screen holds now that the last one has.
	for _ in $(seq 1 20); do
		sleep 1
		"$REPO_ROOT/lab.sh" shot "$FRAMES_DIR/after.ppm" >/dev/null 2>&1 || continue
		if python3 "$CHECK" --console "$FRAMES_DIR/after.ppm" >/dev/null 2>&1; then
			break
		fi
	done
	python3 "$CHECK" --console "$FRAMES_DIR/after.ppm" || die "the console was not restored after the demo left"

	note "live frames prove animation, a bounded central object, a repeating ground texture, a blended panel, the 2D overlay in the same frame, two stated poses, live desktop/portrait resize preserving camera projection and a restored console"
fi

# Full-size release measurements. The first five presents warm the frame path; thirty subsequent
# intervals include pacing, acquire, rendering, HUD, present and worker-thread allocation activity.
{
	date -u +%Y-%m-%dT%H:%M:%SZ
	qemu-system-x86_64 --version | head -n 1
	LC_ALL=C lscpu
	cat "$FRAMES_DIR/kvm.log" "$FRAMES_DIR/cpus.log"
	python3 - "$REPO_ROOT" <<'ENVIRONMENT'
import hashlib,json,os,pathlib,re,shlex,sys
root=pathlib.Path(sys.argv[1])
state=pathlib.Path(os.environ.get("LIBER_DEV_STATE") or root/".build/boot")
try:
    pgid=int((state/"lab-guest.pgid").read_text().strip())
except (OSError,ValueError):
    pgid=int(json.loads((state/"dev-instance.lock").read_text())["pgid"])
found=False
for process in pathlib.Path("/proc").iterdir():
    if not process.name.isdecimal(): continue
    try:
        if os.getpgid(int(process.name))!=pgid: continue
        args=[arg.decode() for arg in (process/"cmdline").read_bytes().split(b"\0") if arg]
    except (OSError,UnicodeError):
        continue
    if args and pathlib.Path(args[0]).name.startswith("qemu-system-"):
        print("guest-command="+shlex.join(args))
        found=True
assert found, "cannot identify the live lab guest's QEMU command"
artifact=root/".build/image/x86_64-unknown-none/bin/test3d-sw"
data=artifact.read_bytes()
print("staged-demo-sha256="+hashlib.sha256(data).hexdigest())
for key in ("target","profile","rustc-commit","rustflags","features"):
    value=re.search(rb"(?:^|\n)"+key.encode()+rb"=([^\n\0]+)",data)
    assert value, "missing built artifact identity field: "+key
    print(key+"="+value.group(1).decode())
print("frame-clock=rt::clock_ns / SYS_CLOCK_MONO_NS (guest calibrated monotonic nanoseconds)")
print("measurement=5 warmup presents, 30 complete frame intervals; animated scene; fixed physical/render extent")
ENVIRONMENT
} >"$FRAMES_DIR/environment.log" 2>&1
failed=0
repeat=0
row=0
for size in "320 240" "800 600" "640 480" "640 480" "640 480"; do
	read -r width height <<<"$size"
	row=$((row + 1))
	if [[ "$width" == 640 ]]; then repeat=$((repeat + 1)); fi
	[[ "$phase" == extended && "$width" == 640 && "$repeat" -gt 1 ]] && continue
	extra=""
	[[ "$phase" == extended ]] && extra="--postprocess"
	log="$FRAMES_DIR/perf-$phase-${width}x${height}-$row.log"
	"$REPO_ROOT/lab.sh" sh --timeout 900 "test3d-sw --no-input --report --frames 35 --width $width --height $height --scene-width $width --scene-height $height $extra" >"$log" 2>&1 || failed=1
	cat "$log"
	if python3 - "$log" "$phase" "$width" "$height" <<'PERF'; then :; else failed=1; fi
import pathlib,re,sys
path,phase,width,height=sys.argv[1:]
text=pathlib.Path(path).read_text()
assert "test3d-sw: presented 35 frame(s)" in text, "all requested frames must finish"
assert "allocation counter observes shared-library preparation" in text, "the actual allocator counter must observe library allocations"
match=re.search(r"steady samples (\d+) median_us (\d+) p99_us (\d+) render_alloc_max (\d+) loop_alloc_max (\d+)",text)
assert match, "missing steady-state timing/allocation report"
samples,median,p99,render_alloc,loop_alloc=map(int,match.groups())
assert samples==30 and render_alloc==0 and loop_alloc==0, "steady-state frames must allocate nothing"
assert "test3d-sw: colour " in text and "test3d-sw: present " in text and "heap live_bytes " in text and "scene prepared bytes " in text and "present_max_us " in text, "memory and present stages must be reported"
if phase=="core" and width=="640" and height=="480":
    elapsed=re.search(r"steady elapsed_ns (\d+) max_us (\d+)",text)
    assert elapsed, "missing complete frame-window elapsed time"
    elapsed_ns,maximum=map(int,elapsed.groups())
    assert median<=33333 and elapsed_ns<=samples*1_000_000_000//30, f"stable 30 FPS floor missed: median={median}us, window={elapsed_ns}ns, p99/max={p99}/{maximum}us"
if phase=="extended":
    assert "HDR chain executed six downsamples, five upsamples and resolve" in text
    assert "test3d-sw: postprocess " in text and "test3d-sw: shadow " in text and "HDR prepared bytes " in text
PERF
done
((failed == 0)) || die "3D performance/allocation criteria failed; proof retained in $PROOF_DIR"
if [[ "$phase" == core ]]; then
	note "all three Core sizes measured, 640x480 stable 30 FPS across three windows and steady-state zero allocations passed"
else
	note "all three Extended sizes measured with shadow/HDR stages and steady-state zero allocations; no Core frame-rate floor applies"
fi
