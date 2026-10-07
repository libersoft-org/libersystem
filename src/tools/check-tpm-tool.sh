#!/bin/bash
# THE TPM FOR APPLICATIONS, cold, in a development image: `swtpm` behind QEMU's CRB front-end, and then behind its
# TIS front-end in a second run - the platform row the `TPM2` table makes, the TPM driver bound to it, TpmService,
# and the shipping `tpm` tool and the development probe driving them through the emulated keyboard.
#
# WHAT THE SCENARIO SHOWS, in its own order: the row (one 4 KiB page at 0xFED40000, no interrupt), `tpm info`, two
# draws, an extend of PCR 16 read around, PCR 8 refused and unchanged, a secret sealed and opened, the probe's
# refusals - `not-granted` twice on PCR 23 and `other-component` on the tool's secret - and its own secret opened, the
# driver disabled and enabled under a held connection, TpmService stopped and started, PCR 16 moved so the secret
# stays shut, and a quote. WHAT THIS SCRIPT THEN CHECKS ON THE HOST, from the serial log: the two draws differ, PCR
# 16's new value is SHA-256 of its old value and the SHA-256 of the typed text, PCR 8 did not move, and OpenSSL
# verifies the quote's signature by the public point it carries, over an attestation that names this nonce and
# carries the digest of PCR 16 as it was read just before.
#
# THE DEVICE-TREE PORTS, `--arch aarch64` or `--arch riscv64`: the same scenario once, behind `tpm-tis-device`, whose
# tree node is the row - `tcg,tpm-tis-mmio`, its one resource the 4 KiB page at the node's translated base - and the
# same host checks.
#
# WHAT IT DOES NOT CLAIM: a TPM on real hardware.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
fail() {
	echo "qemu-tpm-tool: $*" >&2
	exit 1
}

arch=x86_64
case "${1:-}" in
"") ;;
--arch) arch="${2:-}" ;;
*) fail "unexpected argument '$1'" ;;
esac
# THE ROW AS `lsdev` PRINTS IT, in the scenario's two halves, and the identity the policy verbs name it by.
case "$arch" in
x86_64)
	row_head='identity=table:TPM2#0, ids=[{kind=table, text=TPM2}, {kind=identity, text=acpi:\_SB_.'
	row_tail='TPM_}, {kind=hid, text=MSFT0101}], resources=[{kind=mmio, base=4275306496, length=4096, level=false, active-low=false, controller=4294967295}]'
	identity='table:TPM2#0'
	;;
aarch64 | riscv64)
	base=0xc000000
	[[ "$arch" == riscv64 ]] && base=0x4000000
	identity="dt:/platform-bus@${base#0x}/tpm_tis@0"
	row_head="identity=$identity, ids=[{kind=compatible, text=tcg,tpm-tis-mmio}]"
	row_tail="resources=[{kind=mmio, base=$((base)), length=4096, level=false, active-low=false, controller=4294967295}]"
	;;
*) fail "--arch takes x86_64, aarch64 or riscv64" ;;
esac

command -v swtpm >/dev/null || fail "swtpm is not installed - setup.sh installs it, and this gate fails rather than skips without it"
command -v openssl >/dev/null || fail "openssl is not installed"

log="$repo/.build/boot/cold-$arch.log"
work="$(mktemp -d "${TMPDIR:-/tmp}/liber-tpm-tool.XXXXXX")"
tpm_pid=""
cleanup() {
	if [[ -n "$tpm_pid" ]]; then
		kill "$tpm_pid" 2>/dev/null || true
		wait "$tpm_pid" 2>/dev/null || true
	fi
	[[ "$work" == */liber-tpm-tool.* ]] && rm -rf -- "$work"
}
trap cleanup EXIT

# One run of the scenario behind `frontend`, then what the host checks of its log.
run() {
	local frontend="$1" interface="$2"
	local state="$work/$frontend"
	mkdir -p "$state"
	swtpm socket --tpm2 --tpmstate "dir=$state" --ctrl "type=unixio,path=$state/swtpm.sock" --log "file=$state/swtpm.log" &
	tpm_pid="$!"
	for _ in $(seq 1 100); do
		[[ -S "$state/swtpm.sock" ]] && break
		sleep 0.05
	done
	[[ -S "$state/swtpm.sock" ]] || fail "swtpm did not open its control socket"
	python3 - "$root/harness/scenarios/tpm-tool.toml" "$state/tpm-tool.toml" "$interface" "$row_head" "$row_tail" "$identity" <<'FILL'
import sys
source, copy, interface, head, tail, identity = sys.argv[1:]
text = open(source, encoding='utf-8').read()
for name, value in (('@IFACE@', interface), ('@ROW_HEAD@', head), ('@ROW_TAIL@', tail), ('@IDENTITY@', identity)):
    text = text.replace(name, value)
open(copy, 'w', encoding='utf-8').write(text)
FILL
	TPM_SOCKET="$state/swtpm.sock" TPM_FRONTEND="$frontend" "$repo/lab.sh" scenario-cold "$arch" "$state/tpm-tool.toml" || fail "the $frontend scenario failed (serial log: $log; swtpm log: $state/swtpm.log)"
	[[ -f "$log" ]] || fail "the $frontend scenario left no serial log at $log"
	cp "$log" "$state/serial.log"
	kill "$tpm_pid" 2>/dev/null || true
	wait "$tpm_pid" 2>/dev/null || true
	tpm_pid=""
	python3 - "$state/serial.log" "$state" "$frontend" <<'EOF' || fail "the $frontend run's TPM answers do not hold on the host"
import hashlib
import re
import subprocess
import sys

log_path, state, frontend = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(log_path, 'rb').read().decode('utf-8', 'replace')
text = re.sub(r'\x1b\[[0-9;?]*[A-Za-z]', '', text)


def hexes(prefix):
    return [bytes.fromhex(m) for m in re.findall(re.escape(prefix) + r'([0-9a-f]+)', text)]


def need(condition, what):
    if not condition:
        print(f'qemu-tpm-tool: {frontend}: {what}', file=sys.stderr)
        sys.exit(1)


draws = [value for value in hexes('tpm: random ') if len(value) == 16]
need(len(draws) >= 2 and draws[0] != draws[1], 'two draws of sixteen bytes did not both arrive, or were the same')
pcrs = [value for value in hexes('tpm: pcr ') if len(value) == 32]
# In the scenario's order: PCR 16 before and after the extend, PCR 8 before and after its refusal, and PCR 16 before
# the quote.
need(len(pcrs) >= 5, f'{len(pcrs)} PCR reads in the log, not five')
measured = hashlib.sha256(b'liber-measure').digest()
extended = hexes('tpm: extended PCR 16 with ')
need(extended and extended[0] == measured, 'the digest extended is not SHA-256 of the text typed')
need(pcrs[1] == hashlib.sha256(pcrs[0] + measured).digest(), 'PCR 16 is not SHA-256(its old value || the measurement)')
need(pcrs[2] == pcrs[3], 'PCR 8 moved though its extend was refused')
parts = {name: hexes(f'tpm: quote {name} ') for name in ('attest', 'r', 's', 'x', 'y', 'pcr-digest')}
need(all(len(values) == 1 for values in parts.values()), 'the quote was not printed whole, once')
attest, r, s, x, y, digest = (parts[name][0] for name in ('attest', 'r', 's', 'x', 'y', 'pcr-digest'))
need(b'liber-nonce' in attest, 'the attestation does not carry the nonce')
need(digest == hashlib.sha256(pcrs[4]).digest(), 'the quote does not carry the digest of PCR 16 as it was read')
need(digest in attest, 'the attestation does not carry the digest the quote reports')


def der_integer(value):
    body = value.lstrip(b'\0') or b'\0'
    if body[0] & 0x80:
        body = b'\0' + body
    return bytes([0x02, len(body)]) + body


spki = bytes.fromhex('3059301306072a8648ce3d020106082a8648ce3d030107034200') + b'\x04' + x + y
signature = der_integer(r) + der_integer(s)
signature = bytes([0x30, len(signature)]) + signature
open(f'{state}/key.der', 'wb').write(spki)
open(f'{state}/sig.der', 'wb').write(signature)
open(f'{state}/attest.bin', 'wb').write(attest)
verified = subprocess.run(['openssl', 'dgst', '-sha256', '-keyform', 'DER', '-verify', f'{state}/key.der', '-signature', f'{state}/sig.der', f'{state}/attest.bin'], capture_output=True)
need(verified.returncode == 0, 'OpenSSL does not verify the quote signature by the point it carries')
forged = bytearray(attest)
forged[-1] ^= 1
open(f'{state}/forged.bin', 'wb').write(bytes(forged))
refused = subprocess.run(['openssl', 'dgst', '-sha256', '-keyform', 'DER', '-verify', f'{state}/key.der', '-signature', f'{state}/sig.der', f'{state}/forged.bin'], capture_output=True)
need(refused.returncode != 0, 'OpenSSL verified the signature over an attestation one bit different')
print(f'qemu-tpm-tool: {frontend}: two draws differ, PCR 16 is the chain of the text typed, PCR 8 did not move, and OpenSSL verifies the quote over this nonce and PCR 16')
EOF
	grep -aqF "driver.tpm: online - $interface interface" "$state/serial.log" || fail "the $frontend run's driver never said it was online over $interface"
	[[ "$(grep -ac "driver.tpm: online - $interface interface" "$state/serial.log")" -ge 2 ]] || fail "the $frontend run's driver did not come online again after its enable"
}

if [[ "$arch" == x86_64 ]]; then
	run crb CRB
	run tis FIFO
	echo "qemu-tpm-tool: PASS - behind CRB and behind TIS, the TPM2 table's row is one 4 KiB page with no interrupt, the driver binds it and restarts under a held connection, TpmService serves the tool and refuses the probe by name, and the quote verifies"
else
	run tis FIFO
	echo "qemu-tpm-tool: PASS on $arch - behind tpm-tis-device, the tree node's row is one 4 KiB page with no interrupt, the driver binds it and restarts under a held connection, TpmService serves the tool and refuses the probe by name, and the quote verifies"
fi
