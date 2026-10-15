#!/usr/bin/env python3
# THE FAR END OF THE CDC-NCM ORACLE: the gadget's network interface on this host, answered at the Ethernet level.
#
# The device the guest binds is the host kernel's own `usb_f_ncm` - an NCM implementation this tree did not write - and
# what the guest sends through it arrives HERE, as Ethernet frames on the function's network interface. This process is
# the other end of that cable, and it is deliberately not a network stack: it answers two kinds of frame and ignores the
# rest.
#
#   - A PROBE: IEEE 802's local experimental EtherType 0x88B5 and the marker `LIBER-NCM`, then the probe's number, how
#     many copies it asks for (one to eight) and 0xFF. It is answered with that many copies of itself, the two addresses
#     swapped and the last of those three bytes saying which copy each is - SENT BACK TO BACK, so the device packs them
#     into one transfer block when they reach it close enough together. Several datagrams in one block is the case NCM
#     exists for, and the one a driver that reads one datagram a block gets wrong.
#   - AN ARP REQUEST FOR 10.0.2.2, the gateway QEMU's user network would have been: the address the controller's own
#     enumeration oracle asks for, whose ECM adapter a run with this gadget leaves off the bus. Answered from this
#     interface's address.
#
# NOTHING ON THE HOST'S NETWORK IS CHANGED BY IT: no address, no route, no forwarding. The interface it listens on is the
# gadget's own, which `usb-gadget.sh` brought up bare, and a raw packet socket reads and writes frames on it without the
# host's stack taking part in either. It says on stderr what it answered, so a failing run has something to read.

import argparse
import socket
import sys

ETH_P_ALL = 0x0003
ETHERTYPE_PROBE = 0x88B5
ETHERTYPE_ARP = 0x0806
MAGIC = b"LIBER-NCM"
# Where the probe's three bytes are, after the Ethernet header and the marker.
SEQUENCE = 14 + len(MAGIC)
COPIES = SEQUENCE + 1
INDEX = SEQUENCE + 2
MOST_COPIES = 8
GATEWAY = bytes([10, 0, 2, 2])
# An ARP header for Ethernet and IPv4: hardware type 1, protocol 0x0800, lengths six and four.
ARP_ETHERNET_IPV4 = bytes([0, 1, 8, 0, 6, 4])


def answers(frame, own):
    """The frames one received frame asks for, in the order they are to be sent - none for anything else."""
    if len(frame) < 14:
        return []
    kind = int.from_bytes(frame[12:14], "big")
    if kind == ETHERTYPE_PROBE and len(frame) > INDEX and frame[14:SEQUENCE] == MAGIC and frame[INDEX] == 0xFF:
        copies = frame[COPIES]
        if not 1 <= copies <= MOST_COPIES:
            return []
        swapped = frame[6:12] + frame[0:6] + frame[12:]
        out = []
        for index in range(copies):
            copy = bytearray(swapped)
            copy[INDEX] = index
            out.append(bytes(copy))
        return out
    if kind == ETHERTYPE_ARP and len(frame) >= 42:
        arp = frame[14:42]
        asker_mac, asker_ip, wanted = arp[8:14], arp[14:18], arp[24:28]
        # A REQUEST FOR THE GATEWAY, AND NOT A PROBE OF AN ADDRESS: an asker with no address yet, or one announcing its
        # own, is checking that nobody else has it - and answering that would tell it somebody does.
        if arp[0:6] == ARP_ETHERNET_IPV4 and arp[6:8] == b"\x00\x01" and wanted == GATEWAY and asker_ip not in (bytes(4), GATEWAY):
            reply = asker_mac + own + frame[12:14] + ARP_ETHERNET_IPV4 + b"\x00\x02" + own + GATEWAY + asker_mac + asker_ip
            return [reply]
    return []


def main():
    parser = argparse.ArgumentParser(description="answer the CDC-NCM gadget's guest at the Ethernet level")
    parser.add_argument("--interface", required=True, help="the gadget's network interface on this host")
    parser.add_argument("--ready", required=True, help="written once the socket listens")
    args = parser.parse_args()

    with open(f"/sys/class/net/{args.interface}/address") as address:
        own = bytes.fromhex(address.read().strip().replace(":", ""))
    sock = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(ETH_P_ALL))
    sock.bind((args.interface, 0))
    with open(args.ready, "w") as ready:
        ready.write(f"listening on {args.interface} as {own.hex(':')}\n")
    print(f"ncm-peer: listening on {args.interface} as {own.hex(':')}", file=sys.stderr, flush=True)

    answered = 0
    while True:
        try:
            frame, address = sock.recvfrom(65536)
        except OSError as error:
            # THE INTERFACE WENT WITH THE GADGET, which is how a run ends.
            print(f"ncm-peer: {args.interface} is gone ({error.strerror}) after {answered} answer(s)", file=sys.stderr, flush=True)
            return 0
        # WHAT THIS PROCESS SENT comes back to a socket bound to every protocol, and is not the guest's.
        if address[2] == socket.PACKET_OUTGOING:
            continue
        replies = answers(frame, own)
        if not replies:
            continue
        for reply in replies:
            try:
                sock.send(reply)
            except OSError as error:
                print(f"ncm-peer: a {len(reply)}-byte answer was not sent ({error.strerror})", file=sys.stderr, flush=True)
        answered += len(replies)
        kind = "probe" if int.from_bytes(frame[12:14], "big") == ETHERTYPE_PROBE else "ARP request"
        print(f"ncm-peer: a {len(frame)}-byte {kind} answered with {len(replies)} frame(s)", file=sys.stderr, flush=True)


if __name__ == "__main__":
    sys.exit(main())
