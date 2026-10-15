#!/usr/bin/env python3
"""Exact-source xHCI wait/retention proof with memory-backed MMIO and output boundaries.

Preparation alone does not run this file. Explicit --run compiles the retained
original, candidate and causal mutants; a compiler failure is never a negative
proof. This does not establish real DMA timing or the cause of a historical guest
failure. No canonical source, driver, host USB module or guest is changed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


DRIVER = Path('src/user/drivers/core/src')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def item(source, anchor):
    """Extract one literal source item; braces in comments/strings are masked."""
    if source.count(anchor) != 1:
        raise ValueError('missing or ambiguous production anchor: ' + anchor)
    begin = source.index(anchor)
    # Preserve offsets. Selected Rust items contain no raw-string literals.
    masked = re.sub(r'//[^\n]*|/\*.*?\*/|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'',
                    lambda match: ' ' * len(match.group()), source, flags=re.S)
    opening = masked.index('{', begin)
    depth = 1
    end = opening + 1
    while depth and end < len(masked):
        depth += (masked[end] == '{') - (masked[end] == '}')
        end += 1
    if depth:
        raise ValueError('unterminated production item: ' + anchor)
    return source[begin:end]


def constant(source, name):
    found = list(re.finditer(r'(?m)^(?:pub(?:\([^)]*\))? )?const ' + re.escape(name) + r':[^\n]*;', source))
    if len(found) != 1:
        raise ValueError('missing or ambiguous constant: ' + name)
    return found[0].group()


def replace_once(source, before, after):
    if source.count(before) != 1:
        raise ValueError('missing or ambiguous causal mutation: ' + before)
    return source.replace(before, after, 1)


def production(root, overlay, *, candidate):
    pins = {}
    spans = []

    def read(relative):
        selected = overlay / relative
        if not selected.is_file():
            selected = root / relative
        pins[str(selected)] = digest(selected)
        return selected.read_text()

    def take(text, label, anchor):
        value = item(text, anchor)
        spans.append(dict(source=label, anchor=anchor, sha256=hashlib.sha256(value.encode()).hexdigest()))
        return value

    x = read(DRIVER / 'xhci.rs')
    h = read(DRIVER / 'usb_hid.rs')
    n = read(DRIVER / 'usb_net.rs')
    c = read(DRIVER / 'classes.rs')
    serial = read(DRIVER / 'usb_serial.rs')
    uac = read(DRIVER / 'uac.rs')
    modules = []
    for name in ('hid', 'cdc', 'descriptor', 'port', 'usb_class', 'uac'):
        relative = DRIVER / (name + '.rs')
        read(relative)
        path = overlay / relative if (overlay / relative).is_file() else root / relative
        modules.append(f'#[path = {json.dumps(str(path))}] mod {name};')
    relative = Path('src/user/libs/driver/protocol/src/gamepad.rs')
    read(relative)
    path = overlay / relative if (overlay / relative).is_file() else root / relative
    modules.append(f'#[path = {json.dumps(str(path))}] mod gamepad;')

    relative = Path('src/user/libs/driver/protocol/src/audio.rs')
    read(relative)
    path = overlay / relative if (overlay / relative).is_file() else root / relative
    modules.append(f'#[path = {json.dumps(str(path))}] mod audio;')
    usb_audio = read(DRIVER / 'usb_audio.rs')

    constants = ('IR_ERDP','ERDP_EHB','IR_IMAN','IMAN_IE','IMAN_IP','TRB_NORMAL','TRB_LINK','TRB_CYCLE','TRB_TOGGLE_CYCLE',
                 'TRB_IOC','TRB_EV_TRANSFER','TRB_EV_CMD_COMPLETE','TRB_EV_PORT_STATUS',
                 'CC_SUCCESS','CC_SHORT_PACKET','CC_STALL','RING_TRBS','SPIN_BUDGET',
                 'TRB_RESET_ENDPOINT','TRB_SET_TR_DEQUEUE','TRB_DISABLE_SLOT',
                 'TRB_SETUP','TRB_STATUS','TRB_IDT','TRB_DIR_IN',
                 'RT_ENDPOINT','REQ_CLEAR_FEATURE','FEATURE_ENDPOINT_HALT')
    chunks = [constant(x, name) for name in constants]
    chunks += ['mod usb_serial { ' + constant(serial, 'MAX_PORTS') + ' }']
    chunks += [take(x, 'xhci.rs', anchor) for anchor in (
        'struct Ring {','struct UsbDevice {','unsafe fn r8(', 'unsafe fn w32(', 'fn w64(',
        'fn command(', 'unsafe fn take_event(', 'fn wait_event(', 'fn command_and_wait(',
        'fn keep_stray(', 'fn wait_command(', 'fn wait_transfer_at(', 'fn wait_transfer_len(',
        'fn wait_transfer(', 'fn reset_endpoint(', 'fn recover_ep0(', 'fn control_nodata(')]
    chunks += ['impl Ring {\n' + '\n'.join(take(item(x, 'impl Ring {'), 'xhci.rs/Ring', anchor)
               for anchor in ('unsafe fn push(', 'fn release(')) + '\n}',
               'impl UsbDevice {\n' + take(item(x, 'impl UsbDevice {'), 'xhci.rs/UsbDevice', 'fn release(') + '\n}']
    chunks += [take(h, 'usb_hid.rs', anchor) for anchor in (
        'struct Page {', 'struct Pad {', 'pub struct Hid {', 'pub struct Hids {',
        'pub struct Pads {', 'impl gamepad::Sender for Sink {',
        'impl Pads {', 'fn post_report(', 'pub fn post_reports(',
        'pub fn handle_hid_event(', 'unsafe fn feed_hid_report(', 'pub fn depart(')]
    # The one tuple struct has no block.
    if h.count('struct Sink(u64);') != 1:
        raise ValueError('HID sender boundary changed')
    chunks += ['struct Sink(u64);', constant(h, 'MAX_PADS'),
               'impl Hids {\n' + take(item(h, 'impl Hids {'), 'usb_hid.rs/Hids', 'pub const fn new(') + '\n}',
               'impl Hid {\n' + take(item(h, 'impl Hid {'), 'usb_hid.rs/Hid', 'pub fn release(') + '\n}']
    chunks += [take(n, 'usb_net.rs', anchor) for anchor in (
        'struct Notify {', 'struct Ncm {', 'pub struct Net {', 'pub fn post_notification(',
        'pub fn notification_due(', 'pub fn handle_notification(')]
    chunks += [constant(n, 'NOTIFY_BYTES'), constant(n, 'NOTIFY_WINDOW'),
               'impl Net {\n' + '\n'.join(take(item(n, 'impl Net {'), 'usb_net.rs/Net', anchor)
                for anchor in ('pub fn notify_endpoint(', 'pub fn receive_endpoint(', 'pub fn release(')) + '\n}',
               'mod classes { use super::*;\n' + constant(c, 'MAX_KEPT') + '\n' + take(c, 'classes.rs', 'pub fn keep(') + '\n}']

    registration = 'hc.net_rx = Some((dev.slot, net.receive_endpoint()));'
    if candidate:
        registration += '\n' + 'hc.net_notify_rx = net.notify_endpoint().map(|dci| (dev.slot, dci));'
    compact = lambda text: re.sub(r'\s+', '', text)
    register_source = item(x, 'fn register_device(')
    if compact(registration) not in compact(register_source):
        raise ValueError('network registration is not the exact production sequence')
    begin = register_source.index('hc.net_rx = Some((dev.slot, net.receive_endpoint()));')
    last = registration.splitlines()[-1]
    end = register_source.index(last, begin) + len(last)
    registration = register_source[begin:end]
    spans.append(dict(source='xhci.rs', anchor='network owner registration block', sha256=hashlib.sha256(registration.encode()).hexdigest()))
    chunks.append('fn register_network(hc: &mut Xhci, dev: &UsbDevice, net: &Net) {\n' + registration + '\n}')

    detach = item(x, 'unsafe fn detach_port_devices(')
    start = detach.index('let Hids { entries, pads } = hids;')
    end_anchor = 'entries.retain(|(dev, _)| dev.port != port);'
    end = detach.index(end_anchor, start) + len(end_anchor)
    detach_hid = detach[start:end]
    detach_net = item(detach, 'if let Some((dev, net)) = network.as_mut()')
    chunks.append('unsafe fn detach_registered(hc: &mut Xhci, hids: &mut Hids, network: &mut Option<(UsbDevice, Net)>, port: u32) {\n' + detach_hid + '\n' + detach_net + '\n}')
    spans += [dict(source='xhci.rs', anchor='detach HID block', sha256=hashlib.sha256(detach_hid.encode()).hexdigest()),
              dict(source='xhci.rs', anchor='detach network block', sha256=hashlib.sha256(detach_net.encode()).hexdigest())]

    pre_sleep = ''
    if candidate:
        chunks += [take(x, 'xhci.rs', anchor) for anchor in (
            'fn keep_hid(', 'fn drain_hid_pending(', 'fn handle_net_notification(')]
        service = item(x, 'fn service_loop(')
        start = service.index('drain_hid_pending(hc, &mut hids);')
        end = service.index('hids.pads.flush();', start)
        pre_sleep = service[start:end]
        tail = service[end:service.index('common::wait_providers_or_sleep(', end)]
        if 'drain_hid_pending(' in tail or 'take_event(' in tail:
            raise ValueError('pending completion drain no longer immediately precedes the wait preparation')
        if service.count('handle_net_notification(hc, dev, net, status, control)') != 2:
            raise ValueError('raw-ring and retained CDC paths must call the same production consumer')
        spans.append(dict(source='xhci.rs', anchor='pre-sleep drain block', sha256=hashlib.sha256(pre_sleep.encode()).hexdigest()))
    # Keep the production `continue` intact. The fixture stops at the next outer
    # service iteration boundary instead of substituting a successful consumer.
    chunks.append('fn pre_sleep(hc: &mut Xhci, mut hids: &mut Hids, network: &mut Option<(UsbDevice, Net)>) -> bool {\n'
                  'let mut entered = false; loop { if entered { return false; } entered = true;\n'
                  + pre_sleep + '\nreturn true; }\n}')
    # The actual playback queue, pacer and ring writer run against allocated memory.
    # Only the IPC head/receive and output syscalls are fixture boundaries.
    audio_chunks = [constant(usb_audio, name) for name in (
        'PLAYBACK_QUEUE', 'PLAYBACK_START', 'TRB_START_ISOCH_ASAP', 'TRB_ISOCH',
        'CC_RING_UNDERRUN', 'CC_RING_OVERRUN', 'SPEED_HIGH')]
    audio_chunks += [take(usb_audio, 'usb_audio.rs', anchor) for anchor in (
        'struct FeedbackPipe {', 'pub struct Sink {', 'pub struct Source {',
        'pub struct Audio {', 'pub struct Devices {', 'fn frame_bytes(',
        'unsafe fn post_playback(', 'fn answer_play(', 'pub unsafe fn play(',
        'pub unsafe fn end_play(', 'unsafe fn absorb_sink(', 'impl Audio {')]
    if 'pub unsafe fn request_before_fill(' in usb_audio:
        # No capture device is present in the playback fixture, and production answers false at
        # `devices.source()` before it reads the consumer's channel; this is that answer.
        audio_chunks.append('pub unsafe fn request_before_fill(_: &mut Xhci, _: &mut Hids, _: &mut Devices, _: u64, _: u32) -> bool { false }')
    if 'pub unsafe fn play_before_refill(' in usb_audio:
        audio_chunks.append(take(usb_audio, 'usb_audio.rs', 'pub unsafe fn play_before_refill('))
    else:
        # Retained pre-fix source has no late-head operation. This neutral stub is
        # used only by boundary cases; the original raw dispatch never calls it.
        audio_chunks.append('pub unsafe fn play_before_refill(_: &mut Xhci, _: &mut Devices, _: u64, _: u32) -> bool { false }')
    absorb = item(usb_audio, 'pub unsafe fn absorb(')
    begin = absorb.index('if let Some((dev, sink)) = devices.sink()')
    end = absorb.index('let Some((dev, source)) = devices.source()', begin)
    sink_dispatch = absorb[begin:end]
    spans.append(dict(source='usb_audio.rs', anchor='literal sink completion dispatch',
                      sha256=hashlib.sha256(sink_dispatch.encode()).hexdigest()))
    # No capture device is present in the playback fixture. Its sink dispatch and
    # absorb_sink remain literal production code; absent capture returns false.
    audio_chunks.append('pub unsafe fn absorb(hc: &mut Xhci, devices: &mut Devices, pointer: u64, status: u32, control: u32) -> bool { unsafe {\n'
                        + sink_dispatch + '\nfalse } }')
    audio_chunks.append('impl Devices {\n' + take(item(usb_audio, 'impl Devices {'),
        'usb_audio.rs/Devices', 'pub fn sink(') + '\n}')
    chunks.append('mod usb_audio { use super::*; use alloc::collections::VecDeque;\n'
                  + '\n'.join(audio_chunks) + '\n/* AUDIO_CASES */\n}')

    service = item(x, 'fn service_loop(')
    ready_play = ''
    anchor = 'let mut played_audio = false;'
    if anchor in service:
        begin = service.index(anchor)
        end = service.index('// THE WAKE IS ACKNOWLEDGED BEFORE THE RING IS READ', begin)
        ready_play = service[begin:end]
        irq = service.index('interrupt_ack(irq);', end)
        drain_anchor = 'for _ in 0..RING_TRBS {' if 'for _ in 0..RING_TRBS {' in service[irq:] else 'while let Some((pointer, status, control)) = take_event(hc)'
        drain = service.index(drain_anchor, irq)
        late = service.index('let common::ProviderReady::Consumer(at) = ready else { continue };', drain)
        skip = 'if played_audio {\n\t\t\t\tcontinue;\n\t\t\t}'
        if compact(skip) not in compact(service[drain:late]):
            raise ValueError('pre-served audio must skip the later consumer dispatch')
        spans.append(dict(source='xhci.rs', anchor='ready audio before IRQ drain',
                          sha256=hashlib.sha256(ready_play.encode()).hexdigest()))
    else:
        ready_play = 'let played_audio = false;'
    chunks.append('unsafe fn ready_audio(hc: &mut Xhci, mut audio: &mut usb_audio::Devices, '
                  'serving: &common::Serving, ready: common::ProviderReady) -> bool { unsafe {\n'
                  + ready_play + '\nplayed_audio } }')
    audio_end = service.index('// AND THE CLASS MODULES\'', service.index('let mut played_audio = false;')) if 'let mut played_audio = false;' in service else service.index('// AND THE CLASS MODULES\'', service.index('// THE AUDIO SOURCE\'S STANDING CAPTURE.'))
    audio_begin = service.rfind('if control >> 10 & 0x3f == TRB_EV_TRANSFER', 0, audio_end)
    raw_audio = service[audio_begin:audio_end].strip()
    spans.append(dict(source='xhci.rs', anchor='literal raw audio event dispatch',
                      sha256=hashlib.sha256(raw_audio.encode()).hexdigest()))
    chunks.append('unsafe fn audio_completion(hc: &mut Xhci, mut audio: &mut usb_audio::Devices, '
        'serving: &common::Serving, ready: common::ProviderReady, pointer: u64, status: u32, control: u32) -> bool { unsafe {\n'
        + ('let mut hids = Hids::new();\n' if 'request_before_fill(' in raw_audio else '')
        + 'let mut played_audio = false; let mut entered = false; loop { if entered { break; } entered = true;\n'
        + raw_audio + '\nbreak; } played_audio } }')
    # Preserve the entire production dispatch body. Only device/MMIO producer and
    # absent class backends are modeled by SERVICE_DRAIN_MODELS in the fixture.
    if candidate:
        irq_line = 'w32(hc.ir0 + IR_IMAN, IMAN_IE | IMAN_IP);'
        ack_line = 'interrupt_ack(irq);'
        bounded = 'for _ in 0..RING_TRBS {'
        unbounded = 'while let Some((pointer, status, control)) = take_event(hc) {'
        begin = service.index(bounded if bounded in service else unbounded)
        drain_block = item(service, bounded if bounded in service else unbounded)
        irq_at = service.index(irq_line)
        ack_at = service.index(ack_line, irq_at)
        final_line = 'w64(hc.ir0 + IR_ERDP, hc.evt_phys + hc.evt_index * 16 | ERDP_EHB);'
        final_at = service.index(final_line, begin + len(drain_block))
        rescan_line = 'if rescan || hc.ports_changed.take() {'
        rescan_at = service.index(rescan_line, final_at)
        consumer_line = 'let common::ProviderReady::Consumer(at) = ready else { continue };'
        consumer_at = service.index(consumer_line, rescan_at)
        if not irq_at < ack_at < begin < final_at < rescan_at < consumer_at:
            raise ValueError('service IRQ/drain/EHB/rescan/consumer order changed')
        for label, value in (('service complete event dispatch loop', drain_block),
                             ('service IRQ and EHB/consumer order', service[irq_at:consumer_at + len(consumer_line)])):
            spans.append(dict(source='xhci.rs', anchor=label, sha256=hashlib.sha256(value.encode()).hexdigest()))
        service_code = '\n'.join((irq_line, ack_line, 'let mut rescan = false;', drain_block,
                                   final_line, 'let rescanned = rescan || hc.ports_changed.take();',
                                   consumer_line, 'return (at, rescanned);'))
        chunks.append('#[cfg(candidate)] mod service_drain { use super::*;\n/* SERVICE_DRAIN_MODELS */\n'
            'unsafe fn one_pass(hc: &mut Xhci, classes: &mut RefillingClass) -> (usize, bool) { unsafe {\n'
            'let irq = 99; let bootstrap = 0; let bind = &();\n'
            'let mut uas: Option<(UsbDevice, Never)> = None;\n'
            'let mut network: Option<(UsbDevice, Net)> = None;\n'
            'let mut serials = EmptySerials; let mut sessions: Vec<Never> = Vec::new();\n'
            'let mut buffers: Vec<Vec<u8>> = Vec::new(); let mut frame: Vec<u8> = Vec::new(); let mut received = Received::default();\n'
            'let mut hids = Hids::new(); let mut audio = (); let mut serving = Serving; let mut played_audio = false;\n'
            'let mut entered = false; loop { assert!(!entered); entered = true;\n'
            'let ready = common::ProviderReady::Consumer(0);\n' + service_code + '\n} } }\n}')
    return '\n\n'.join(chunks), '\n'.join(modules), pins, spans


def run(args):
    root, candidate, output = (path.resolve() for path in (args.root, args.candidate_tree, args.output))
    original = args.original_tree.resolve() if args.original_tree else None
    if output.exists():
        raise ValueError('fresh output required; preserve earlier proof evidence')
    fixture_path = Path(__file__).with_name('xhci-event-retention.rs')
    fixture = fixture_path.read_text()
    if fixture.count('/* EXACT_PRODUCTION */') != 1 or fixture.count('/* EXACT_MODULES */') != 1:
        raise ValueError('fixture has missing or ambiguous exact-source insertion')
    begin = fixture.index('/* AUDIO_CASES_BEGIN */')
    end = fixture.index('/* AUDIO_CASES_END */', begin)
    audio_cases = fixture[begin + len('/* AUDIO_CASES_BEGIN */'):end]
    fixture = fixture[:begin] + fixture[end + len('/* AUDIO_CASES_END */'):]
    begin = fixture.index('/* SERVICE_DRAIN_MODELS_BEGIN */')
    end = fixture.index('/* SERVICE_DRAIN_MODELS_END */', begin)
    service_models = fixture[begin + len('/* SERVICE_DRAIN_MODELS_BEGIN */'):end]
    fixture = fixture[:begin] + fixture[end + len('/* SERVICE_DRAIN_MODELS_END */'):]
    old_pins, old_spans = {}, []
    cases = []
    if original is not None:
        old, old_modules, old_pins, old_spans = production(root, original, candidate=False)
        cases += [
            ('original-cdc', old, old_modules, False, 'command-cdc', 'CDC completion must not fail the command'),
            ('original-hid', old, old_modules, False, 'command-hid', 'HID completion must not fail the command'),
        ]
    new, new_modules, new_pins, new_spans = production(root, candidate, candidate=True)
    audio_old_pins, audio_old_spans = {}, []
    if args.audio_original_tree:
        old_audio, old_audio_modules, audio_old_pins, audio_old_spans = production(
            root, args.audio_original_tree.resolve(), candidate=True)
        cases.append(('original-audio-order', old_audio, old_audio_modules, True,
                      'audio-ready', 'a queued period must precede silent refills'))
    tail_old_pins, tail_old_spans = {}, []
    if args.audio_tail_original_tree:
        old_tail, old_tail_modules, tail_old_pins, tail_old_spans = production(
            root, args.audio_tail_original_tree.resolve(), candidate=True)
        cases.append(('original-audio-tail', old_tail, old_tail_modules, True,
                      'audio-tail', 'an ending packet keeps its paced length'))
    service_old_pins, service_old_spans = {}, []
    if args.service_original_tree:
        old_service, old_service_modules, service_old_pins, service_old_spans = production(
            root, args.service_original_tree.resolve(), candidate=True)
        cases.append(('original-service-drain', old_service, old_service_modules, True,
                      'service-drain', 'selected consumer must follow at most one ring'))
    late_old_pins, late_old_spans = {}, []
    if args.audio_late_original_tree:
        old_late, old_late_modules, late_old_pins, late_old_spans = production(
            root, args.audio_late_original_tree.resolve(), candidate=True)
        cases.append(('original-audio-late', old_late, old_late_modules, True,
                      'audio-late', 'late credited PCM must precede the next silent refill'))
    frozen = {**old_pins, **new_pins, **audio_old_pins, **tail_old_pins, **service_old_pins, **late_old_pins,
              str(fixture_path): digest(fixture_path), str(Path(__file__).resolve()): digest(Path(__file__))}
    # Both reviewed rustfmt spellings retain the same nested-recovery branch.
    nested_skip_anchors = (
        '|| hc.net_notify_pending.is_some()\n\t\t\t{\n\t\t\t\tcontinue;',
        '|| hc.net_notify_pending.is_some() {\n\t\t\t\tcontinue;',
    )
    if sum(new.count(anchor) for anchor in nested_skip_anchors) != 1:
        raise ValueError('nested-recovery mutation needs exactly one supported source spelling')
    nested_skip = next(anchor for anchor in nested_skip_anchors if anchor in new)
    cases += [
        ('candidate', new, new_modules, True, 'all', None),
        ('drop-cdc-payload', replace_once(new, 'hc.net_notify_pending = Some((status, control));',
            'let _ = (status, control);'), new_modules, True, 'command-cdc', 'CDC completion payload must be retained'),
        ('drop-hid-payload', replace_once(new, 'hc.hid_pending.push((status, control));',
            'let _ = (status, control);'), new_modules, True, 'command-hid', 'HID completion payload must be retained'),
        ('skip-pre-sleep-drain', replace_once(new, 'drain_hid_pending(hc, &mut hids);',
            'let _ = &mut hids;'), new_modules, True, 'command-hid', 'HID report must be consumed before sleep'),
        ('keep-detached-hid', replace_once(new, 'hc.hid_pending.retain(|&(_, control)| control >> 24 != dev.slot);',
            'let _ = dev.slot;'), new_modules, True, 'detach', 'detached HID completion must not survive slot reuse'),
        ('sleep-after-nested-recovery', replace_once(new,
            nested_skip, nested_skip.replace('continue;', 'let _ = hc;')),
            new_modules, True, 'nested-stall', 'nested recovery completions must prevent sleep'),
    ]
    cases += [
        ('candidate-audio-order', new, new_modules, True, 'audio-ready', None),
        ('candidate-audio-boundaries', new, new_modules, True, 'audio-boundaries', None),
        ('delay-ready-audio', replace_once(new,
            '&& channel_peek(serving.at(at)) == i64::from(audio::PERIOD_BYTES)',
            '&& false && channel_peek(serving.at(at)) == i64::from(audio::PERIOD_BYTES)'),
            new_modules, True, 'audio-ready', 'a queued period must precede silent refills'),
    ]
    cases += [
        ('candidate-audio-tail', new, new_modules, True, 'audio-tail', None),
        ('short-final-audio-packet', replace_once(new,
            'let length = bytes;', 'let length = if sink.ending { taken } else { bytes };'),
            new_modules, True, 'audio-tail', 'an ending packet keeps its paced length'),
        ('count-final-audio-padding', replace_once(new,
            'if !sink.ending {\n\t\t\t\t\tif !sink.dry', 'if true {\n\t\t\t\t\tif !sink.dry'),
            new_modules, True, 'audio-tail', 'intentional end padding is not an underrun'),
        ('leave-final-audio-padding', replace_once(new,
            'core::ptr::write_bytes((at + taken as u64) as *mut u8, 0, bytes - taken);',
            'core::ptr::write_bytes((at + taken as u64) as *mut u8, 0, if sink.ending { 0 } else { bytes - taken });'),
            new_modules, True, 'audio-tail', 'ending DMA preserves every PCM byte and zero-fills only final tail'),
    ]
    cases += [
        ('candidate-service-drain', new, new_modules, True, 'service-drain', None),
        ('unbounded-service-drain', replace_once(new,
            'for _ in 0..RING_TRBS {\n\t\t\t\tlet Some((pointer, status, control)) = take_event(hc) else { break };',
            'while let Some((pointer, status, control)) = take_event(hc) {'),
            new_modules, True, 'service-drain', 'selected consumer must follow at most one ring'),
        ('late-service-irq-ack', replace_once(new, 'interrupt_ack(irq);', 'let _ = irq;'),
            new_modules, True, 'service-drain', 'event drain must follow IRQ acknowledgment'),
        ('skip-empty-service-ehb', replace_once(new,
            'w64(hc.ir0 + IR_ERDP, hc.evt_phys + hc.evt_index * 16 | ERDP_EHB);\nlet rescanned',
            'let _ = hc;\nlet rescanned'),
            new_modules, True, 'service-drain', 'empty wake must still clear EHB'),
    ]
    cases += [
        ('candidate-audio-late', new, new_modules, True, 'audio-late', None),
        ('candidate-audio-late-boundaries', new, new_modules, True, 'audio-late-boundaries', None),
        ('delay-late-audio', replace_once(new,
            '|| channel_peek(server) != i64::from(audio::PERIOD_BYTES)',
            '|| true || channel_peek(server) != i64::from(audio::PERIOD_BYTES)'),
            new_modules, True, 'audio-late', 'late credited PCM must precede the next silent refill'),
    ]
    if args.audio_late_only:
        cases = [case for case in cases if case[4] in ('audio-late', 'audio-late-boundaries')]
    if args.audio_order_only:
        cases = [case for case in cases if case[4] in ('audio-ready', 'audio-boundaries')]
    if args.audio_tail_only:
        cases = [case for case in cases if case[4] == 'audio-tail']
    if args.service_drain_only:
        cases = [case for case in cases if case[4] == 'service-drain']
    output.mkdir(parents=True)
    (output / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    (output / 'extracted-spans.json').write_text(json.dumps(dict(original=old_spans, audio_original=audio_old_spans, tail_original=tail_old_spans, service_original=service_old_spans, late_original=late_old_spans, candidate=new_spans), indent=2) + '\n')
    results = []
    rustc = os.environ.get('RUSTC', 'rustc')
    for label, body, modules, cfg, selected, expected in cases:
        rust, binary = output / (label + '.rs'), output / label
        rust.write_text(fixture.replace('/* EXACT_PRODUCTION */', body).replace('/* EXACT_MODULES */', modules).replace('/* AUDIO_CASES */', audio_cases).replace('/* SERVICE_DRAIN_MODELS */', service_models))
        command = [rustc, '--edition=2024', '--crate-name', 'xhci_event_retention', str(rust), '-o', str(binary)]
        if cfg:
            command += ['--cfg', 'candidate']
        compiled = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=120)
        (output / (label + '-compile.log')).write_text(json.dumps(command) + '\nexit: ' + str(compiled.returncode) + '\n' + compiled.stdout + compiled.stderr)
        if compiled.returncode:
            raise ValueError(label + ': compilation failed; this is not causal evidence')
        actual = subprocess.run([str(binary), selected], cwd=root, capture_output=True, text=True, timeout=20)
        (output / (label + '.log')).write_text('exit: ' + str(actual.returncode) + '\n' + actual.stdout + actual.stderr)
        if expected is None:
            marker = {'all': 'xhci-event-retention: PASS 9 groups', 'audio-ready': 'xhci-audio-order: PASS ready period', 'audio-boundaries': 'xhci-audio-order: PASS boundaries', 'audio-tail': 'xhci-audio-tail: PASS one period, zero tail, idle and live underrun', 'audio-late': 'xhci-audio-late: PASS late arrival, FS/HS and exact DMA', 'audio-late-boundaries': 'xhci-audio-late: PASS credit, control, endpoint and closure boundaries', 'service-drain': 'xhci-service-drain: PASS finite batch, FIFO, rescan and IRQ/EHB'}[selected]
            if actual.returncode or marker not in actual.stdout:
                raise ValueError(label + ': exact-source positive proof failed')
        elif actual.returncode != 101 or expected not in actual.stderr:
            raise ValueError(label + ': expected named causal assertion did not fail')
        results.append(dict(label=label, compile_exit=compiled.returncode, run_exit=actual.returncode,
                            expected_assertion=expected, verdict='PASS' if expected is None else 'EXPECTED_CAUSAL_FAILURE'))
        print('xhci-event-retention: ' + label + ': ' + results[-1]['verdict'], flush=True)
    if any(digest(Path(path)) != expected for path, expected in frozen.items()):
        raise ValueError('source or fixture changed during proof')
    (output / 'result.json').write_text(json.dumps(dict(status='EXACT_SOURCE_PROOF_PASS', cases=results,
        source_inputs=frozen, scope='Memory-backed event/command/report/audio rings; real production waits, retention, playback queue and consumers. Clock, IPC readiness/receive, absent class backends and the replenishing event producer are modeled. The literal service dispatch loop and real event-ring dequeue/port signal/MMIO writes run unchanged; bounded progress is not proof of the sole cause of the historical RISC UVC failure. The initial-ready and late-arrival schedules prove avoidable padding, not the cause of a historical guest speaker failure or real-time performance.'), indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--original-tree', type=Path, help='optional retained original for staged causal negatives')
    parser.add_argument('--audio-original-tree', type=Path, help='optional retained pre-fix audio ordering for its exact-source causal negative')
    parser.add_argument('--audio-tail-original-tree', type=Path, help='optional retained short final packet for its exact-source causal negative')
    parser.add_argument('--audio-order-only', action='store_true', help='only the focused audio ordering and unchanged-command checks')
    parser.add_argument('--audio-tail-only', action='store_true', help='only the focused final packet and ending-stream checks')
    parser.add_argument('--service-original-tree', type=Path, help='optional retained unbounded event drain for a causal negative')
    parser.add_argument('--service-drain-only', action='store_true', help='only the bounded service-drain proof and causal mutations')
    parser.add_argument('--audio-late-original-tree', type=Path, help='optional retained pre-fix raw event dispatch for the late-arrival negative')
    parser.add_argument('--audio-late-only', action='store_true', help='only late PCM arrival, credit/command boundaries and causal mutant')
    parser.add_argument('--candidate-tree', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--run', action='store_true')
    args = parser.parse_args()
    if sum((args.audio_order_only, args.audio_tail_only, args.service_drain_only, args.audio_late_only)) > 1:
        parser.error('choose one focused selection')
    if not args.run:
        print('HOLD: private exact-source proof is prepared only; explicit admitted --run required')
        return
    run(args)


if __name__ == '__main__':
    main()
