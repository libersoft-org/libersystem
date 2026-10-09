#!/usr/bin/env python3
"""Prepare pinned host-only P02M0194 dependencies; never install into the guest image."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / '.build/bluetooth-oracles'
HERE = Path(__file__).resolve().parent


def run(*args, **kwargs):
    subprocess.run(list(map(str, args)), check=True, **kwargs)


def sources(offline):
    manifest = json.loads((HERE / 'bluetooth-oracles-sources.json').read_text())
    for name, item in manifest.items():
        archive = WORK / f'{name}.tar.gz'
        if not archive.exists():
            if offline:
                raise RuntimeError(f'{name}: run src/harness/prepare-bluetooth-oracles.py --only codecs once to prepare the pinned dependency')
            with urllib.request.urlopen(item['url'], timeout=60) as response:
                data = response.read()
            if hashlib.sha256(data).hexdigest() != item['sha256']:
                raise RuntimeError(f'{name}: downloaded archive digest differs from the pin')
            archive.write_bytes(data)
        data = archive.read_bytes()
        if hashlib.sha256(data).hexdigest() != item['sha256']:
            raise RuntimeError(f'{name}: cached archive digest differs from the pin')
        destination = WORK / name
        # Extract the verified sources on every preparation, so editing a checkout
        # cannot silently change the oracle. Extra files are not compilation inputs.
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as bundle:
            for member in bundle:
                if name != 'aosp-sbc':
                    parts = member.name.split('/', 1)
                    if len(parts) != 2 or not parts[1]:
                        continue
                    member.name = parts[1]
                bundle.extract(member, destination, filter='data')
        print(f'bluetooth-oracles: {name} {item["revision"]} SHA256 {item["sha256"]}', flush=True)
    (WORK / 'source-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def codecs(offline):
    sources(offline)
    run('make', '-C', WORK / 'liblc3', '-j4', 'LC3_PLUS=0', 'LC3_PLUS_HR=0', 'bin/liblc3.so')
    for role in ('encoder', 'decoder'):
        directory = WORK / 'aosp-sbc' / role
        run('cc', '-O2', '-shared', '-fPIC', '-I', directory / 'include', ROOT / f'src/tools/bluetooth-sbc-{role}.c', *sorted((directory / 'srce').glob('*.c')), '-o', WORK / f'sbc-{role}.so')
    run('cc', '-O2', '-shared', '-fPIC', '-I', WORK / 'libsbc/include', ROOT / 'src/tools/bluetooth-sbc-google.c', *sorted((WORK / 'libsbc/src').glob('*.c')), '-o', WORK / 'google-sbc.so')
    run('cargo', 'build', '--offline', '--release', '--manifest-path', ROOT / 'src/user/services/logic/Cargo.toml', '--target-dir', WORK / 'rust')
    run('rustc', '--edition', '2024', '-O', ROOT / 'src/tools/bluetooth-codec-host.rs', '--extern', f'service_logic={WORK}/rust/release/libservice_logic.rlib', '-L', f'dependency={WORK}/rust/release/deps', '-o', WORK / 'codec-host')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--only', choices=('codecs', 'radio', 'all'), default='all')
    parser.add_argument('--offline', action='store_true', help='use verified cached archives; do not access the network')
    args = parser.parse_args()
    WORK.mkdir(parents=True, exist_ok=True)
    if args.only in ('radio', 'all'):
        if args.offline:
            parser.error('--offline is supported for --only codecs')
        run('python3', '-m', 'venv', WORK / 'venv')
        run(WORK / 'venv/bin/pip', 'install', '-r', HERE / 'bluetooth-oracles-requirements.txt')
    if args.only in ('codecs', 'all'):
        codecs(args.offline)


if __name__ == '__main__':
    main()
