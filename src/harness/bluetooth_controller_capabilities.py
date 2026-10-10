"""Read-only observations of the independent guest controller's HCI replies.

Nothing here supplies or changes an HCI response. A known missing DHKey command
selects only P194's explicit per-feature fixture fallback; other missing inputs
or capabilities remain an error rather than silently reducing the experiment.
"""
import re

# Bluetooth Core Supported Commands table (octet, bit). The host equipment
# check cross-checks these positions against the pinned independent Bumble API.
COMMANDS = {
    'ssp_io_reply': (18, 7), 'esco_setup': (29, 3), 'esco_accept': (29, 4),
    'classic_sc_host': (32, 3), 'p256_public_key': (34, 1), 'dhkey': (34, 2),
    'dhkey_v2': (41, 2), 'set_cig': (41, 7), 'create_cis': (42, 1),
    'big_sync': (43, 0), 'iso_data_path': (43, 3),
}
CAPABILITY_OPS = {0x1002, 0x1003, 0x2003, 0x0c7a}


def capability_reply(packet):
    if len(packet) >= 5 and packet[0] == 0x0e and int.from_bytes(packet[3:5], 'little') in CAPABILITY_OPS:
        return packet.hex()
    return None


def read_capabilities(log):
    replies = {}
    for raw in re.findall(r'HCI capability ([0-9a-f]+)', log):
        packet = bytes.fromhex(raw)
        if len(packet) < 6 or packet[0] != 0x0e or packet[1] != len(packet) - 2:
            raise ValueError('malformed observed HCI capability completion')
        op = int.from_bytes(packet[3:5], 'little')
        if op in replies and replies[op] != packet[5:]:
            raise ValueError('controller capability changed within the observed boot')
        replies[op] = packet[5:]
    for op, size in ((0x1002, 65), (0x1003, 9), (0x2003, 9), (0x0c7a, 1)):
        if op not in replies or len(replies[op]) != size or replies[op][0] != 0:
            raise ValueError(f'missing, refused or malformed actual controller capability 0x{op:04x}')
    commands = replies[0x1002][1:]
    lmp = int.from_bytes(replies[0x1003][1:], 'little')
    le = int.from_bytes(replies[0x2003][1:], 'little')
    supported = {name: bool(commands[octet] & (1 << bit)) for name, (octet, bit) in COMMANDS.items()}
    features = dict(ssp=bool(lmp & (1 << 51)), esco=bool(lmp & (1 << 31)),
                    le_encryption=bool(le & 1), privacy=bool(le & (1 << 6)),
                    extended_advertising=bool(le & (1 << 12)), periodic_advertising=bool(le & (1 << 13)),
                    cis_central=bool(le & (1 << 28)), cis_peripheral=bool(le & (1 << 29)),
                    bis_source=bool(le & (1 << 30)), bis_sink=bool(le & (1 << 31)))
    missing = [name for name, available in supported.items() if not available and name not in ('dhkey', 'dhkey_v2')]
    missing += [name for name, available in features.items() if not available]
    if missing:
        raise ValueError(f'unexpected missing controller prerequisites: {missing}')
    if not supported['dhkey'] and supported['dhkey_v2']:
        raise ValueError('unreviewed controller with DHKey v2 only; do not silently select the stock fallback')
    return dict(commands_hex=commands.hex(), lmp_features_hex=replies[0x1003][1:].hex(),
                le_features_hex=replies[0x2003][1:].hex(), commands=supported, features=features,
                classic_sc_host_accepted=True, le_secure_connections=supported['dhkey'],
                evidence='unaltered command-complete packets received by the production USB bridge')


def fixture_fallbacks(capabilities):
    rows = [dict(feature='encrypted BIG wrong/no-code RF rejection', gate='bluetooth-le-audio',
                 reason='pinned RootCanal BIG create sync ignores encryption and Broadcast Code; correct-code transport remains independent')]
    if not capabilities['le_secure_connections']:
        rows.extend([
            dict(feature='LE SC Numeric Comparison, both Passkey directions, authentication/agreement downgrade refusals, multiple encrypted links',
                 gate='bluetooth-le', reason='controller Generate DHKey is absent; no legacy replacement of SC assertions',
                 required_fixture_cases=['SC passkey shown by guest', 'LE SC authentication downgrade with original-key reuse']),
            dict(feature='SC HOGP bond reuse across service restart/cold boot and forget',
                 gate='bluetooth-service', reason='tag SC pairing needs controller DHKey; explicit legacy remote independently retains GATT/HOGP, peer/own-RPA and bond lifecycle coverage'),
            dict(feature='LE-to-BR/EDR CTKD', gate='bluetooth-le', reason='initial LE SC pairing needs controller DHKey; BR/EDR-to-LE direction remains independent'),
            dict(feature='LE Audio coordinated-set SC bonding, CIS stereo/voice, VCS, GTBS, set departure and BIG-to-CIS routing',
                 gate='bluetooth-le-audio', reason='earbud SC bonding needs controller DHKey; standalone BIG-to-A2DP route remains independent'),
        ])
    return rows


def self_test():
    # Recorded stock RootCanal 1.13.0 replies, not an invented runtime oracle.
    # Mutations below test only the harness classifier and never reach a guest.
    raw = 'HCI capability 0e4401021000f3ffef03ceff83ff3f3fff1ffe0fe86efff783fffc000418e3f7ffff7f3f0000fef0fb7ffeffff07c0a3ff1f9a01030000000000000000000000000000000000\nHCI capability 0e0c01031000fffe8ffedbff1b87\nHCI capability 0e0c01032000ff3900f322400000\nHCI capability 0e04017a0c00\n'
    packets = [bytes.fromhex(line.split()[-1]) for line in raw.splitlines()]
    def lines(values):
        return ''.join('HCI capability ' + value.hex() + '\n' for value in values)
    observed = read_capabilities(raw)
    assert not observed['le_secure_connections'] and len(fixture_fallbacks(observed)) == 5
    def refuse(values):
        try:
            read_capabilities(lines(values))
        except ValueError:
            return
        raise AssertionError('missing/refused/malformed/unreviewed controller accepted')
    for index in range(len(packets)):
        refuse(packets[:index] + packets[index + 1:])
        changed = list(packets)
        packet = bytearray(changed[index])
        packet[5] = 1
        changed[index] = bytes(packet)
        refuse(changed)
        changed[index] = packets[index][:-1]
        refuse(changed)
    for name, (octet, bit) in COMMANDS.items():
        if name in ('dhkey', 'dhkey_v2'):
            continue
        changed = list(packets)
        packet = bytearray(changed[0])
        packet[6 + octet] &= ~(1 << bit)
        changed[0] = bytes(packet)
        refuse(changed)
    changed = list(packets)
    packet = bytearray(changed[0])
    packet[6 + 34] |= 4
    changed[0] = bytes(packet)
    capable = read_capabilities(lines(changed))
    assert capable['le_secure_connections'] and len(fixture_fallbacks(capable)) == 1
    refuse(packets + changed[:1])
    packet[6 + 34] &= ~4
    packet[6 + 41] |= 4
    changed[0] = bytes(packet)
    refuse(changed)
