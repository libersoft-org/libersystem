#!/usr/bin/env python3
"""One live virtio-sound crash/recovery trial on an existing private development guest.

Uses only the shipping audio controls, recorder/player and existing development kill request.
No guest is started here. Run after building/staging: LIBER_DEV_STATE=... python3 this-file.
The caller owns that guest and must start a fresh one before repeating to reset driver attempts.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def bindings(text):
    decoder = json.JSONDecoder()
    for at, char in enumerate(text):
        if char == "[":
            try:
                value, _ = decoder.raw_decode(text[at:])
            except ValueError:
                continue
            if isinstance(value, list) and value and all(isinstance(row, dict) and "artifact" in row for row in value):
                return value
    raise RuntimeError("lsdev did not return binding records")


def card(rows, name):
    found = [row for row in rows if Path(row["artifact"]).stem == name]
    if len(found) != 1 or str(found[0]["state"]).lower() != "online":
        raise RuntimeError(f"expected exactly one online {name} binding: {found}")
    return found[0]


def audio_id(text, row):
    address = f"{row['bus']:02x}:{row['dev']:02x}.{row['func']}"
    found = re.findall(r"device ([0-9]+): sound device at " + re.escape(address) + r" ", text)
    if len(found) != 1:
        raise RuntimeError(f"audio inventory does not name exactly one device at {address}")
    return int(found[0])


def counters(text):
    value = re.search(r"moves ([0-9]+) silent-frames ([0-9]+) route-overflows ([0-9]+) route-underruns ([0-9]+)", text)
    if not value:
        raise RuntimeError("AudioService counters did not answer")
    return tuple(map(int, value.groups()))


class OwnedLaunch:
    """Use the existing scenario API: every operation closes its protocol session.

    QEMU serves only one development-channel connection at a time. A long-lived
    `dev launch` subprocess would keep the subsequent real driver-kill request
    waiting in that socket's backlog until playback had already ended.
    """
    def __init__(self, guest, output, program='play', arguments='vol://system/p0099-recovery.wav', journal=None):
        self.guest = guest
        self.output = output
        self.program = program
        self.arguments = arguments
        self.journal = journal
        self.koid = None
        self.finished = False
        self.text = ''

    def note(self, operation, **fields):
        if self.journal is not None:
            with self.journal.open('a') as log:
                log.write(json.dumps(dict(operation=operation, monotonic=time.monotonic(), **fields)) + '\n')

    def start(self):
        self.output.write_text('')
        self.note('launch-request', program=self.program, arguments=self.arguments, cwd='vol://system', timeout=10)
        koid = self.guest.launch(self.program, self.arguments, 'vol://system', 10)
        self.note('launch-reply', koid=koid)
        if not isinstance(koid, int) or koid <= 0:
            raise RuntimeError(f'the governed {self.program} launch was refused')
        self.koid = koid

    def poll(self, timeout=5):
        if self.koid is None:
            raise RuntimeError(f'the governed {self.program} was not launched')
        if not self.finished:
            self.note('output-request', koid=self.koid, timeout=timeout)
            text, finished = self.guest.launch_output(timeout)
            self.note('output-reply', koid=self.koid, finished=finished, available=text is not None)
            if text is None:
                raise RuntimeError(f'the governed {self.program} lost its output/completion state')
            self.text += text
            self.output.write_text(self.text)
            self.finished = finished
        return self.finished

    def cleanup(self):
        if self.koid is not None and not self.finished:
            # Closing a host socket/process does not stop the governed guest task.
            # Only this trial uses the private guest's launch slot.
            self.note('stop-request', koid=self.koid, timeout=5)
            stopped = self.guest.stop_launch(5)
            self.note('stop-reply', koid=self.koid, stopped=stopped)
            if not stopped and not self.poll(5):
                raise RuntimeError(f'the owned {self.program} did not finish or accept cancellation')


def complete_playback(text, finished, frames, duration_ms, elapsed):
    metadata = re.findall(r'^play: WAV 48000 Hz, 2 channels, ([0-9]+) frames\r?$', text, re.M)
    if not finished or metadata != [str(frames)]:
        raise RuntimeError('the governed player did not finish the complete WAV after the kill')
    if any(line.startswith('play: ') and not re.fullmatch(
            r'play: WAV 48000 Hz, 2 channels, [0-9]+ frames', line)
            for line in text.splitlines()):
        raise RuntimeError('the governed player reported a playback failure')
    if elapsed < duration_ms / 1000 - 1:
        raise RuntimeError('the player finished before its PCM could be consumed at the negotiated rate')


def self_test():
    rows = [{"artifact": "drivers/virtio_snd.lsexe", "state": "online", "bus": 0, "dev": 7, "func": 0, "generation": 1}]
    assert card(bindings("shell echo\n" + json.dumps(rows)), "virtio_snd") == rows[0]
    assert audio_id("device 3: sound device at 00:07.0 (sound device)", rows[0]) == 3
    assert counters("moves 2 silent-frames 0 route-overflows 0 route-underruns 0") == (2, 0, 0, 0)
    for text in ("[]", "not JSON", '[{"state":"online"}]'):
        try:
            bindings(text)
        except RuntimeError:
            pass
        else:
            raise AssertionError("bad inventory accepted")
    # A minimal scenario peer supplies real completion versus merely printed
    # metadata, and records cancellation of the task this trial actually owns.
    class FakeGuest:
        def __init__(self, accepted=True, lost=False):
            self.calls = []
            self.accepted = accepted
            self.lost = lost
            self.reads = 0

        def launch(self, *args):
            self.calls.append('launch')
            return 41 if self.accepted else None

        def launch_output(self, timeout):
            self.calls.append('output')
            self.reads += 1
            if self.lost:
                return None, False
            return (sample, False) if self.reads == 1 else ('', True)

        def stop_launch(self, timeout):
            self.calls.append('stop')
            return True

    class Output:
        def write_text(self, text):
            self.text = text

    sample = 'play: WAV 48000 Hz, 2 channels, 576000 frames\n'
    peer = FakeGuest()
    playing = OwnedLaunch(peer, Output())
    playing.start()
    assert not playing.poll()  # Metadata alone is not completion.
    peer.calls.append('actual-kernel-kill')  # No launch operation holds a connection here.
    assert playing.poll()
    playing.cleanup()
    assert peer.calls == ['launch', 'output', 'actual-kernel-kill', 'output']
    complete_playback(playing.text, playing.finished, 576000, 12000, 12.2)
    for text, done, elapsed in [(sample, False, 12.2), (sample, True, 2),
                                (sample.replace('576000', '575999'), True, 12.2),
                                (sample + 'play: unsupported or invalid audio\n', True, 12.2)]:
        try:
            complete_playback(text, done, 576000, 12000, elapsed)
        except RuntimeError:
            pass
        else:
            raise AssertionError('incomplete or failed playback accepted')
    for accepted, lost, expected in [(True, False, ['launch', 'output', 'stop']),
                                     (True, True, ['launch', 'output', 'stop']),
                                     (False, False, ['launch'])]:
        peer = FakeGuest(accepted, lost)
        playing = OwnedLaunch(peer, Output())
        try:
            playing.start()
            playing.poll()
        except RuntimeError:
            assert lost or not accepted
        finally:
            playing.cleanup()
        assert peer.calls == expected

    # Exercise the existing LabGuest methods themselves at their socket seam.
    # A peer which permits only one open session would reject the old long-lived
    # launch process plus a second simultaneous kill request.
    import contextlib
    import io
    import lab
    import struct
    from unittest.mock import patch

    channels, operations = [], []
    reads = 0

    class Channel:
        closed = False

        def close(self):
            self.closed = True

    def session(timeout, announce=True):
        assert all(channel.closed for channel in channels), 'overlapping development sessions'
        channel = Channel()
        channels.append(channel)
        return channel, bytearray(), {}

    def request(channel, buffer, request_id, opcode, payload=b'', **kwargs):
        nonlocal reads
        assert not channel.closed
        operations.append(opcode)
        if opcode == lab.OP_LAUNCH:
            return lab.OP_LAUNCH_ACK, 0, struct.pack('<Q', 41)
        if opcode == lab.OP_LAUNCH_OUTPUT:
            reads += 1
            return lab.OP_LAUNCH_BYTES, 0, bytes([reads == 2, 0]) + (sample.encode() if reads == 1 else b'')
        if opcode == lab.OP_KERNEL_CONSOLE:
            assert payload == b'\x04' + struct.pack('<HH', 0x1af4, 0x1059)
            return lab.OP_KERNEL_CONSOLE_ACK, 0, bytes(8)
        assert opcode == lab.OP_LAUNCH_STOP
        return lab.OP_LAUNCH_STOP_ACK, 0, b'\x01'

    with patch.object(lab, 'proto_session', session), patch.object(lab, 'proto_request', request):
        playing = OwnedLaunch(lab.LabGuest(5), Output())
        playing.start()
        assert not playing.poll()
        with contextlib.redirect_stdout(io.StringIO()):
            lab.cmd_dev_kernel_console(['--timeout', '5', 'kill-driver', '1af4:1059'])
        assert playing.poll()
        playing.cleanup()
        recorder = OwnedLaunch(lab.LabGuest(5), Output(), 'audiorec', '-s 12 -f p0099-recovery.wav')
        recorder.start()
        recorder.cleanup()  # The real stop adapter cancels an unfinished owned recorder too.
    assert all(channel.closed for channel in channels)
    assert operations == [lab.OP_LAUNCH, lab.OP_LAUNCH_OUTPUT, lab.OP_KERNEL_CONSOLE,
                          lab.OP_LAUNCH_OUTPUT, lab.OP_LAUNCH, lab.OP_LAUNCH_STOP]
    print('audio-driver-recovery: parser, sequential launch/kill/completion and owned cancellation self-test PASS; no guest ran')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--output", type=Path, default=ROOT / ".build/logs/audio-driver-recovery")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not os.environ.get("LIBER_DEV_STATE"):
        raise RuntimeError("LIBER_DEV_STATE must name the caller's private, already booted development guest")
    if (ROOT / ".build/boot/lab-ctl.sock").exists():
        raise RuntimeError("an ad-hoc lab guest is up; this trial requires the private development guest")
    args.output.mkdir(parents=True, exist_ok=True)
    sequence = 0

    def run(argv, timeout=30):
        nonlocal sequence
        sequence += 1
        log = args.output / f"{sequence:03d}.log"
        try:
            result = subprocess.run(argv, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            partial = error.stdout or ''
            if isinstance(partial, bytes):
                partial = partial.decode(errors='replace')
            log.write_text('COMMAND ' + repr(argv) + '\n' + partial + '\nHOST COMMAND TIMEOUT\n')
            raise
        log.write_text("COMMAND " + repr(argv) + "\n" + result.stdout)
        if result.returncode:
            raise RuntimeError(f"command failed ({result.returncode}): {argv}")
        return result.stdout

    def sh(command, timeout=30):
        return run(["./lab.sh", "sh", "--timeout", str(timeout), command], timeout + 5)

    before = bindings(sh("lsdev --json"))
    virtio, hda = card(before, "virtio_snd"), card(before, "hda")
    devices = sh("audioctl devices")
    original, fallback = audio_id(devices, virtio), audio_id(devices, hda)
    if "done" not in sh(f"audioctl default {original} output"):
        raise RuntimeError("could not select the virtio-sound output")
    import lab
    guest = lab.LabGuest(5)

    def remaining(deadline):
        left = deadline - time.monotonic()
        if left <= 0:
            raise RuntimeError('the governed audio observation deadline expired')
        # lab's shell timeout is in whole seconds. No individual observation
        # consumes another full thirty-second default while a stream is live.
        # Ten seconds leaves room for the broker's five-second quiet nudge if
        # a service line buried a genuinely fresh shell prompt.
        return max(1, min(10, int(left)))

    def preserve_serial():
        serial = Path(os.environ['LIBER_DEV_STATE']) / 'dev-serial.log'
        if serial.exists():
            (args.output / 'serial.log').write_bytes(serial.read_bytes())

    # Wait for the governed recorder's actual completion, independently of a
    # shell prompt which unrelated service output can bury. A failed recording
    # is also cancelled before this script returns to its owning guest runner.
    recorder = OwnedLaunch(guest, args.output / 'record.log', 'audiorec',
                           '-s 12 -f p0099-recovery.wav', args.output / 'record-operations.jsonl')
    try:
        recorder.start()
        deadline = time.monotonic() + 45
        while not recorder.poll(remaining(deadline)):
            time.sleep(0.1)
        recording = re.search(r'audiorec: 48000Hz/2ch/16-bit ([0-9]+)fr duration=([0-9]+)ms', recorder.text)
        if not recording or int(recording[1]) != 576000 or int(recording[2]) != 12000:
            raise RuntimeError('the bounded 12-second WAV recording did not complete')
    finally:
        try:
            recorder.cleanup()
        finally:
            preserve_serial()

    player = OwnedLaunch(guest, args.output / 'play.log', journal=args.output / 'play-operations.jsonl')
    started = time.monotonic()
    try:
        player.start()
        deadline = time.monotonic() + 10
        while True:
            streams = sh('audioctl streams', remaining(deadline))
            if player.poll(remaining(deadline)):
                raise RuntimeError('playback ended before the kill')
            if re.search(rf'stream [0-9]+: .*on device {original}, the default', streams):
                break
            time.sleep(0.05)
        baseline = counters(sh('audioctl counters', remaining(deadline)))
        if player.poll(remaining(deadline)):
            raise RuntimeError('playback ended before the kill')
        run(['./dev.sh', 'kernel-console', '--timeout', '5', 'kill-driver', '1af4:1059'], 10)
        killed = time.monotonic()
        recovered = None
        observed_fallback = False
        deadline = killed + 50
        while not player.poll(remaining(deadline)):
            streams = sh('audioctl streams', remaining(deadline))
            observed_fallback |= bool(re.search(rf'stream [0-9]+: .*on device {fallback}, the default', streams))
            now = bindings(sh('lsdev --json', remaining(deadline)))
            current_hda = card(now, 'hda')
            if current_hda['generation'] != hda['generation']:
                raise RuntimeError("HDA was restarted while carrying the crashed virtio device's stream")
            current = [row for row in now if Path(row['artifact']).stem == 'virtio_snd']
            if current and str(current[0]['state']).lower() == 'online' and current[0]['generation'] > virtio['generation']:
                if recovered is None:
                    recovered = time.monotonic() - killed
            time.sleep(0.1)
        elapsed = time.monotonic() - started
        complete_playback(player.text, player.finished, int(recording[1]), int(recording[2]), elapsed)
        final = bindings(sh('lsdev --json'))
        if card(final, 'hda')['generation'] != hda['generation'] or card(final, 'virtio_snd')['generation'] <= virtio['generation']:
            raise RuntimeError('driver generation evidence does not show one virtio replacement and stable HDA')
        after = counters(sh('audioctl counters'))
        if after[0] < baseline[0] + 2 or after[1:] != baseline[1:]:
            raise RuntimeError(f'routing did not remain continuous: before={baseline}, after={after}')
        final_devices = sh('audioctl devices')
        replacement = audio_id(final_devices, card(final, 'virtio_snd'))
        if replacement == original or audio_id(final_devices, card(final, 'hda')) != fallback:
            raise RuntimeError('provider replacement or unchanged HDA identity was not observed')
        report = dict(result='PASS', trial_count=1, player_koid=player.koid, player_finished=player.finished,
                      player_seconds=elapsed, recovery_observed_seconds=recovered,
                      fallback_observed=observed_fallback, before_counters=baseline, after_counters=after,
                      old_virtio_id=original, new_virtio_id=replacement, hda_id=fallback)
        (args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print('audio-driver-recovery: ' + json.dumps(report))
    finally:
        try:
            player.cleanup()
        finally:
            preserve_serial()


if __name__ == "__main__":
    main()
