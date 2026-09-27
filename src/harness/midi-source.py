#!/usr/bin/env python3
"""The far end of the MIDI gadget: an instrument, played by a process.

The USB MIDI oracle checks that what a device sends on each cable arrives on that cable, in order, as the
bytes it was - so this process plays a known phrase on each of the gadget card's two raw MIDI ports. The
function turns each port's bytes into USB-MIDI event packets on the cable of the same number and holds them
until the guest's transport reads them.

IT WAITS FOR THE CONFIGURATION FIRST. `f_midi` drops what is written while the host has not configured the
device - its IN endpoint is not enabled yet - so the phrase is played only once the controller the gadget is
bound to says `configured`. Then it stays, holding the ports open: closing a raw MIDI output drains it, and a
drain waits on a host that reads only when the oracle starts receiving.

WITH `--echo` IT PLAYS NOTHING OF ITS OWN and plays BACK: whatever arrives on a port - what the guest sent on
that cable - is written out on the same port, so it comes back to the guest on the cable it left on. That is
the transmit oracle's far end: the guest can compare what it sent with what it hears without this process
telling it anything.
"""

import argparse
import glob
import os
import select
import sys
import time

from gadget_state import wait_for_guest_configuration

CARD_ID = "LiberMidi"
# THE ORACLE'S PHRASES - `usb_midi_*` in the kernel's hardware suite expects exactly these, per cable: note on
# and off, a system exclusive message long enough to span packets, a program change; and on the second cable
# a note on another channel, a timing clock between messages, and a control change.
PHRASES = (
    bytes([0x90, 0x3C, 0x64, 0x80, 0x3C, 0x00, 0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7, 0xC0, 0x05]),
    bytes([0x91, 0x40, 0x7F, 0xF8, 0xB1, 0x07, 0x64]),
)


def say(message):
    print(f"midi-source: {message}", file=sys.stderr, flush=True)


def card_device():
    for path in glob.glob("/sys/class/sound/card*/id"):
        try:
            with open(path) as handle:
                if handle.read().strip() == CARD_ID:
                    card = os.path.basename(os.path.dirname(path))[len("card"):]
                    return f"/dev/snd/midiC{card}D0"
        except OSError:
            continue
    return None


def echo(ports):
    say("echoing: whatever arrives on a port goes back out on it")
    while True:
        ready, _, _ = select.select(ports, [], [], 60)
        for fd in ready:
            data = os.read(fd, 256)
            if data:
                os.write(fd, data)
                say(f"cable {ports.index(fd)}: echoed {data.hex(' ')}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--echo", action="store_true", help="play back what arrives instead of playing the phrases")
    echoing = parser.parse_args().echo
    deadline = time.monotonic() + 900
    # THE GUEST'S CONFIGURATION, NOT THE HOST'S - see `gadget_state`.
    if not wait_for_guest_configuration():
        say("the guest never configured the device")
        return 1
    device = None
    while time.monotonic() < deadline:
        device = card_device()
        if device and os.path.exists(device):
            break
        time.sleep(0.1)
    else:
        say("the gadget's card never appeared")
        return 1
    time.sleep(0.5)
    # ONE OPEN PER PORT: a raw MIDI device hands each open the lowest free substream, so the first is port 0
    # and the second port 1 - cable 0 and cable 1. Opened for both directions, the input and the output of one
    # open are the same port.
    if echoing:
        echo([os.open(device, os.O_RDWR), os.open(device, os.O_RDWR)])
    ports = [os.open(device, os.O_WRONLY), os.open(device, os.O_WRONLY)]
    for cable, (fd, phrase) in enumerate(zip(ports, PHRASES)):
        os.write(fd, phrase)
        say(f"cable {cable}: played {phrase.hex(' ')}")
    while True:
        time.sleep(60)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as error:
        say(f"stopped: {error}")
        sys.exit(1)
