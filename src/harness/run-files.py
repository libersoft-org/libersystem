#!/usr/bin/env python3
# A RUN'S FILES LIVE AS LONG AS THE RUN, AND NOT ONE BOOT LONGER.
#
# Every file `qemu-run.sh` makes for one run carries the runner's pid as a dot-separated component of its name - the
# guest's private system disk `virtio-blk.KEY.PID.img`, the scratch disks, the ESP, the firmware store, the control
# socket, a lease `TEMPLATE.lease.PID`, a half-written `X.PID.candidate`. The runner execs QEMU or waits for it, so the
# pid is alive exactly as long as the files are in use. `scratch_sweep` used to be the only cleanup, and it looked at
# ONE prefix, the one the next run happened to use: a copy of a template whose key never came back - every volume
# build makes a new key - was never looked at again. 743 of them, 30 GB, were found in `.build/boot` on 2026-10-16.
#
#   sweep DIR...        every file in DIR whose owner has ended is removed, whatever its prefix - run at the start
#                       of every run; and every development instance's image set whose state directory is gone
#                       (`state-dev-NAME.path` names the directory, `qemu-run.sh` writes it).
#   reap OWNER DIR...   detached by the runner: waits for OWNER to end, then removes OWNER's files in DIR at once -
#                       but the two kinds a gate reads after its run: a guest's console capture (`.out`) and its USB
#                       stick (`usb-media*`), which the next run's sweep removes.
#
# THE OWNER is the name's last or second-to-last dot-separated component, three to seven digits - a pid, never a
# version number or a content key. A pid a live process holds is never taken, even where the process is not the run:
# that keeps a leftover a little longer, which is the safe direction.

import os
import re
import select
import stat
import sys

OWNER = re.compile(r'^[0-9]{3,7}$')
# Names no run owns, whatever digits they carry: a set's lock and a set's marker are named after a state directory,
# whose `mktemp` suffix can be all digits.
UNOWNED = ('.lock', '.path')


def owner_of(name):
    if name.endswith(UNOWNED):
        return None
    parts = name.split('.')
    if len(parts) < 2:
        return None
    for candidate in (parts[-1], parts[-2] if len(parts) >= 3 else None):
        if candidate is not None and OWNER.match(candidate):
            return int(candidate)
    return None


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def removable(path):
    # Regular files and sockets: never a directory, never a symlink's target.
    try:
        mode = os.lstat(path).st_mode
    except FileNotFoundError:
        return False
    return stat.S_ISREG(mode) or stat.S_ISSOCK(mode)


def remove(path):
    try:
        os.unlink(path)
        return True
    except FileNotFoundError:
        return False


def kept_after_run(name):
    # What a gate reads once its run has ended: the console capture, and the USB stick the guest wrote to.
    return name.endswith('.out') or name.startswith('usb-media')


def dev_sets(directory):
    # A development instance's image set, named after its state directory: gone with the directory.
    removed = 0
    for marker in os.listdir(directory):
        if not (marker.startswith('state-dev-') and marker.endswith('.path')):
            continue
        name = marker[len('state-dev-'):-len('.path')]
        try:
            with open(os.path.join(directory, marker)) as handle:
                state = handle.read().strip()
        except OSError:
            continue
        if not state or os.path.isdir(state):
            continue
        tag = f'-dev-{name}'
        members = [entry for entry in os.listdir(directory) if f'{tag}.' in entry or entry.endswith(tag)]
        # A lease a live process holds means a run is still using the set, whatever the directory says.
        if any(owner_of(entry) is not None and '.lease.' in entry and alive(owner_of(entry)) for entry in members):
            continue
        for entry in members:
            path = os.path.join(directory, entry)
            if removable(path):
                removed += remove(path)
        removed += remove(os.path.join(directory, marker))
    return removed


def sweep(directories):
    removed = 0
    for directory in directories:
        if not os.path.isdir(directory):
            continue
        for name in os.listdir(directory):
            owner = owner_of(name)
            if owner is None or alive(owner):
                continue
            path = os.path.join(directory, name)
            if removable(path):
                removed += remove(path)
        removed += dev_sets(directory)
    return removed


def reap(owner, directories):
    # DETACHED FROM EVERYTHING THE RUNNER HELD: a pipe a caller reads to its end, a lock, the medium's descriptor. Each
    # would otherwise stay open as long as this waits.
    os.closerange(3, 65536)
    try:
        handle = os.pidfd_open(owner)
    except ProcessLookupError:
        handle = None
    if handle is not None:
        select.select([handle], [], [])
        os.close(handle)
    for directory in directories:
        if not os.path.isdir(directory):
            continue
        for name in os.listdir(directory):
            if owner_of(name) != owner or kept_after_run(name):
                continue
            path = os.path.join(directory, name)
            if removable(path):
                remove(path)


def main(argv):
    if len(argv) >= 3 and argv[1] == 'sweep':
        sweep(argv[2:])
        return 0
    if len(argv) >= 4 and argv[1] == 'reap' and OWNER.match(argv[2]):
        reap(int(argv[2]), argv[3:])
        return 0
    print('usage: run-files.py sweep DIR... | reap OWNER DIR...', file=sys.stderr)
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv))
