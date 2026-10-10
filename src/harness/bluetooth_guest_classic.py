"""Production guest actions for independent classic radio profile verification.

The gate supplies serial/control plumbing. Acceptance uses separately running
Bumble/AOSP peers, and the explicitly same-team OPP/BNEP peers with real dnsmasq.
"""
import hashlib
import re
import time


def audio_device(g, predicate, timeout=25):
    for _ in range(timeout):
        inventory = g.command('audioctl devices')
        for line in inventory.splitlines():
            match = re.search(r'device (\d+):', line)
            if match and predicate(line):
                return int(match[1]), line
        g.pause(1)
    raise AssertionError('expected Bluetooth audio device did not arrive: ' + inventory)


def peer_event(g, event, since, peer='headset', **fields):
    return g.eventually('events', lambda result: any(
        item.get('peer') == peer and item.get('event') == event and all(item.get(key) == value for key, value in fields.items())
        for item in result['events']), peer=peer, since=since, timeout=20)


def gain_notifications(g, since, expected):
    # ERROR from a harmless unsupported command is a same-DLC ordering barrier:
    # Bumble has parsed all earlier AG notifications before its response.
    refusal = g.control('hfp-expect-error', peer='headset', command='AT+INDEPENDENT_GAIN_BARRIER')
    g.require(refusal.get('status') == 'ERROR', 'gain event barrier was not an actual ERROR response')
    events = g.control('events', since=since)['events']
    observed = [(item['event'], item['value']) for item in events
                if item.get('peer') == 'headset' and item.get('event') in ('hfp-volume', 'hfp-microphone-volume')]
    g.require(observed == expected, f'gain update emitted stale, duplicate or cross-control notifications: {observed}, expected {expected}')


def late_voice_completion(g):
    cursor = g.control('events')['next']
    g.control('sco-hold', peer='headset')
    mark = g.start('audioprobe radio-voice-close')
    g.wait(mark, 'radio-voice-close waiting')
    g.event('sco-request-held', 'headset', since=cursor, timeout=15)
    peer_event(g, 'hfp-indicator', cursor, name='call', value=1)
    g.control('hangup', peer='headset')
    g.require('PASS radio-voice-close' in g.finish(mark), 'the pending voice owner did not close')
    # The application made no PCM request, so its close reaches AudioService's
    # last-owner cleanup and the Bluetooth PCM peer-close without an outstanding
    # period hiding that channel from the wait set. BCC ERROR is the causal barrier:
    # only voice_down clears audio_allowed; setting the call to None does not.
    peer_event(g, 'hfp-indicator', cursor, name='call', value=0)
    healthy = g.control('hfp-at', peer='headset', command='AT+CLCC')
    g.require(healthy.get('response') == '[]', 'the live negotiated HFP channel did not answer CLCC with OK/no call')
    refusal = g.control('hfp-expect-error', peer='headset', command='AT+BCC')
    g.require(refusal.get('status') == 'ERROR', 'voice-owner withdrawal lacked an actual BCC ERROR')
    healthy = g.control('hfp-at', peer='headset', command='AT+CLCC')
    g.require(healthy.get('response') == '[]', 'the HFP channel stopped answering after BCC refusal')
    g.require(g.control('status')['headset']['sco_request_held'], 'the independent acceptance was no longer held')
    # Release the actual independent headset's accept only after application/PCM
    # ownership ended. A timeout or disconnected HFP channel cannot satisfy this.
    g.control('sco-release', peer='headset')
    g.event('sco-connected', 'headset', since=cursor, timeout=15)
    departed = g.event('sco-disconnected', 'headset', since=cursor, timeout=15)
    g.require(departed['reason'] == 0x13, f'orphan SCO was not explicitly disconnected by the guest: {departed}')
    g.require(not g.control('status')['headset']['sco'], 'orphan SCO remains after its voice owner ended')
    retained = g.control('events', since=cursor)['events']
    g.require(not any(item.get('peer') == 'headset' and item.get('event') in ('hfp-slc', 'hsp-connected', 'disconnection') for item in retained),
              'late-completion refusal did not stay on the same negotiated HFP connection')
    voice, _ = audio_device(g, lambda line: 'Bluetooth, voice' in line)
    g.command_eventually(f'audioctl microphone {voice}', lambda text: f'device {voice}: microphone level 53' in text)
    g.command(f'audioctl default {voice} voice')
    g.record('classic-late-voice-completion', scheduling='held real independent headset acceptance',
             completion='actual SCO success after voice-owner/PCM close', refusal=departed,
             recovery='same HFP connection, microphone53 retained; normal duplex/endpoint reopen follows')


def refused_pair(g, peer, cause, *, legacy=False):
    """Answer any real numeric prompt, then require a protocol failure, never a timeout."""
    cursor = g.events()['next']
    mark = g.start(f'btctl pair-legacy {g.address(peer)} bredr' if legacy else f'btctl pair {g.address(peer)} bredr')
    answered = False
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        text = g.serial.text_since(mark)
        if b'btctl: the pairing failed' in text or b'was refused' in text or b'bonded:' in text:
            break
        match = re.search(rb'Does it match (\d{6})\? \[y/n\]', text)
        if match and not answered:
            remote = g.event('pairing-number', peer, since=cursor)
            g.require(remote['number'] == int(match[1]), 'independent downgrade comparison numbers differed')
            g.serial.type(b'y\n')
            answered = True
        g.require(b'PIN for ' not in text, 'forbidden legacy pairing reached a PIN-entry prompt')
        g.pause(.05)
    output = g.finish(mark)
    g.require('bonded:' not in output and ('btctl: the pairing failed' in output or 'was refused' in output),
              f'forbidden pairing lacked an explicit refusal: {output}')
    g.require(cause in output, f'pairing failure lacks the causal guest policy refusal (a timeout is not evidence): {output}')
    return output


def classic_pairing(g, pair):
    """Initial classic bonds and their independent security-policy adversaries."""
    pair(g, 'keyboard', 'Secure Connections, authenticated', io='KEYBOARD_INPUT_ONLY')
    pair(g, 'phone', 'Secure Connections, authenticated', io='DISPLAY_OUTPUT_AND_YES_NO_INPUT')
    phone = g.address('phone')
    g.command(f'btctl trust {phone} audio', expect='trusted')
    saved = g.control('save-key', peer='phone', address=g.guest_address)
    for name, settings, legacy, cause in (
        ('agreement', dict(io='DISPLAY_OUTPUT_AND_YES_NO_INPUT', sc=False, mitm=True, ssp=True), False,
         'BluetoothService: a bonded peer paired again at a lower level; the bond is kept and the link refused'),
        ('authentication', dict(io='NO_OUTPUT_NO_INPUT', sc=True, mitm=False, ssp=True), False,
         'BluetoothService: a peer bonded with authentication offered Just Works; refused, and the bond is kept'),
        ('legacy-over-sc', dict(io='NO_OUTPUT_NO_INPUT', sc=False, mitm=False, ssp=False), True,
         'BluetoothService: a legacy PIN pairing was refused - the operator did not ask for one, or a better bond exists'),
    ):
        g.control('disconnect', peer='phone')
        g.control('forget-key', peer='phone', address=g.guest_address)
        g.control('pairing-config', peer='phone', **settings)
        g.command('btctl scan 5')
        refused_pair(g, 'phone', cause, legacy=legacy)
        g.command_eventually('btctl list', lambda text: any(phone in row.lower() and 'Secure Connections, authenticated' in row for row in text.splitlines()))
        g.control('disconnect', peer='phone')
        g.control('pairing-config', peer='phone', io='DISPLAY_OUTPUT_AND_YES_NO_INPUT', sc=True, mitm=True, ssp=True)
        restored = g.control('restore-key', peer='phone', address=g.guest_address)
        g.require(restored == saved, 'independent original key snapshot changed')
        before = g.control('status')['phone']['pairings']
        secured = g.control('authenticate', peer='phone', address=g.guest_address)
        g.require(secured['encrypted'] and secured['pairings'] == before, 'refused downgrade replaced the guest bond: its original key no longer reconnects')
        g.record('classic-downgrade-' + name, retained_key=saved['sha256'], reconnect='encrypted without new pairing')

    # A trusted phone keeps page scan enabled. The still-unbonded headset must
    # get an actual security refusal, rather than an unrelated paging timeout.
    g.control('pairing-config', peer='headset', io='NO_OUTPUT_NO_INPUT', sc=True, mitm=False, ssp=True)
    g.control('pairing-start', peer='headset', address=g.guest_address)
    refused = g.eventually('pairing-result', lambda value: value is not None, peer='headset', timeout=35)
    g.require(refused.get('ok') is False and refused.get('error_namespace') == 'hci' and refused.get('error_code') in (0x05, 0x0E, 0x0F, 0x18, 0x2F),
              f'unwatched incoming pairing had no explicit security refusal: {refused}')
    g.require(g.control('status')['headset']['pairings'] == 0, 'unwatched incoming peer gained a bond')
    g.control('disconnect', peer='headset')
    mark = g.start('btctl pairable 20')
    g.wait(mark, 'pairable for 20 s')
    g.control('pairing-start', peer='headset', address=g.guest_address)
    g.wait(mark, 'Allow it? [y/n]')
    g.serial.type(b'y\n')
    accepted = g.eventually('pairing-result', lambda value: value is not None, peer='headset', timeout=35)
    g.require(accepted.get('ok') and accepted.get('encrypted'), f'consented incoming pairing failed: {accepted}')
    g.require('no longer pairable' in g.finish(mark), 'incoming prompt watcher did not close')
    headset = g.address('headset')
    g.command_eventually('btctl list', lambda text: any(headset in row.lower() and 'Secure Connections, Just Works' in row for row in text.splitlines()))
    g.record('classic-incoming-policy', unwatched=refused, watched='consent prompt then encrypted Secure Connections')

    pair(g, 'gamepad', 'P-192 Simple Pairing, authenticated', io='DISPLAY_OUTPUT_AND_YES_NO_INPUT', sc=False, required_prompt='compare')
    g.control('pairing-config', peer='serial', io='NO_OUTPUT_NO_INPUT', sc=False, mitm=False, ssp=False)
    g.command('btctl scan 5')
    refused_pair(g, 'serial', 'BluetoothService: a legacy PIN pairing was refused - the operator did not ask for one, or a better bond exists')
    g.require(g.control('status')['serial']['pairings'] == 0, 'ordinary pairing admitted legacy before explicit request')
    pair(g, 'serial', 'legacy PIN', io='NO_OUTPUT_NO_INPUT', sc=False, mitm=False, legacy=True)
    g.record('classic-legacy-consent', ordinary='refused', explicit='PIN prompt and legacy bond')
    g.command(f'btctl forget {g.address("serial")}', expect='is forgotten')
    g.control('disconnect', peer='serial')
    g.control('forget-key', peer='serial', address=g.guest_address)
    pair(g, 'serial', 'Secure Connections, Just Works', io='NO_OUTPUT_NO_INPUT', mitm=False)


def music_voice(g):
    headset, phone = g.address('headset'), g.address('phone')
    cursor = g.control('events')['next']
    g.command(f'btctl connect {headset} audio', expect='connecting audio')
    output, description = audio_device(g, lambda line: '(Bluetooth)' in line and 'out 48000' in line)
    peer_event(g, 'avdtp-delay', cursor, tenths_ms=1800)
    # Four 128-sample SBC frames at 48 kHz are the declared codec/queue term.
    _, description = audio_device(g, lambda line: line.startswith(f'device {output}:') and 'latency 190664 us' in line)
    g.require('latency 190664 us' in description, f'non-default 180 ms AVDTP delay is absent from the endpoint latency: {description}')
    g.command(f'audioctl default {output} output')
    g.command(f'audioctl volume {output} 100')
    g.command('audioprobe radio-music', expect='PASS radio-music')
    music = g.eventually('music-verdict', lambda value: value['samples'] >= 24000, peer='headset', timeout=15)
    g.require(music['rms'] > 1000 and music['peak_frequency'] == 1000, f'independent AOSP decoder did not hear 1 kHz music: {music}')
    g.record('classic-music', independent_decode=music, avdtp_delay_tenths_ms=1800, endpoint=description)

    # Absolute volume is observed independently in each direction.
    cursor = g.control('events')['next']
    g.command(f'audioctl volume {output} 60')
    peer_event(g, 'volume', cursor, value=76)
    g.control('volume', peer='headset', value=127)
    audio_device(g, lambda line: line.startswith(f'device {output}:') and 'level 100 ' in line)
    cursor = g.control('events')['next']
    g.command(f'btctl media {headset} pause')
    peer_event(g, 'media-key', cursor, key=0x46, pressed=True)

    # A separate independent A2DP source feeds the guest's route endpoint.
    g.control('music-reset', peer='headset')
    g.command(f'btctl connect {phone} audio', expect='connecting audio')
    g.control('source-start', peer='phone', address=g.guest_address)
    _, route = audio_device(g, lambda line: 'Bluetooth, a phone playing to this system' in line)
    g.require('48000' in route, 'phone route did not expose its negotiated SBC sample rate')
    g.pause(2)
    g.command('audioctl streams', expect="the phone's stream")
    routed = g.eventually('music-verdict', lambda value: value['samples'] >= 24000, peer='headset', timeout=15)
    g.require(routed['rms'] > 1000 and routed['peak_frequency'] == 1000, f'phone SBC did not pass through the guest mixer to the independent headset: {routed}')
    g.record('classic-phone-route', independent_decode=routed)
    cursor = g.control('events')['next']
    g.command(f'btctl media {phone} play')
    peer_event(g, 'media-key', cursor, peer='phone', key=0x44, pressed=True)
    g.control('media-state', peer='phone', state='PAUSED', track=2)
    for key in ('PLAY', 'PAUSE', 'FORWARD', 'BACKWARD'):
        response = g.control('key', peer='phone', key=key)
        g.require(response['pressed_code'] == 0x08 and response['released_code'] == 0x08,
                  f'guest acknowledged unsupported AVRCP {key} instead of NOT_IMPLEMENTED: {response}')
    g.record('classic-avrcp-target', keys=['PLAY', 'PAUSE', 'FORWARD', 'BACKWARD'], response='NOT_IMPLEMENTED for press and release')
    g.command(f'btctl disconnect {phone} audio', expect='disconnecting audio')

    # Both controller voice modes use the same application call contract. The
    # independent peer encodes a 1500 Hz microphone; the guest verifies its pitch.
    for codec in ('msbc', 'cvsd', 'hsp'):
        if codec == 'hsp':
            g.control('hsp-connect', peer='headset', address=g.guest_address)
        else:
            g.control('hfp-connect', peer='headset', address=g.guest_address, codec=codec)
        voice, description = audio_device(g, lambda line: 'Bluetooth, voice' in line)
        g.require(('16000' if codec == 'msbc' else '8000') in description, f'{codec} endpoint has the wrong PCM rate: {description}')
        g.command(f'audioctl default {voice} voice')
        g.command_eventually(f'audioctl microphone {voice}', lambda text: f'device {voice}: microphone level 67' in text)
        cursor = g.control('events')['next']
        g.command(f'audioctl volume {voice} 60')
        peer_event(g, 'hfp-volume', cursor, value=9)
        gain_notifications(g, cursor, [('hfp-volume', 9)])
        g.control('hfp-at', peer='headset', command='AT+VGS=15')
        audio_device(g, lambda line: line.startswith(f'device {voice}:') and 'level 100 ' in line)
        # The peer's real VGM must change the new public microphone control,
        # while the old speaker control remains unchanged in both directions.
        cursor = g.control('events')['next']
        g.control('hfp-at', peer='headset', command='AT+VGM=9')
        g.command_eventually(f'audioctl microphone {voice}', lambda text: f'device {voice}: microphone level 60' in text)
        gain_notifications(g, cursor, [])
        audio_device(g, lambda line: line.startswith(f'device {voice}:') and 'level 100 ' in line)
        cursor = g.control('events')['next']
        g.command(f'audioctl microphone {voice} 40', expect='done')
        peer_event(g, 'hfp-microphone-volume', cursor, value=6)
        gain_notifications(g, cursor, [('hfp-microphone-volume', 6)])
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 40')
        audio_device(g, lambda line: line.startswith(f'device {voice}:') and 'level 100 ' in line)
        cursor = g.control('events')['next']
        g.command(f'audioctl volume {voice} 60', expect='done')
        peer_event(g, 'hfp-volume', cursor, value=9)
        gain_notifications(g, cursor, [('hfp-volume', 9)])
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 40')
        g.control('hfp-at', peer='headset', command='AT+VGS=15')
        audio_device(g, lambda line: line.startswith(f'device {voice}:') and 'level 100 ' in line)
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 40')
        cursor = g.control('events')['next']
        refusal = g.control('hfp-expect-error', peer='headset', command='AT+VGM=16')
        g.require(refusal.get('status') == 'ERROR', 'invalid microphone gain did not produce the exact HFP ERROR')
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 40')
        gain_notifications(g, cursor, [])
        # Retain a non-default value across the supported application's voice
        # ownership and freshly minted control grants. AudioService itself has
        # restart=escalate; this is not a consumer restart/resnapshot assertion.
        g.control('hfp-at', peer='headset', command='AT+VGM=7')
        g.command_eventually(f'audioctl microphone {voice}', lambda text: f'device {voice}: microphone level 47' in text)
        cursor = g.control('events')['next']
        mark = g.start('audioprobe radio-voice-close')
        g.wait(mark, 'radio-voice-close waiting')
        g.event('sco-connected', 'headset', since=cursor, timeout=15)
        if codec == 'hsp':
            g.control('hfp-at', peer='headset', command='AT+CKPD=200')
        else:
            peer_event(g, 'hfp-indicator', cursor, name='call', value=1)
            g.control('hangup', peer='headset')
        g.require('PASS radio-voice-close' in g.finish(mark), 'the microphone-retention voice owner did not close')
        closed = g.event('sco-disconnected', 'headset', since=cursor, timeout=15)
        g.require(closed['reason'] == 0x13, f'voice-owner close did not explicitly end SCO: {closed}')
        g.require(not g.control('status')['headset']['sco'], 'closed voice owner left SCO alive')
        # Each audioctl process receives a fresh operator grant; no new VGM is
        # sent after the value above. Normal duplex below reopens the PCM endpoint.
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 47')
        audio_device(g, lambda line: line.startswith(f'device {voice}:') and 'level 100 ' in line)
        retained = g.control('events', since=cursor)['events']
        g.require(not any(item.get('peer') == 'headset' and item.get('event') in ('hfp-slc', 'hsp-connected', 'disconnection') for item in retained),
                  'microphone retention assertion recreated the headset connection')
        gain_notifications(g, cursor, [])
        g.command(f'audioctl default {voice} voice')
        # A nonrepresentable operator level is read back at the actual 0..15 gain.
        cursor = g.control('events')['next']
        g.command(f'audioctl microphone {voice} 50', expect='done')
        peer_event(g, 'hfp-microphone-volume', cursor, value=8)
        gain_notifications(g, cursor, [('hfp-microphone-volume', 8)])
        g.command(f'audioctl microphone {voice}', expect=f'device {voice}: microphone level 53')
        if codec == 'msbc':
            late_voice_completion(g)
        if codec == 'hsp':
            g.control('hfp-at', peer='headset', command='AT+CKPD=200')
            g.pause(.5)
            g.require(not g.control('status')['headset']['sco'], 'HSP button opened SCO without an application voice session')
        else:
            g.control('hfp-at', peer='headset', command='AT+BIEV=2,80')
            g.command_eventually('btctl list', lambda text: any(headset in row.lower() and 'battery 80%' in row for row in text.splitlines()))
            g.control('hfp-expect-error', peer='headset', command='ATA')
            g.control('hfp-expect-error', peer='headset', command='AT+BCC')
        cursor = g.control('events')['next']
        mark = g.start('audioprobe radio-voice')
        g.wait(mark, 'radio-voice incoming')
        peer_event(g, 'hfp-ring', cursor)
        if codec == 'hsp':
            g.control('hfp-at', peer='headset', command='AT+CKPD=200')
        else:
            g.control('answer', peer='headset')
        g.wait(mark, 'radio-voice answered')
        g.pause(2)
        voice_pcm = g.eventually('voice-verdict', lambda value: value['samples'] >= 8000, peer='headset', timeout=15)
        g.require(voice_pcm['rms'] > 500 and voice_pcm['peak_frequency'] == 333, f'independent {codec} peer did not hear the application voice tone: {voice_pcm}')
        if codec == 'hsp':
            g.control('hfp-at', peer='headset', command='AT+CKPD=200')
        else:
            g.control('hangup', peer='headset')
        completed = g.finish(mark)
        measured = re.search(r'PASS radio-voice samples (\d+) mean-square (\d+) frequency (\d+)', completed)
        g.require(measured is not None, f'{codec} duplex/call probe did not report actual microphone energy')
        reference = voice_pcm.get('microphone_reference')
        g.require(reference is not None and reference['samples'] >= 8000 and reference['mean_square'] > 1_000_000,
                  f'{codec} independent transmitted microphone reference is absent: {reference}')
        ratio = int(measured[2]) / reference['mean_square']
        g.require(.35 <= ratio <= 1.65,
                  f'{codec} capture was scaled again or corrupted (mic level53, energy ratio {ratio}, reference {reference})')
        peer_event(g, 'sco-disconnected', cursor)
        g.record('classic-voice-' + codec, independent_decode=voice_pcm,
                 no_session='HSP button creates no SCO' if codec == 'hsp' else 'ATA/BCC rejected',
                 calls='HSP button answer/hangup' if codec == 'hsp' else 'incoming/answer/active/hangup',
                 speaker_gain='operator60 -> peer9; peer15 -> operator100',
                 microphone_gain='peer9 -> API60; API40 -> peer6; API50 -> peer8/API53; exact ERROR for16; no gain cross-notifications; retained47 across actual voice-owner close and fresh control grants',
                 microphone_energy=dict(guest_mean_square=int(measured[2]), ratio=ratio, reference=reference),
                 battery=None if codec == 'hsp' else 80)
        g.control('disconnect', peer='headset')
        g.pause(1)


def transfer(g):
    phone = g.address('phone')
    outbound = 'independent-opp-payload-' * 48
    out_path, in_path = 'bt-oracle-out.txt', 'bt-oracle-in.txt'
    g.command(f'echo {outbound} > {out_path}')
    expected = (outbound + '\n').encode()
    receipts = []
    for goep in (False, True):
        offered = g.control('opp-offer-l2cap', peer='phone', enabled=goep)
        g.require(offered['goep_psm'] == (0x1001 if goep else None), 'peer SDP offer did not change')
        g.command(f'btctl send {phone} oracle-out.txt < {out_path}', expect='sent "oracle-out.txt"')
        received = g.control('opp-verdict', peer='phone')
        g.require(received['complete'] and received['name'] == 'oracle-out.txt'
                  and received['bytes'] == len(expected) and received['sha256'] == hashlib.sha256(expected).hexdigest(),
                  f'OPP peer did not receive the exact redirected object: {received}')
        g.require(received['transport'] == ('l2cap-ertm' if goep else 'rfcomm') and received['ertm'] == goep,
                  f'OPP used the wrong transport for the advertised record: {received}')
        receipts.append(received)
    g.control('opp-offer-l2cap', peer='phone', enabled=False)

    incoming = ('independent-bumble-object-' * 37 + '\n').encode()
    mark = g.start(f'btctl receive {phone} 4096 > {in_path}')
    g.wait(mark, 'waiting up to 180 seconds')
    sent = g.control('opp-send', peer='phone', address=g.guest_address, name='../../untrusted-name.txt', data=incoming.hex())
    g.require(sent['sha256'] == hashlib.sha256(incoming).hexdigest(), 'OPP sender digest changed')
    g.require(f'received {len(incoming)} bytes' in g.finish(mark), 'guest did not complete the consented OPP receive')
    contents = g.command(f'cat {in_path}')
    g.require(incoming.decode().rstrip('\n') in contents, 'the exact incoming OPP object did not reach its redirected file')
    counted = g.command(f'wc {in_path} json-min')
    g.require(re.search(r'"bytes"\s*:\s*' + str(len(incoming)) + r'\s*[,}]', counted) is not None,
              f'incoming OPP file contains missing or additional bytes: {counted}')
    g.record('classic-opp', outgoing=receipts, incoming=sent, peer='same-team OBEX over independent Bumble RFCOMM and L2CAP ERTM')

    denied = g.control('opp-send', peer='phone', address=g.guest_address, name='unconsented.txt', data=b'unconsented'.hex(), allow_error=True)
    g.require(not denied['ok'] and ('OPP RFCOMM channel 9 refused by peer' in denied.get('error', '')
                                   or 'OBEX refusal code ' in denied.get('error', '')),
              f'OPP without a waiting receiver had no protocol refusal: {denied}')
    mark = g.start(f'btctl receive {phone} 64 > {in_path}')
    g.wait(mark, 'waiting up to 180 seconds')
    bounded = g.control('opp-send', peer='phone', address=g.guest_address, name='too-big.txt', data=(b'b' * 512).hex(), allow_error=True)
    g.require(not bounded['ok'] and 'OBEX refusal code ' in bounded.get('error', ''), f'OPP declared-size bound did not refuse the object: {bounded}')
    refused = g.finish(mark)
    g.require('the object did not arrive whole' in refused, 'bounded receive did not report failure')
    g.record('classic-opp-consent-bound', unsolicited=denied['error'], oversized=bounded['error'])

    # DHCP and network responses come from dnsmasq/the isolated Linux stack. The
    # BNEP adapter only carries Ethernet frames and records counts/lease evidence.
    g.control('pan-start', peer='phone')
    before = g.command('ip').splitlines()
    cursor = g.control('events')['next']
    g.command(f'btctl connect {phone} pan', expect='connecting pan')
    peer_event(g, 'pan-setup', cursor, peer='phone')
    held = g.command('ip')
    g.require('10.94.0.' not in held and any(line in held for line in before if '): mac ' in line), 'PAN without replacement consent displaced the selected uplink')
    g.command(f'btctl disconnect {phone} pan', expect='disconnecting pan')
    g.pause(1)
    g.command(f'btctl connect {phone} pan replace', expect='allowed to replace the uplink')
    network = g.eventually('pan-verdict', lambda value: '10.94.0.' in value['leases'], peer='phone', timeout=40)
    assigned = re.search(r'\b10\.94\.0\.(?:1[0-9]|20)\b', network['leases'])
    g.require(assigned is not None, f'dnsmasq did not issue a lease in its configured pool: {network}')
    g.command('ip', expect=assigned[0])
    g.command('ping -c 3 10.94.0.1', expect='3 packets transmitted, 3 received, 0% packet loss')
    echoed = g.command('nc 10.94.0.1 194 independent-pan-tcp-echo', expect='nc 10.94.0.1: connected')
    g.require(any(line.strip() == 'independent-pan-tcp-echo' for line in echoed.splitlines()), 'independent Linux TCP peer did not echo the request')
    network = g.control('pan-verdict', peer='phone')
    g.require(network['frames_in'] > 10 and network['frames_out'] > 10, f'PAN traffic did not cross both directions: {network}')
    g.record('classic-pan', network=network, oracle='same-team BNEP; independent dnsmasq lease and Linux ICMP/TCP', replacement='held until operator allowed')
    g.command(f'btctl disconnect {phone} pan', expect='disconnecting pan')
    g.pause(1)
    g.require(assigned[0] not in g.command('ip'), 'PAN lease remained selected after its link was removed')
