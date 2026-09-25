"""What the gadget daemons share: this run's gadget state, and the moment the GUEST has configured the device.

THE GUEST'S CONFIGURATION, NOT THE HOST'S. A gadget on `dummy_hcd` is a device on this host's own bus first, and
this host's USB core configures it the moment it appears; `usb-host` then takes it for the guest, which resets it
and configures it again. Anything a function queues into the host's configuration is lost with that reset, so a
daemon that must speak first waits for the UDC's state to be `configured`, leave it, and come back.
"""

import os
import time


def state_value(name):
    state = os.environ.get("LIBER_GADGET_STATE") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "liber-usb-gadget")
    try:
        with open(os.path.join(state, name)) as handle:
            return handle.read().strip()
    except OSError:
        return None


def udc_state(udc):
    try:
        with open(f"/sys/class/udc/{udc}/state") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def wait_for_guest_configuration(seconds=900):
    udc = state_value("udc")
    if not udc:
        return False
    deadline = time.monotonic() + seconds
    phase = 0
    while time.monotonic() < deadline:
        configured = udc_state(udc) == "configured"
        if phase == 0 and configured:
            phase = 1
        elif phase == 1 and not configured:
            phase = 2
        elif phase == 2 and configured:
            return True
        time.sleep(0.05)
    return False
