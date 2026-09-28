# The TPM, for programs that use it

WHAT THIS DOCUMENT IS. LiberSystem drives the machine's TPM 2.0 and offers it to programs through one typed
contract, `liber:tpm@1`. This page is for a developer writing such a program: what the contract offers, which
grant each call needs, what a PCR and a sealed secret mean on this system, and what the TPM here is not. The
contract's generated reference is [`docs/gen/liber/tpm/v1.md`](gen/liber/tpm/v1.md); the client library is
`tpm-client` (`src/user/libs/clients/tpm-client`), which the system's own `tpm` tool links the same way.

## What a program is given

A program never opens the TPM. PermissionManager mints it connections at launch, one per grant its permission row
names, each alive as long as the program is:

| Grant         | Calls it carries                         | What it can hurt |
|---------------|------------------------------------------|------------------|
| `tpm`         | `info`, `random`, `pcr-read`, `quote`    | nothing: it observes |
| `tpm-measure` | `pcr-extend`                             | every later quote and unseal on the machine |
| `tpm-seal`    | `seal`, `unseal`                         | the program's own secrets |

`info` works on every grant. A call sent on a connection whose grant does not carry it is answered `not-granted`
by TpmService; `tpm-client` holds whichever grants the program received and answers `not-granted` itself, without
a message, for a call none of them carries. Nothing is granted by default: a program receives a TPM grant only when
its permission row names it.

There is no command passthrough. A raw TPM command interface is an authority over every session and every
hierarchy of the machine, so every call is a typed operation whose command the system builds.

## The calls, their bounds and what they cost

- `info`: the interface (FIFO or CRB), the manufacturer's four characters, the vendor string, the two
  firmware-version words, and whether sealing works on this TPM.
- `random`: at most 1024 bytes from the TPM's generator.
- `pcr-read`: any PCR, 0 to 23, of the SHA-256 bank.
- `pcr-extend`: PCR 16 or 23 only, with a 32-byte SHA-256 digest.
- `seal`: at most 96 bytes, sealed to one PCR's present value (any PCR 0-23); the answer is the sealed object's
  bytes, which the program keeps wherever it keeps files. `unseal` takes them back.
- `quote`: one PCR (any, 0-23) over a nonce of at most 64 bytes; the answer is the TPM's attestation, its ECDSA
  P-256 signature (r, s), the signing key's public point (x, y), and the PCR digest the attestation carries.

`seal`, `unseal` and `quote` each generate a primary key in the TPM, which takes a TPM tens to hundreds of
milliseconds; the others are one short command. The TPM runs one operation at a time for the whole machine: a
program has one call outstanding per connection (a second answers `busy`), and sixteen calls wait in all.

Every answer carries an `outcome`: `done` with its value, or the refusal by name - `not-granted`,
`pcr-not-allowed`, `bounds`, `busy`, `policy-refused` (the unseal's PCR no longer holds its value),
`owner-hierarchy-unavailable`, `other-component`, `interrupted`, `unavailable`, `tpm` (the TPM's own response code,
in `code`) or `fault` (the TPM's interface failed, or its answer could not be read).

## PCRs

The PCRs a program may extend are 16 and 23: the PC Client profile's debug PCR and its application PCR, both
extendable at locality 0. PCRs 0 to 7 are the firmware's, 8 to 15 are kept for this system's own measured boot,
and 17 to 22 belong to the dynamic root of trust. Every PCR can be read, sealed to and quoted.

PCRS ARE SHARED BY THE WHOLE MACHINE. Any holder of `tpm-measure` can move PCR 16 or 23, and a secret sealed to one
of them then stays shut until the next boot resets it. A program that seals to 16 or 23 is sealing to a value
another program may change.

A RESUME FROM SLEEP (S3) RESETS PCRS 16 AND 23 as well: the PC Client profile saves only PCRs 0 to 15 across a
sleep. So a secret sealed to 16 or 23 stays shut after a sleep too, while one sealed to a PCR from 0 to 15 survives
it.

## Sealed secrets

A sealed object opens:

- only on THIS TPM - it is bound to the TPM's owner seed;
- only while its PCR holds the value it was sealed to - otherwise `unseal` answers `policy-refused`;
- only for the COMPONENT THAT SEALED IT - TpmService tags every secret with the SHA-256 of the sealing component's
  name, and an `unseal` by another component answers `other-component` and returns nothing. This holds because
  TpmService is the only process that reaches the TPM's unseal;
- and never after a TPM clear, which changes the owner seed.

A sealed object's bytes are not secret - its private half is encrypted to the TPM's storage key - and a program may
store them anywhere.

## When sealing is unavailable

Seal, unseal and quote each begin with a primary key made under the owner hierarchy with an empty authorization.
On a machine whose owner hierarchy has an authorization value (another system or the firmware provisioned it) or is
disabled, they answer `owner-hierarchy-unavailable` without a command reaching the TPM, and `info` says sealing is
unavailable; `random`, `pcr-read` and `pcr-extend` keep working. No path supplies an owner authorization. The remedy
is a TPM clear from the firmware's setup, which makes every earlier sealed object unopenable.

## Quotes

The quote is signed by a restricted signing key the TPM generates for it, which signs only what the TPM itself
produced - so a quote cannot be forged by asking the TPM to sign bytes that look like one. The key is NOT certified
by the TPM's endorsement key: a verifier learns it on first use (trust on first use), and checks each later quote's
signature against the public point it carries.

## When the TPM, its driver or the service restarts

The TPM's state is in the chip. When its driver restarts, a program's connections STAY: a call the driver was
running answers `interrupted` and is never replayed (an extend may have reached the TPM before the driver stopped,
and a replay would extend twice - a program that must know reads the PCR), calls made while the driver is away
answer `unavailable`, and the same connection is served again once the driver is back. When TpmService restarts,
every connection dies with it, and the grants are minted again at the program's next launch.

## Which TPMs are driven

A TPM 2.0 whose interface is the FIFO (start method 6) or the CRB (start method 7) - on a PC through the ACPI `TPM2`
table, and on a device-tree machine through a `tcg,tpm-tis-mmio` node. A TPM whose start method needs AML (ACPI
start), an Arm secure-monitor call or an I2C bus is not driven, and on such a machine no TPM exists for programs.
Localities above 0, RSA keys, HMAC and parameter-encryption sessions are not offered.

## What this TPM is not

It is NOT A KEY STORE WITH BACKUP: a sealed secret is lost with the TPM, with a TPM clear, and with the PCR value it
was sealed to. It is NOT A MEASURED-BOOT CHAIN: the system does not measure its own loader and kernel yet, so PCRs 8
to 15 carry nothing, and a quote says what the firmware measured and what programs extended - no more.
