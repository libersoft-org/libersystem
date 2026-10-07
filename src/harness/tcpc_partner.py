"""A TYPE-C PORT CONTROLLER AND THE POWER DELIVERY SOURCE AT THE OTHER END OF ITS CABLE, for `vhost-i2c-gpio.py --tcpc`.

THE CONTROLLER (`Tcpc`) is a TCPCI revision 2.0 register file at an I2C address, its ALERT# a GPIO line, level and
active low, and the PHY below it: the GoodCRC and retries a port controller does on the wire are its own, so a message
the guest writes to TRANSMIT is acknowledged here or not at all, and a message the partner sends is taken into the
receive buffer - and GoodCRC'd - only while RECEIVE_DETECT enables SOP. It measures VBUS and raises the high and low
alarms against the thresholds the guest sets.

THE PARTNER (`Source`) is a USB Power Delivery SOURCE written from the specification's source-side policy engine and
message tables, and shares no code with the sink engine the guest runs, so the two ends of every exchange are
different machines: Source_Capabilities after VBUS is on and again every SourceCapabilityTimer without a GoodCRC,
SenderResponseTimer for the Request (and a hard reset when it runs out), Accept, tSrcTransition, the supply moved,
PS_RDY; Soft_Reset answered; a hard reset sent or received taking VBUS to vSafe0V after tPSHardReset and back after
tSrcRecover. Its profiles and one-shot scripts are what the TCPCI gate drives through the control socket.

WHAT IT RECORDS - every request with what it named, the time from each Source_Capabilities' alert to the TRANSMIT of
the Request that answers it, the times a sink's hard reset came after what started its timer, the sink path's changes -
goes to the log, and what breaks a rule the sink must keep is a VIOLATION the gate fails on: a request for a position
the latest capabilities do not have, for an offer that is not fixed or for more current than it offers; the sink path
enabled without VBUS; a hard reset sent with the sink path on; the sink path, or the automatic discharge, still on when
a hard reset takes VBUS down; a message sent while the supply moves.

STRETCH. On the emulated ports the source's timers that wait on the sink - SenderResponseTimer and tSrcTransition -
are multiplied by the factor the gate passes, because an emulated guest cannot answer inside them; every other timer
and the protocol are unchanged.
"""

import struct
import time

# ------------------------------------------------------------------ the controller's registers

VENDOR_ID = 0x00
PRODUCT_ID = 0x02
BCD_DEVICE = 0x04
TC_REVISION = 0x06
PD_REVISION = 0x08
PD_INTERFACE_REVISION = 0x0A
ALERT = 0x10
ALERT_MASK = 0x12
POWER_STATUS_MASK = 0x14
FAULT_STATUS_MASK = 0x15
TCPC_CONTROL = 0x19
ROLE_CONTROL = 0x1A
FAULT_CONTROL = 0x1B
POWER_CONTROL = 0x1C
CC_STATUS = 0x1D
POWER_STATUS = 0x1E
FAULT_STATUS = 0x1F
COMMAND = 0x23
DEVICE_CAPABILITIES_1 = 0x24
DEVICE_CAPABILITIES_2 = 0x26
MESSAGE_HEADER_INFO = 0x2E
RECEIVE_DETECT = 0x2F
RECEIVE_BUFFER = 0x30
TRANSMIT = 0x50
TRANSMIT_BUFFER = 0x51
VBUS_VOLTAGE = 0x70
VBUS_VOLTAGE_ALARM_HI = 0x76
VBUS_VOLTAGE_ALARM_LO = 0x78

ALERT_CC_STATUS = 1 << 0
ALERT_POWER_STATUS = 1 << 1
ALERT_RX_STATUS = 1 << 2
ALERT_RX_HARD_RESET = 1 << 3
ALERT_TX_FAILED = 1 << 4
ALERT_TX_DISCARDED = 1 << 5
ALERT_TX_SUCCESS = 1 << 6
ALERT_VBUS_ALARM_HI = 1 << 7
ALERT_VBUS_ALARM_LO = 1 << 8

POWER_SINKING_VBUS = 1 << 0
POWER_VBUS_PRESENT = 1 << 2
POWER_VBUS_DETECTION = 1 << 3
POWER_UNINITIALIZED = 1 << 6

CONTROL_AUTO_DISCHARGE = 1 << 4
CONTROL_DISABLE_ALARMS = 1 << 5
CONTROL_DISABLE_MONITORING = 1 << 6

COMMAND_DISABLE_VBUS_DETECT = 0x22
COMMAND_ENABLE_VBUS_DETECT = 0x33
COMMAND_DISABLE_SINK_VBUS = 0x44
COMMAND_SINK_VBUS = 0x55

DETECT_SOP = 1 << 0
DETECT_HARD_RESET = 1 << 5

# DEVICE_CAPABILITIES_1: a sink path the controller switches, and VBUS measured with alarms.
CAPABILITIES_1 = 1 << 2 | 1 << 10

ROLE_RD = 0b10
ROLE_OPEN = 0b11

# VBUS_VOLTAGE's present threshold, and the most the receive path holds before it stops acknowledging.
VBUS_PRESENT_MV = 4000
RECEIVE_DEPTH = 3

# ------------------------------------------------------------------ Power Delivery, the source's side

REVISION_2 = 1
REVISION_3 = 2

# Control messages.
GOODCRC, ACCEPT, REJECT, PING, PS_RDY, GET_SOURCE_CAP, GET_SINK_CAP, DR_SWAP, PR_SWAP, VCONN_SWAP, WAIT, SOFT_RESET = 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13
NOT_SUPPORTED = 16
# Data messages.
SOURCE_CAPABILITIES, REQUEST, SINK_CAPABILITIES, VENDOR_DEFINED = 1, 2, 4, 15

CONTROL_NAMES = {1: 'GoodCRC', 3: 'Accept', 4: 'Reject', 5: 'Ping', 6: 'PS_RDY', 7: 'Get_Source_Cap', 8: 'Get_Sink_Cap', 9: 'DR_Swap', 10: 'PR_Swap', 11: 'VCONN_Swap', 12: 'Wait', 13: 'Soft_Reset', 16: 'Not_Supported'}
DATA_NAMES = {1: 'Source_Capabilities', 2: 'Request', 3: 'BIST', 4: 'Sink_Capabilities', 15: 'Vendor_Defined'}

# THE SOURCE'S TIMERS, in seconds, each inside the specification's bounds.
T_VBUS_ON = 0.150  # the source's tCCDebounce before VBUS goes on
T_FIRST_SOURCE_CAP = 0.100  # tFirstSourceCap: at most 250 ms
T_SOURCE_CAPABILITY = 0.150  # SourceCapabilityTimer: 100 to 200 ms
N_CAPS_COUNT = 50
T_SENDER_RESPONSE = 0.027  # SenderResponseTimer: 24 to 30 ms - STRETCHED on the emulated ports
T_RECEIVER_RESPONSE = 0.003  # tReceiverResponse: at most 15 ms
T_SRC_TRANSITION = 0.030  # tSrcTransition: 25 to 35 ms - STRETCHED on the emulated ports
T_SUPPLY_SETTLE = 0.020  # the supply's move, inside tPSTransition's 450 ms
# THE STRETCHED tSrcTransition'S CEILING. The sink's PSTransitionTimer - 450 to 550 ms from the Accept to PS_RDY - is a
# sink timer and is not stretched, so a stretched wait before the transition has to leave the move and PS_RDY inside it:
# at the emulated ports' factor of 100 the 30 ms became 3 s, and every sink sent Hard Reset some 500 ms after the
# Accept, before the source had begun (aarch64, 2026-10-07). 300 ms is ten times the specification's and leaves the rest.
T_SRC_TRANSITION_CEILING = 0.300
T_PS_HARD_RESET = 0.030  # tPSHardReset: 25 to 35 ms
T_SRC_RECOVER = 0.700  # tSrcRecover: 660 to 1000 ms


def fixed(millivolts, milliamps, usb_communications=False):
    return (1 << 26 if usb_communications else 0) | (millivolts // 50) << 10 | milliamps // 10


def pps(minimum_mv, maximum_mv, milliamps):
    return 3 << 30 | (maximum_mv // 100) << 17 | (minimum_mv // 100) << 8 | milliamps // 50


def header(kind, count, revision, message_id, extended=False):
    """A source's header: power role source (bit 8), data role DFP (bit 5)."""
    return kind & 0x1F | 1 << 5 | (revision & 3) << 6 | 1 << 8 | (message_id & 7) << 9 | (count & 7) << 12 | (1 << 15 if extended else 0)


def message(kind, revision, message_id, objects=()):
    return struct.pack('<H', header(kind, len(objects), revision, message_id)) + b''.join(struct.pack('<I', o) for o in objects)


def parse(data):
    """(type, data?, count, id, revision, objects) of a message the sink sent, or None when it is not one."""
    if len(data) < 2:
        return None
    word = struct.unpack_from('<H', data)[0]
    count = (word >> 12) & 7
    if len(data) != 2 + 4 * count:
        return None
    objects = list(struct.unpack_from(f'<{count}I', data, 2))
    return word & 0x1F, count > 0, count, (word >> 9) & 7, (word >> 6) & 3, objects


def name_of(kind, is_data):
    return (DATA_NAMES if is_data else CONTROL_NAMES).get(kind, f'{"data" if is_data else "control"} {kind}')


# THE PROFILES: whether it speaks Power Delivery, its revision, its Rp, its offers.
PROFILES = {
    'charger31': dict(pd=True, revision=REVISION_3, rp=3, caps=[fixed(5000, 3000), fixed(9000, 3000), fixed(12000, 3000), fixed(15000, 3000), fixed(20000, 3000), pps(3300, 21000, 3000)]),
    'charger20': dict(pd=True, revision=REVISION_2, rp=3, caps=[fixed(5000, 3000), fixed(9000, 3000), fixed(15000, 3000)]),
    'weak': dict(pd=True, revision=REVISION_3, rp=2, caps=[fixed(5000, 2000), fixed(9000, 1500), fixed(12000, 1000)]),
    'typec15': dict(pd=False, revision=REVISION_3, rp=2, caps=[]),
    'typec30': dict(pd=False, revision=REVISION_3, rp=3, caps=[]),
}
# `caps less`: the 15 V offer and above withdrawn.
LESS = [fixed(5000, 3000), fixed(9000, 3000)]
# The scripts `script NAME` arms for the next step they apply to.
SCRIPTS = ('wait', 'reject', 'no-answer', 'no-ps-rdy', 'overshoot', 'vbus-stays-off', 'detach-mid', 'no-caps')


class Scheduler:
    """Named one-shot timers on the monotonic clock: the program's loop asks when the next is due and runs what is."""

    def __init__(self, clock=time.monotonic):
        self.clock = clock
        self.timers = []
        self.sequence = 0

    def at(self, delay, name, action):
        self.sequence += 1
        self.timers.append((self.clock() + delay, self.sequence, name, action))

    def cancel(self, *names):
        self.timers = [timer for timer in self.timers if timer[2] not in names]

    def cancel_all(self):
        self.timers = []

    def armed(self, name):
        return any(timer[2] == name for timer in self.timers)

    def next_due(self):
        return min((timer[0] for timer in self.timers), default=None)

    def run_due(self):
        while True:
            now = self.clock()
            due = sorted(timer for timer in self.timers if timer[0] <= now)
            if not due:
                return
            first = due[0]
            self.timers.remove(first)
            first[3]()


class Tcpc:
    """THE PORT CONTROLLER at `address`, its alert on GPIO line `line`: registers read and written over I2C, the partner
    on its CC and VBUS."""

    def __init__(self, address, line, set_line, log, clock=time.monotonic):
        self.address = address
        self.line = line
        self.set_line = set_line
        self.log = log
        self.clock = clock
        self.alert = 0
        self.alert_mask = 0
        self.power_status_mask = 0
        self.tcpc_control = 0
        # AS A DEAD-BATTERY SINK POWERS UP: Rd on both lines.
        self.role_control = ROLE_RD << 2 | ROLE_RD
        self.power_control = CONTROL_DISABLE_ALARMS | CONTROL_DISABLE_MONITORING
        self.fault_status = 0
        self.header_info = 0
        self.receive_detect = 0
        self.vbus_detect = False
        self.sinking = False
        self.alarm_hi = 0
        self.alarm_lo = 0
        self.alarm_side = None
        self.receive = []
        self.buffer = None
        self.transmit_buffer = b''
        self.pointer = 0
        # Register transfers the sink made, counted so a response can be told in them.
        self.transfers = 0
        # Its initialisation, reported for the first reads of POWER_STATUS.
        self.initialising = 2
        self.partner = None
        self.others = {}
        self.cc_status = self.compute_cc()
        self.power_status = self.compute_power()

    # ---------------------------------------------------------------- what the partner and the registers make

    def sink_presented(self):
        """Rd on the partner's CC line: the sink is there as the partner sees it."""
        cc1, cc2 = self.role_control & 3, (self.role_control >> 2) & 3
        return ROLE_RD in (cc1, cc2)

    def compute_cc(self):
        partner = self.partner
        level = partner.rp if partner and partner.cc_attached and self.sink_presented() else 0
        if not level:
            return 1 << 4  # ConnectResult: presenting Rd, both lines open
        return 1 << 4 | ((level << 2) if partner.flipped else level)

    def vbus_mv(self):
        return self.partner.vbus_mv if self.partner else 0

    def compute_power(self):
        present = self.vbus_detect and self.vbus_mv() >= VBUS_PRESENT_MV
        return (POWER_SINKING_VBUS if self.sinking else 0) | (POWER_VBUS_PRESENT if present else 0) | (POWER_VBUS_DETECTION if self.vbus_detect else 0)

    def measures(self):
        return not self.power_control & CONTROL_DISABLE_MONITORING

    def signal(self):
        self.set_line(self.line, 0 if self.alert & self.alert_mask else 1)

    def raise_alert(self, bits):
        self.alert |= bits
        self.signal()

    def changed(self):
        """CC or VBUS moved: the status registers recomputed, their alerts raised, the alarms checked."""
        cc = self.compute_cc()
        if cc != self.cc_status:
            self.cc_status = cc
            self.raise_alert(ALERT_CC_STATUS)
        power = self.compute_power()
        if (power ^ self.power_status) & self.power_status_mask:
            self.raise_alert(ALERT_POWER_STATUS)
        self.power_status = power
        self.check_alarms()

    def check_alarms(self):
        """AN ALARM IS RAISED AS VBUS CROSSES A THRESHOLD, once - not again on every change while it stays past it."""
        if not self.measures() or self.power_control & CONTROL_DISABLE_ALARMS:
            self.alarm_side = None
            return
        millivolts = self.vbus_mv()
        side = 'high' if self.alarm_hi and millivolts > self.alarm_hi * 25 else 'low' if millivolts < self.alarm_lo * 25 else None
        if side is None or side == self.alarm_side:
            self.alarm_side = side
            return
        self.alarm_side = side
        threshold = (self.alarm_hi if side == 'high' else self.alarm_lo) * 25
        self.log(f'alarm {side} at {millivolts} mV (threshold {threshold} mV)')
        self.partner.alarmed()
        self.raise_alert(ALERT_VBUS_ALARM_HI if side == 'high' else ALERT_VBUS_ALARM_LO)

    # ---------------------------------------------------------------- the receive path

    def deliver(self, data):
        """A message the partner sent: GoodCRC'd and queued when SOP is received and the path has room."""
        if not self.receive_detect & DETECT_SOP or len(self.receive) >= RECEIVE_DEPTH:
            return False
        self.receive.append(bytes(data))
        self.load()
        return True

    def load(self):
        if self.buffer is None and self.receive:
            self.buffer = self.receive.pop(0)
            self.raise_alert(ALERT_RX_STATUS)
            if self.partner:
                self.partner.reached(self.buffer)

    def deliver_hard_reset(self):
        """The partner's hard reset: signalled when hard resets are received; the receive path and detection reset."""
        seen = bool(self.receive_detect & DETECT_HARD_RESET)
        self.receive = []
        self.buffer = None
        self.receive_detect = 0
        if seen:
            self.raise_alert(ALERT_RX_HARD_RESET)
        return seen

    # ---------------------------------------------------------------- I2C

    def register_bytes(self):
        """The register file as a read sees it, from 0x00 to 0x7F."""
        view = bytearray(0x80)
        struct.pack_into('<HHHHHH', view, VENDOR_ID, 0x1D6B, 0x5110, 0x0001, 0x0020, 0x3011, 0x2010)
        struct.pack_into('<HH', view, ALERT, self.alert, self.alert_mask)
        view[POWER_STATUS_MASK] = self.power_status_mask
        view[TCPC_CONTROL] = self.tcpc_control
        view[ROLE_CONTROL] = self.role_control
        view[POWER_CONTROL] = self.power_control
        view[CC_STATUS] = self.cc_status
        power = self.compute_power()
        if self.initialising:
            power |= POWER_UNINITIALIZED
        view[POWER_STATUS] = power
        view[FAULT_STATUS] = self.fault_status
        struct.pack_into('<HH', view, DEVICE_CAPABILITIES_1, CAPABILITIES_1, 0)
        view[MESSAGE_HEADER_INFO] = self.header_info
        view[RECEIVE_DETECT] = self.receive_detect
        if self.buffer is not None:
            frame = bytes([len(self.buffer) + 1, 0]) + self.buffer
            view[RECEIVE_BUFFER:RECEIVE_BUFFER + len(frame)] = frame
        millivolts = self.vbus_mv() if self.measures() else 0
        struct.pack_into('<HHH', view, VBUS_VOLTAGE, min(millivolts // 25, 0x3FF) if millivolts < 25 * 0x400 else 1 << 10 | min(millivolts // 50, 0x3FF), 0, 0)
        struct.pack_into('<HH', view, VBUS_VOLTAGE_ALARM_HI, self.alarm_hi, self.alarm_lo)
        return view

    def read(self, length, after_write):
        self.transfers += 1
        view = self.register_bytes()
        if self.pointer == POWER_STATUS and self.initialising:
            self.initialising -= 1
        return bytes(view[(self.pointer + at) & 0x7F] for at in range(length))

    def write(self, data, combined=False):
        if not data:
            return True
        if not combined:
            self.transfers += 1
        self.pointer = data[0]
        if combined or len(data) == 1:
            return True
        register, body = data[0], bytes(data[1:])
        word = struct.unpack_from('<H', body.ljust(2, b'\0'))[0]
        if register == ALERT:
            self.alert &= ~word
            if word & ALERT_RX_STATUS:
                self.buffer = None
                self.load()
            self.signal()
        elif register == ALERT_MASK:
            self.alert_mask = word
            self.signal()
        elif register == POWER_STATUS_MASK:
            self.power_status_mask = body[0]
        elif register == TCPC_CONTROL:
            self.tcpc_control = body[0]
        elif register == ROLE_CONTROL:
            before = self.sink_presented()
            self.role_control = body[0]
            if before != self.sink_presented() and self.partner:
                self.partner.rd_changed(self.sink_presented())
            self.changed()
        elif register == POWER_CONTROL:
            self.power_control = body[0]
            self.check_alarms()
        elif register == FAULT_STATUS:
            self.fault_status &= ~body[0]
        elif register == COMMAND:
            self.command(body[0])
        elif register == MESSAGE_HEADER_INFO:
            self.header_info = body[0]
        elif register == RECEIVE_DETECT:
            self.receive_detect = body[0]
            if self.partner:
                self.partner.detecting(body[0])
        elif register == TRANSMIT:
            self.transmit(body[0])
        elif register == TRANSMIT_BUFFER:
            self.transmit_buffer = body[1:1 + body[0]]
        elif register == VBUS_VOLTAGE_ALARM_HI:
            self.alarm_hi = word & 0x3FF
            self.check_alarms()
        elif register == VBUS_VOLTAGE_ALARM_LO:
            self.alarm_lo = word & 0x3FF
            self.check_alarms()
        else:
            self.others[register] = body
        return True

    def command(self, value):
        if value == COMMAND_ENABLE_VBUS_DETECT:
            self.vbus_detect = True
            self.changed()
        elif value == COMMAND_DISABLE_VBUS_DETECT:
            self.vbus_detect = False
            self.changed()
        elif value in (COMMAND_SINK_VBUS, COMMAND_DISABLE_SINK_VBUS):
            on = value == COMMAND_SINK_VBUS
            if on != self.sinking:
                self.sinking = on
                if self.partner:
                    self.partner.sink_path(on)
                self.changed()

    def transmit(self, value):
        kind = value & 0x7
        if kind == 5:
            # A HARD RESET SENT: both transmission bits say so, and the receive path and detection reset.
            self.receive = []
            self.buffer = None
            self.receive_detect = 0
            self.raise_alert(ALERT_TX_SUCCESS | ALERT_TX_FAILED)
            if self.partner:
                self.partner.hard_reset_from_sink()
            return
        if kind != 0:
            self.raise_alert(ALERT_TX_FAILED)
            return
        data = bytes(self.transmit_buffer)
        acknowledged = self.partner.receive(data) if self.partner else False
        self.raise_alert(ALERT_TX_SUCCESS if acknowledged else ALERT_TX_FAILED)
        if acknowledged:
            self.partner.acknowledged(data)


class Source:
    """THE POWER DELIVERY SOURCE on the cable: a profile, a policy engine and its scripts."""

    def __init__(self, tcpc, scheduler, log, stretch=1.0):
        self.tcpc = tcpc
        self.timers = scheduler
        self.say = log
        self.stretch = stretch
        self.profile = None
        self.pd = False
        self.revision = REVISION_3
        self.rp = 0
        self.flipped = False
        self.cc_attached = False
        self.vbus_mv = 0
        self.caps = []
        self.state = 'detached'
        self.message_id = 0
        self.last_sink_id = None
        self.caps_count = 0
        self.scripts = set()
        self.silent = False
        self.contract = None
        self.violations = []
        self.requests = []
        self.responses = []
        self.response_transfers = []
        self.transfers_at_caps = 0
        self.response_timeouts = 0
        self.hard_resets_from_sink = []
        self.hard_resets_to_sink = 0
        self.answers = []
        self.sink_capabilities = None
        self.marks = {}
        self.pending_caps_alert = None
        self.negotiation_target = 0
        self.negotiations_done = 0
        self.mark()

    # ---------------------------------------------------------------- bookkeeping

    def now(self):
        return self.timers.clock()

    def violation(self, text):
        self.violations.append(text)
        self.say(f'VIOLATION: {text}')

    def mark(self):
        """From here, what the gate asks after: VBUS's lowest, the hard resets, the sink path's offs, soft resets."""
        self.since = dict(vbus_low=self.vbus_mv, hard_resets=0, path_offs=0, soft_resets=0, alarms=0)

    def set_vbus(self, millivolts):
        self.vbus_mv = millivolts
        self.since['vbus_low'] = min(self.since['vbus_low'], millivolts)
        self.tcpc.changed()

    # ---------------------------------------------------------------- the cable

    def attach(self, name, flipped=False):
        profile = PROFILES[name]
        self.detach(quiet=True)
        self.profile = name
        self.pd = profile['pd']
        self.revision = profile['revision']
        self.rp = profile['rp']
        self.caps = list(profile['caps'])
        self.flipped = flipped
        self.cc_attached = True
        self.silent = 'no-caps' in self.scripts
        self.scripts.discard('no-caps')
        self.state = 'attach-wait'
        self.say(f'attached as {name}{" flipped" if flipped else ""}')
        self.tcpc.changed()
        if self.tcpc.sink_presented():
            self.timers.at(T_VBUS_ON, 'vbus-on', self.vbus_on)

    def detach(self, quiet=False):
        self.timers.cancel_all()
        was = self.cc_attached
        self.cc_attached = False
        self.state = 'detached'
        self.contract = None
        self.message_id = 0
        self.last_sink_id = None
        self.set_vbus(0)
        if was and not quiet:
            self.say('detached')

    def rd_changed(self, present):
        """The sink's Rd came or went - error recovery opens CC."""
        if not self.cc_attached:
            return
        if not present:
            self.say('the sink opened CC - the source sees no sink, and turns VBUS off')
            self.timers.cancel_all()
            self.contract = None
            self.state = 'attach-wait'
            self.set_vbus(0)
        else:
            self.say('the sink presents Rd again')
            self.timers.at(T_VBUS_ON, 'vbus-on', self.vbus_on)

    def vbus_on(self):
        self.set_vbus(5000)
        self.startup()

    def startup(self):
        self.message_id = 0
        self.last_sink_id = None
        self.caps_count = 0
        self.contract = None
        if not self.pd or self.silent:
            self.state = 'no-pd' if not self.pd else 'silent'
            return
        self.state = 'startup'
        self.timers.at(T_FIRST_SOURCE_CAP, 'caps', self.send_caps)

    # ---------------------------------------------------------------- sending

    def send(self, kind, objects=()):
        data = message(kind, self.revision, self.message_id, objects)
        acknowledged = self.tcpc.deliver(data)
        if self.negotiation_target == 0:
            self.say(f'to the sink: {name_of(kind, bool(objects))}{"" if acknowledged else " - not acknowledged"}')
        if acknowledged:
            self.message_id = (self.message_id + 1) % 8
        return acknowledged

    def send_caps(self):
        if not self.cc_attached or self.vbus_mv < VBUS_PRESENT_MV:
            return
        self.pending_caps_alert = None
        if self.send(SOURCE_CAPABILITIES, self.caps):
            self.state = 'send-caps'
            self.caps_count = 0
            self.timers.at(T_SENDER_RESPONSE * self.stretch, 'sender-response', self.no_request)
        else:
            self.caps_count += 1
            if self.caps_count < N_CAPS_COUNT:
                self.timers.at(T_SOURCE_CAPABILITY, 'caps', self.send_caps)
            else:
                self.state = 'no-pd'

    def no_request(self):
        self.response_timeouts += 1
        self.say(f'the sink did not answer the capabilities within {T_SENDER_RESPONSE * self.stretch * 1000:.0f} ms - a hard reset')
        self.hard_reset_to_sink()

    def answer(self, kind, objects=()):
        self.timers.at(T_RECEIVER_RESPONSE, 'answer', lambda: self.send(kind, objects))

    # ---------------------------------------------------------------- what the controller tells the partner

    def reached(self, data):
        """A message of the partner's reached the sink's receive buffer, its alert raised."""
        parsed = parse(data)
        if parsed and parsed[0] == SOURCE_CAPABILITIES and parsed[1]:
            self.pending_caps_alert = self.now()
            self.transfers_at_caps = self.tcpc.transfers
        if parsed and parsed[0] == ACCEPT and not parsed[1]:
            self.marks['accept'] = self.now()

    def detecting(self, value):
        if value & DETECT_SOP:
            self.marks['receiving'] = self.now()

    def acknowledged(self, data):
        parsed = parse(data)
        if parsed and parsed[0] == REQUEST and parsed[1]:
            self.marks['request-acknowledged'] = self.now()

    def alarmed(self):
        self.since['alarms'] += 1

    def sink_path(self, on):
        self.say(f'sink path {"on" if on else "off"} at {self.vbus_mv} mV')
        if on and (self.vbus_mv < VBUS_PRESENT_MV or not self.cc_attached):
            self.violation(f'the sink path enabled with VBUS at {self.vbus_mv} mV')
        if not on:
            self.since['path_offs'] += 1

    def receive(self, data):
        """A message the sink sent: acknowledged (GoodCRC) when a Power Delivery source is attached and listening."""
        if not (self.cc_attached and self.pd and self.vbus_mv >= VBUS_PRESENT_MV) or self.state in ('silent', 'no-pd', 'hard-reset'):
            return False
        parsed = parse(data)
        if parsed is None:
            self.violation(f'the sink sent {data.hex()}, which is no message')
            return True
        kind, is_data, count, message_id, revision, objects = parsed
        if self.negotiation_target == 0:
            self.say(f'from the sink: {name_of(kind, is_data)}')
        self.timers.at(0, 'handle', lambda: self.handle(kind, is_data, message_id, revision, objects))
        return True

    def handle(self, kind, is_data, message_id, revision, objects):
        name = name_of(kind, is_data)
        if (kind, is_data) == (SOFT_RESET, False):
            self.last_sink_id = None
        if self.last_sink_id == message_id and (kind, is_data) != (SOFT_RESET, False):
            return
        self.last_sink_id = message_id
        if revision > self.revision:
            self.violation(f'the sink spoke revision {revision + 1}.0 to a {self.revision + 1}.0 source')
        if self.state == 'transition':
            self.violation(f'the sink sent {name} while the supply moved')
            return
        if (kind, is_data) == (REQUEST, True):
            self.request(objects[0] if objects else 0)
        elif (kind, is_data) == (SOFT_RESET, False):
            self.since['soft_resets'] += 1
            self.say(f'soft reset from the sink with VBUS at {self.vbus_mv} mV')
            self.timers.cancel('sender-response', 'caps', 'transition', 'ps-rdy')
            self.message_id = 0
            self.contract = None
            self.state = 'soft-reset'
            self.answer(ACCEPT)
            self.timers.at(T_RECEIVER_RESPONSE * 2, 'caps', self.send_caps)
        elif (kind, is_data) == (SINK_CAPABILITIES, True):
            self.sink_capabilities = objects
            self.say('sink capabilities ' + ' '.join(f'{o:08x}' for o in objects))
            self.answered(name)
        elif (kind, is_data) in ((NOT_SUPPORTED, False), (REJECT, False)) and self.state == 'awaiting-answer':
            self.answered(name)
        elif (kind, is_data) == (GET_SOURCE_CAP, False):
            self.timers.at(T_RECEIVER_RESPONSE, 'caps', self.send_caps)
        else:
            self.say(f'the sink sent {name} in {self.state}')

    # ---------------------------------------------------------------- the negotiation

    def request(self, rdo):
        now = self.now()
        self.timers.cancel('sender-response')
        if self.pending_caps_alert is not None:
            response = (now - self.pending_caps_alert) * 1000
            self.responses.append(response)
            self.response_transfers.append(self.tcpc.transfers - self.transfers_at_caps)
            if self.negotiation_target == 0:
                self.say(f'the request came {response:.1f} ms and {self.response_transfers[-1]} register transfers after the capabilities\' alert')
            self.pending_caps_alert = None
        position = (rdo >> 28) & 7
        operating, maximum = ((rdo >> 10) & 0x3FF) * 10, (rdo & 0x3FF) * 10
        mismatch = (rdo >> 26) & 1
        if not 1 <= position <= len(self.caps):
            self.violation(f'a request for position {position} of {len(self.caps)} offers')
            self.answer(REJECT)
            return
        offer = self.caps[position - 1]
        if offer >> 30 != 0:
            self.violation(f'a request for offer {position}, which is not fixed ({offer:08x})')
            self.answer(REJECT)
            return
        millivolts, offered = ((offer >> 10) & 0x3FF) * 50, (offer & 0x3FF) * 10
        self.requests.append((position, millivolts, operating, maximum, mismatch))
        if self.negotiation_target == 0:
            self.say(f'request position {position} fixed {millivolts} mV operating {operating} mA maximum {maximum} mA mismatch {mismatch}')
        if operating > offered or maximum > offered:
            self.violation(f'a request for {operating}/{maximum} mA of an offer of {offered} mA')
            self.answer(REJECT)
            return
        for script, response in (('wait', WAIT), ('reject', REJECT)):
            if script in self.scripts:
                self.scripts.discard(script)
                self.say(f'answering the request with {name_of(response, False)}')
                self.state = 'ready' if self.contract else 'wait-caps'
                self.answer(response)
                return
        if 'no-answer' in self.scripts:
            self.scripts.discard('no-answer')
            self.say('leaving the request unanswered')
            self.state = 'unanswered'
            return
        if 'detach-mid' in self.scripts:
            self.scripts.discard('detach-mid')
            self.say('detaching in the middle of the negotiation')
            self.detach()
            return
        self.state = 'accepted'
        self.answer(ACCEPT)
        self.timers.at(T_RECEIVER_RESPONSE + min(T_SRC_TRANSITION * self.stretch, max(T_SRC_TRANSITION, T_SRC_TRANSITION_CEILING)), 'transition', lambda: self.transition(position, millivolts))

    def transition(self, position, millivolts):
        self.state = 'transition'
        if 'overshoot' in self.scripts:
            self.scripts.discard('overshoot')
            self.say(f'the supply overshoots to 18000 mV on its way to {millivolts} mV')
            self.set_vbus(18000)
        else:
            self.set_vbus(millivolts)
        if 'no-ps-rdy' in self.scripts:
            self.scripts.discard('no-ps-rdy')
            self.say('leaving PS_RDY unsent')
            return
        self.timers.at(T_SUPPLY_SETTLE, 'ps-rdy', lambda: self.ps_rdy(position, millivolts))

    def ps_rdy(self, position, millivolts):
        if millivolts != self.vbus_mv:
            self.set_vbus(millivolts)
        self.state = 'ready'
        self.send(PS_RDY)
        self.contract = (position, millivolts)
        if self.negotiation_target:
            self.negotiations_done += 1
            if self.negotiations_done < self.negotiation_target:
                self.timers.at(0.005, 'caps', self.send_caps)
            else:
                self.negotiation_target = 0
                self.say(f'{self.negotiations_done} negotiations done - {self.distribution()}')
        else:
            self.say(f'contract at {millivolts} mV (position {position})')

    # ---------------------------------------------------------------- hard resets

    def hard_reset_from_sink(self):
        now = self.now()
        if self.tcpc.sinking:
            self.violation('the sink sent a hard reset with its sink path on')
        since = {
            'unanswered': ('its request was acknowledged', self.marks.get('request-acknowledged')),
            'transition': ('Accept reached it', self.marks.get('accept')),
            'accepted': ('Accept reached it', self.marks.get('accept')),
        }.get(self.state, ('it began receiving', self.marks.get('receiving')))
        after = (now - since[1]) * 1000 if since[1] is not None else -1
        self.hard_resets_from_sink.append((since[0], after))
        self.since['hard_resets'] += 1
        self.say(f'hard reset from the sink {after:.1f} ms after {since[0]} (state {self.state})')
        # A SOURCE WITHOUT POWER DELIVERY never sees the signalling: its VBUS stays.
        if not self.pd:
            self.say('the source speaks no Power Delivery - its VBUS stays')
            return
        self.hard_reset()

    def hard_reset_to_sink(self):
        self.hard_resets_to_sink += 1
        self.since['hard_resets'] += 1
        seen = self.tcpc.deliver_hard_reset()
        self.say(f'hard reset sent to the sink{"" if seen else " - its controller does not receive hard resets"}')
        self.hard_reset()

    def hard_reset(self):
        """THE SOURCE'S HARD RESET: VBUS to vSafe0V after tPSHardReset, back after tSrcRecover - or kept off."""
        self.timers.cancel_all()
        self.state = 'hard-reset'
        self.contract = None
        self.message_id = 0
        self.last_sink_id = None
        self.timers.at(T_PS_HARD_RESET, 'vbus-off', self.hard_reset_vbus_off)

    def hard_reset_vbus_off(self):
        # tPSHardReset IS NOT STRETCHED: a guest the emulated ports slow down may still be switching when it runs out, so
        # there this is a note, and under KVM a violation.
        late = self.violation if self.stretch == 1.0 else (lambda text: self.say(f'NOTE: {text}'))
        if self.tcpc.sinking:
            late('the sink path was on when the hard reset took VBUS down')
        if self.tcpc.power_control & CONTROL_AUTO_DISCHARGE:
            late('the automatic discharge was on when the hard reset took VBUS down')
        self.set_vbus(0)
        if 'vbus-stays-off' in self.scripts:
            self.scripts.discard('vbus-stays-off')
            self.say('VBUS kept off after the hard reset')
            self.state = 'vbus-off'
            return
        self.timers.at(T_SRC_RECOVER, 'vbus-back', self.hard_reset_vbus_back)

    def hard_reset_vbus_back(self):
        self.set_vbus(5000)
        self.say('VBUS back at 5000 mV after the hard reset')
        self.startup()

    # ---------------------------------------------------------------- the gate's asks

    def ask(self, name, kind, is_data=False, objects=()):
        self.asked = name
        self.state = 'awaiting-answer'
        if not self.send(kind, objects):
            self.state = 'ready'
            return f'error the sink did not acknowledge {name}'
        self.timers.at(T_SENDER_RESPONSE * self.stretch * 4, 'answer-wait', self.unanswered_ask)
        return f'ok sent {name}'

    def answered(self, name):
        if self.state == 'awaiting-answer':
            self.timers.cancel('answer-wait')
            self.answers.append((self.asked, name))
            self.say(f'{self.asked} answered {name}')
            self.state = 'ready' if self.contract else 'startup'

    def unanswered_ask(self):
        if self.state == 'awaiting-answer':
            self.answers.append((self.asked, 'nothing'))
            self.say(f'{self.asked} answered nothing')
            self.state = 'ready'

    def malformed(self, kind):
        if kind == 'count':
            data = struct.pack('<H', header(SOURCE_CAPABILITIES, 2, self.revision, self.message_id)) + struct.pack('<I', self.caps[0])
        elif kind == 'reserved':
            data = message(13, self.revision, self.message_id, [0])
        elif kind == 'extended':
            data = struct.pack('<H', header(SOURCE_CAPABILITIES, 1, self.revision, self.message_id, extended=True)) + struct.pack('<I', 0x00020004)
        else:
            return f'error unknown malformed kind {kind}'
        # INJECTED AS THE CONTROLLER WOULD TAKE THEM: a malformed message on the wire is GoodCRC'd all the same.
        if not self.tcpc.receive_detect & DETECT_SOP:
            return 'error the sink is not receiving'
        self.tcpc.receive.append(data)
        self.tcpc.load()
        self.message_id = (self.message_id + 1) % 8
        return f'ok sent a malformed message ({kind})'

    def distribution(self):
        if not self.responses:
            return 'no response measured'
        ordered = sorted(self.responses)

        def at(fraction):
            return ordered[min(len(ordered) - 1, int(fraction * len(ordered)))]

        transfers = sorted(self.response_transfers) or [0]
        return f'responses {len(ordered)} p50 {at(0.5):.3f} ms p99 {at(0.99):.3f} ms max {ordered[-1]:.3f} ms timeouts {self.response_timeouts} transfers {transfers[len(transfers) // 2]}-{transfers[-1]}'

    def command(self, words):
        what = words[0]
        if what == 'attach':
            self.attach(words[1], flipped=len(words) > 2 and words[2] == 'flipped')
            return f'ok attached {words[1]}'
        if what == 'detach':
            self.detach()
            return 'ok detached'
        if what == 'script':
            if words[1] not in SCRIPTS:
                return f'error unknown script {words[1]}'
            self.scripts.add(words[1])
            return f'ok script {words[1]} armed'
        if what == 'caps':
            self.caps = list(LESS) if len(words) > 1 and words[1] == 'less' else list(PROFILES[self.profile]['caps'])
            self.send_caps()
            return f'ok capabilities sent ({len(self.caps)} offers)'
        if what == 'hard-reset':
            self.hard_reset_to_sink()
            return 'ok hard reset sent'
        if what == 'vbus':
            self.set_vbus(int(words[1]))
            return f'ok VBUS at {words[1]} mV'
        if what == 'send':
            table = {'pr-swap': ('PR_Swap', PR_SWAP, ()), 'dr-swap': ('DR_Swap', DR_SWAP, ()), 'vconn-swap': ('VCONN_Swap', VCONN_SWAP, ()), 'get-sink-cap': ('Get_Sink_Cap', GET_SINK_CAP, ()), 'discover-identity': ('Discover_Identity', VENDOR_DEFINED, (0xFF00_8001,))}
            if words[1] not in table:
                return f'error unknown message {words[1]}'
            name, kind, objects = table[words[1]]
            if kind == VENDOR_DEFINED:
                self.asked = name
                self.state = 'awaiting-answer'
                if not self.send(kind, objects):
                    self.state = 'ready'
                    return f'error the sink did not acknowledge {name}'
                self.timers.at(T_SENDER_RESPONSE * self.stretch * 4, 'answer-wait', self.unanswered_ask)
                return f'ok sent {name}'
            return self.ask(name, kind, objects=objects)
        if what == 'malformed':
            return self.malformed(words[1])
        if what == 'negotiate':
            self.responses = []
            self.response_transfers = []
            self.response_timeouts = 0
            self.negotiations_done = 0
            self.negotiation_target = int(words[1])
            self.send_caps()
            return f'ok negotiating {words[1]} times'
        if what == 'timing':
            if self.negotiation_target:
                return f'ok running {self.negotiations_done}/{self.negotiation_target}'
            return f'ok done {self.distribution()}'
        if what == 'responses':
            return 'ok ' + ' '.join(f'{r:.3f}' for r in self.responses)
        if what == 'mark':
            self.mark()
            return 'ok marked'
        if what == 'status':
            return self.status()
        if what == 'answers':
            return 'ok ' + '; '.join(f'{asked} -> {answered}' for asked, answered in self.answers)
        if what == 'requests':
            return 'ok ' + '; '.join(f'{p} {mv} {op} {mx} {mm}' for p, mv, op, mx, mm in self.requests)
        if what == 'hard-resets':
            return 'ok ' + '; '.join(f'{after:.1f} ms after {since}' for since, after in self.hard_resets_from_sink)
        if what == 'violations':
            return 'ok none' if not self.violations else 'error ' + ' | '.join(self.violations)
        return f'error unknown tcpc command {what}'

    def status(self):
        contract = f'{self.contract[1]}' if self.contract else 'none'
        since = self.since
        return (f'ok state {self.state} attached {int(self.cc_attached)} vbus {self.vbus_mv} sinking {int(self.tcpc.sinking)} contract {contract} '
                f'requests {len(self.requests)} hard-resets-from-sink {len(self.hard_resets_from_sink)} hard-resets-to-sink {self.hard_resets_to_sink} '
                f'vbus-low {since["vbus_low"]} marked-hard-resets {since["hard_resets"]} marked-path-offs {since["path_offs"]} '
                f'marked-soft-resets {since["soft_resets"]} marked-alarms {since["alarms"]} receive-detect {self.tcpc.receive_detect:#04x}')


def build(address, line, set_line, log, stretch=1.0, clock=time.monotonic):
    """The controller and its partner, joined: the controller's registers, the partner's policy engine, one scheduler."""
    scheduler = Scheduler(clock)
    tcpc = Tcpc(address, line, set_line, log, clock)
    tcpc.partner = Source(tcpc, scheduler, log, stretch)
    return tcpc, scheduler
