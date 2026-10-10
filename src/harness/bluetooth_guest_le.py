"""Production guest scenarios for independent ordinary LE peers."""
import re
import time

GROUP = 'le-generic'


def data_peer(g):
    return getattr(g, 'le_data_peer', 'tag')


def control(g, action, peer=None, **values):
    return g.control(action, group=GROUP, peer=peer or data_peer(g), **values)


def state(g, peer=None):
    return control(g, 'status')[peer or data_peer(g)]


def settled(g, peer, predicate, timeout=40):
    return g.eventually('status', lambda result: predicate(result[peer]), group=GROUP, timeout=timeout)[peer]


def fingerprints(row):
    return row['keys']


def paired_count(row):
    return row['smp_pairings'], row['classic_pairings']


def deny_pair(g, peer, *, legacy=False, kind='random', cause=None):
    g.command('btctl scan 5')
    cursor = g.events()['next']
    output = g.command(f'btctl {"pair-legacy" if legacy else "pair"} {g.address(peer)} {kind}')
    g.require('btctl: the pairing failed' in output or 'the pairing was refused (Denied)' in output,
              f'{peer} pairing policy produced no explicit refusal: {output}')
    g.require(not re.search(r'(?m)^bonded(?:[:\r\n]|$)', output), f'{peer} refused pairing produced a bond')
    if cause:
        g.require(cause in output, f'{peer} pairing failed without the causal policy refusal: {output}')
    elif legacy:
        g.require('the pairing was refused (Denied)' in output, 'legacy downgrade lacked an explicit Denied result')
    else:
        failure = g.event('generic-pairing-failure', peer, since=cursor)
        g.require(failure['reason'] == 3, f'legacy-only peer did not receive SMP Authentication Requirements refusal: {failure}')


def no_grant(g, error):
    mark = g.start('btgatt')
    output = g.finish(mark)
    g.require(f'PermissionManager: Bluetooth GATT grant for btgatt alias tag-1 refused: {error}; launch refused' in output and 'btgatt:' not in output,
              f'untrusted/nonbonded GATT alias did not refuse its grant: {output}')


def type_tag(g, label):
    row = settled(g, data_peer(g), lambda row: any(c['encrypted'] and c['hid_notify'] for c in row['connections']))
    g.wait_hid_input(label, peer=data_peer(g), group=GROUP)
    mark = len(g.serial.data)
    control(g, 'type', text=f'echo independent-hogp-{label}\n')
    output = g.finish(mark)
    g.require(re.search(rf'(?m)^independent-hogp-{label}\r?$', output), 'HOGP report did not type through InputService into the console')
    g.require(row['hid_reads'] >= 2, 'HOGP report map was not read across multiple ATT responses')
    return row


def privacy_evidence(g):
    proof = control(g, 'privacy')
    g.require(proof['encrypted'] and proof['is_private'] and proof['identity'], f'guest RPA was not independently resolved: {proof}')
    g.require(proof['identity'].split('/')[0].lower() == g.guest_address, f'guest distributed the wrong identity: {proof}')
    return proof


def subscription_failures(g):
    # Deliberately withholding one real Bumble discovery request is a same-team
    # fault oracle, separate from the independent positive ATT interoperability.
    def only_fresh_writes(before, after):
        writes = after['writes'][len(before['writes']):]
        g.require(len(writes) == 1 and writes[0]['value'] == b'hello'.hex(),
                  f'a retired GATT operation still transmitted its queued value: {writes}')
        handle = writes[0]['handle']
        requests = after['att_requests'][len(before['att_requests']):]
        g.require(requests == [dict(handle=handle + 1, value='0100', encrypted=True),
                               dict(handle=handle, value=b'hello'.hex(), encrypted=True)],
                  f'a retired subscription still wrote its CCCD: {requests}')
        return writes

    before = state(g)
    cursor = g.events()['next']
    control(g, 'att-hold')
    mark = g.start('btgatt revoke')
    held = g.event('generic-att-held', data_peer(g), since=cursor, timeout=15)
    output = g.finish(mark, timeout=20)
    g.require('btgatt: PASS revoke owner ended with queued operations' in output and 'btgatt: FAIL' not in output,
              f'queued GATT owner did not finish its revocation fixture: {output}')
    released = control(g, 'att-release')
    g.require(released == dict(released=1, retired=0), f'owner fixture did not release the same live bearer: {released}')
    # A later complete ordinary launch is a barrier through the same bearer;
    # its hello is the only new write allowed after the old owner has ended.
    g.command('btgatt', expect='btgatt: PASS')
    after = state(g)
    writes = only_fresh_writes(before, after)
    g.record('ordinary-le-gatt-owner-revocation', held=held, released=released, writes=writes,
             oracle='same-team withheld ATT request on independent Bumble server')

    before = after
    cursor = g.events()['next']
    control(g, 'att-hold')
    mark = g.start('btgatt timeout')
    held = g.event('generic-att-held', data_peer(g), since=cursor, timeout=15)
    output = g.finish(mark, timeout=50)
    g.require(re.search(r'btgatt: PASS silent subscription timed out after [0-9]+ ticks', output)
              and 'btgatt: FAIL' not in output, f'silent GATT subscription did not meet its deadline: {output}')
    ended = settled(g, data_peer(g), lambda row: len(row['att_held']) == 1 and not row['att_held'][0]['connected'], timeout=10)
    released = control(g, 'att-release')
    g.require(released == dict(released=0, retired=1), f'a late response was moved onto a replacement bearer: {released}')
    settled(g, data_peer(g), lambda row: len(row['encryptions']) > len(before['encryptions'])
            and any(c['encrypted'] for c in row['connections']))
    g.command('btgatt', expect='btgatt: PASS')
    after = state(g)
    writes = only_fresh_writes(before, after)
    g.record('ordinary-le-gatt-subscription-timeout', held=held, retired=ended['att_held'],
             released=released, writes=writes, output=output,
             oracle='same-team withheld ATT request on independent Bumble server')


def application_le(g):
    # The grant/ATT/HOGP contract is the same on either explicitly bonded peer;
    # callers retain their actual pairing level in the evidence.
    g.command(f'btctl alias {g.address(data_peer(g))} tag-1', expect='is "tag-1"')
    no_grant(g, 'Denied')
    g.command(f'btctl trust {g.address(data_peer(g))} gatt', expect='is trusted for gatt')
    g.command('btgatt', expect='btgatt: PASS')
    g.command('btgatt indicate', expect="peer's indication came back")
    tag = settled(g, data_peer(g), lambda row: row['indications'] >= 1)
    g.require(len(tag['writes']) == 2 and all(row['value'] == b'hello'.hex() and row['encrypted'] for row in tag['writes']),
              f'application GATT operations did not reach the independent encrypted attributes: {tag["writes"]}')
    subscription_failures(g)
    server = control(g, 'server-values')
    g.require(server['names'] == ['LiberSystem'] and server['appearances'], f'guest GAP server values missing: {server}')
    g.require(any('1800' in uuid for uuid in server['services']) and any('1801' in uuid for uuid in server['services']),
              f'guest did not expose GAP/GATT services: {server}')
    g.command_eventually('btctl list', lambda text: any(g.address(data_peer(g)) in line.lower() and '87%' in line for line in text.splitlines()))
    g.command(f'btctl trust {g.address(data_peer(g))} input', expect='is trusted for input')
    type_tag(g, 'initial')
    g.le_privacy_initial = privacy_evidence(g)
    g.le_privacy_started = time.monotonic()
    g.le_tag_keys = fingerprints(state(g))
    g.le_tag_pairings = paired_count(state(g))
    g.record('ordinary-le-gatt-hogp', peer_name=data_peer(g), security=('SC authenticated' if data_peer(g) == 'tag' else 'explicit LE legacy Just Works'), peer=state(g), privacy=g.le_privacy_initial, guest_server=server)



def ordinary_le(g, pair):
    # The tag starts at a real RPA and distributes a separate static identity.
    tag = control(g, 'advertise')
    advertising = tag['advertising_address'].split('/')[0].lower()
    g.require(advertising != g.address('tag'), 'private-address peer advertised its identity')
    pair(g, 'tag', 'Secure Connections, authenticated', group=GROUP, kind='random',
         address=advertising, io='DISPLAY_OUTPUT_AND_YES_NO_INPUT')
    application_le(g)

    # Secure Connections Passkey Entry in both directions, with three LE links
    # actually present together (the earbuds later cover SC Just Works).
    for peer, io in [('display', 'DISPLAY_OUTPUT_ONLY'), ('remote', 'KEYBOARD_INPUT_ONLY')]:
        control(g, 'advertise', peer)
        pair(g, peer, 'Secure Connections, authenticated', group=GROUP, kind='random', io=io)
    peers = control(g, 'status')
    g.require(all(any(c['encrypted'] for c in peers[name]['connections']) for name in ('tag', 'display', 'remote')), 'three independent encrypted LE links were not simultaneously present')
    before = control(g, 'save-keys', 'remote')
    control(g, 'disconnect', 'remote')
    control(g, 'forget-keys', 'remote')
    control(g, 'pairing-config', 'remote', io='NO_OUTPUT_NO_INPUT', sc=True, mitm=False)
    deny_pair(g, 'remote', cause='BluetoothService: a bonded LE peer paired again at a lower level; the bond is kept and the link refused')
    control(g, 'disconnect', 'remote')
    control(g, 'pairing-config', 'remote', io='KEYBOARD_INPUT_ONLY', sc=True)
    restored = control(g, 'restore-keys', 'remote')
    g.require(fingerprints(restored) == fingerprints(before), 'independent saved LE key snapshot changed')
    count = paired_count(restored)
    g.command(f'btctl trust {g.address("remote")} input', expect='is trusted for input')
    restored = settled(g, 'remote', lambda row: any(c['encrypted'] for c in row['connections']))
    g.require(paired_count(restored) == count and fingerprints(restored) == fingerprints(before), 'LE authentication downgrade replaced the guest bond')
    g.command(f'btctl untrust {g.address("remote")} input')
    g.record('ordinary-le-authentication-downgrade-refused', peer=restored)
    # Downgrade to legacy is forbidden even when explicitly requested, and the
    # existing authenticated SC bond remains usable after the refusal.
    before = state(g, 'remote')
    control(g, 'disconnect', 'remote')
    control(g, 'pairing-config', 'remote', io='KEYBOARD_INPUT_ONLY', sc=False)
    deny_pair(g, 'remote', legacy=True)
    g.require(fingerprints(state(g, 'remote')) == fingerprints(before), 'legacy downgrade changed the independent saved key')
    g.command(f'btctl forget {g.address("remote")}', expect='is forgotten')
    control(g, 'forget-keys', 'remote')
    deny_pair(g, 'remote')
    control(g, 'disconnect', 'remote')
    pair(g, 'remote', 'LE legacy', group=GROUP, kind='random', io='KEYBOARD_INPUT_ONLY', sc=False, legacy=True)
    legacy = state(g, 'remote')
    g.command(f'btctl trust {g.address("remote")} input', expect='is trusted for input')
    control(g, 'disconnect', 'remote')
    restored = settled(g, 'remote', lambda row: len(row['encryptions']) > len(legacy['encryptions']) and any(c['encrypted'] for c in row['connections']))
    g.require(paired_count(restored) == paired_count(legacy) and fingerprints(restored) == fingerprints(legacy), 'legacy reconnect did not reuse EDIV/Rand/LTK without pairing')
    g.command(f'btctl forget {g.address("remote")}', expect='is forgotten')
    control(g, 'disconnect', 'remote')
    control(g, 'forget-keys', 'remote')
    pair(g, 'remote', 'LE legacy', group=GROUP, kind='random', io='NO_OUTPUT_NO_INPUT', sc=False, mitm=False, legacy=True)
    g.record('ordinary-le-pairing-models', peers=control(g, 'status'), legacy_reconnect=restored)
    for peer in ('display', 'remote'):
        control(g, 'stop-advertising', peer)
        control(g, 'disconnect', peer)
    ctkd(g, pair)
    reuse_tag(g, 'peer-rpa', rotate=True)


def ordinary_le_without_dhkey(g, pair):
    # These are the already-required explicitly requested legacy cases. Never
    # relabel them as a replacement proof of any Secure Connections scenario.
    g.le_data_peer = 'remote'
    control(g, 'advertise')
    control(g, 'pairing-config', io='KEYBOARD_INPUT_ONLY', sc=False)
    g.command('btctl scan 5', expect=g.address('remote'))
    output = g.command(f'btctl pair {g.address("remote")} random')
    g.require('the pairing was refused (Unsupported)' in output,
              f'controller without DHKey did not refuse ordinary LE SC pairing: {output}')
    g.record('le-sc-controller-refusal', output=output,
             scope='controller capability refusal; not the SMP legacy Authentication Requirements negative')
    pair(g, 'remote', 'LE legacy', group=GROUP, kind='random', io='KEYBOARD_INPUT_ONLY', sc=False, legacy=True)
    legacy = state(g)
    g.command(f'btctl trust {g.address("remote")} input', expect='is trusted for input')
    control(g, 'disconnect')
    restored = settled(g, 'remote', lambda row: len(row['encryptions']) > len(legacy['encryptions']) and any(c['encrypted'] for c in row['connections']))
    g.require(paired_count(restored) == paired_count(legacy) and fingerprints(restored) == fingerprints(legacy), 'legacy reconnect did not reuse EDIV/Rand/LTK without pairing')
    g.command(f'btctl forget {g.address("remote")}', expect='is forgotten')
    control(g, 'disconnect')
    control(g, 'forget-keys')
    pair(g, 'remote', 'LE legacy', group=GROUP, kind='random', io='NO_OUTPUT_NO_INPUT', sc=False, mitm=False, legacy=True)
    g.record('ordinary-le-explicit-legacy-models', peer=state(g), legacy_reconnect=restored,
             scope='Passkey Entry then Just Works; SC models and SC downgrade negatives remain fixture evidence')
    application_le(g)
    ctkd(g, pair, le_sc=False)
    reuse_tag(g, 'peer-rpa', rotate=True)


def ctkd(g, pair, *, le_sc=True):
    # Both radios use this peer's actual controller public address. The operator
    # disambiguates the two records by radio; no second authority/grant is added.
    peer, address = 'dual', g.address('dual')
    pair(g, peer, 'Secure Connections, authenticated', group=GROUP, kind='bredr', io='DISPLAY_OUTPUT_AND_YES_NO_INPUT')
    def both_keys(row):
        return any('link_key' in item['fingerprints'] and 'ltk' in item['fingerprints'] for item in row['keys'])
    derived = settled(g, peer, both_keys)
    g.command(f'btctl -k public trust {address} input', expect='is trusted for input')
    control(g, 'advertise', peer)
    restored = settled(g, peer, lambda row: any(c['transport'] == 'LE' and c['encrypted'] for c in row['connections']))
    g.require(paired_count(restored) == paired_count(derived) and fingerprints(restored) == fingerprints(derived), 'BR/EDR-to-LE derived key was not reused')
    g.record('ctkd-bredr-to-le', peer=restored)
    control(g, 'stop-advertising', peer)
    for kind in ('public', 'bredr'):
        g.command(f'btctl -k {kind} forget {address}', expect='is forgotten')
    control(g, 'disconnect', peer)
    control(g, 'forget-keys', peer)
    if not le_sc:
        return
    control(g, 'advertise', peer)
    pair(g, peer, 'Secure Connections, authenticated', group=GROUP, kind='public', io='DISPLAY_OUTPUT_AND_YES_NO_INPUT')
    derived = settled(g, peer, both_keys)
    g.command(f'btctl -k bredr trust {address} input', expect='is trusted for input')
    control(g, 'connect-classic', peer, address=g.guest_address)
    restored = settled(g, peer, lambda row: any(c['transport'] == 'BR_EDR' and c['encrypted'] for c in row['connections']))
    g.require(paired_count(restored) == paired_count(derived) and fingerprints(restored) == fingerprints(derived), 'LE-to-BR/EDR derived key was not reused')
    g.record('ctkd-le-to-bredr', peer=restored)
    control(g, 'stop-advertising', peer)
    control(g, 'disconnect', peer)


def reuse_tag(g, label, *, rotate=False):
    before = state(g)
    rotation = None
    if rotate:
        rotation = control(g, 'rotate')
        g.require(rotation['is_resolvable'] and rotation['identity'].split('/')[0].lower() == g.address(data_peer(g)),
                  f'peer rotation did not retain its actual identity/address kind: {rotation}')
        g.require(rotation['address'] != before['advertising_address'], 'peer RPA did not change')
    else:
        control(g, 'advertise')
    row = type_tag(g, label)
    g.require(paired_count(row) == g.le_tag_pairings and fingerprints(row) == g.le_tag_keys, f'{label} replaced the stored LE bond')
    privacy = privacy_evidence(g)
    if rotation:
        g.require(row['advertising_address'] == rotation['address'], 'encrypted reconnection did not retain the rotated advertising address')
    g.record('ordinary-le-reuse-' + label, peer_name=data_peer(g), privacy=privacy, peer=row, peer_rotation=rotation)
    return privacy


def privacy_rotation(g):
    # The production timer is fifteen minutes. Other radio scenarios run during
    # this interval; no test-only clock or rewritten production deadline is used.
    remaining = 905 - (time.monotonic() - g.le_privacy_started)
    if remaining > 0:
        print(f'Waiting {remaining:.0f}s for the production LE private-address timer', flush=True)
        g.pause(remaining)
    before = state(g)
    control(g, 'disconnect')
    settled(g, data_peer(g), lambda row: len(row['encryptions']) > len(before['encryptions']) and any(c['encrypted'] for c in row['connections']))
    privacy = reuse_tag(g, 'host-rpa')
    g.require(privacy['remote'] != g.le_privacy_initial['remote'], 'guest own RPA never rotated on its production timer')
    g.record('ordinary-le-periodic-privacy', before=g.le_privacy_initial, after=privacy)


def forget_tag(g):
    g.command(f'btctl forget {g.address(data_peer(g))}', expect='is forgotten')
    rotated = control(g, 'rotate')
    g.require(rotated['is_resolvable'], f'forgotten peer did not advertise an actual RPA: {rotated}')
    g.pause(3)
    g.require(not state(g)['connections'], 'forgotten HOGP peer rejoined the accept list')
    no_grant(g, 'NotFound')
    g.record('ordinary-le-forget', peer_name=data_peer(g), peer=state(g),
             peer_rotation=rotated)
