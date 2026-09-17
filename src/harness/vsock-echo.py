#!/usr/bin/env python3
# THE HOST END OF THE VSOCK ORACLE.
#
# A guest driver that brings a vsock device up and reports a state is not a driver that moves bytes,
# and every other driver this tree admitted had to move some. vsock's other end is the HOST, so the
# oracle needs a process here: this one listens on a port, echoes back whatever arrives, and exits
# when it is killed.
#
# It is deliberately tiny and deliberately not a service: it holds no state between connections, it
# answers one port, and it says on stderr what it saw so a failing run has something to read.

import argparse
import os
import socket
import sys
import threading
import time


def serve(conn):
    # Echo until the peer stops. A stream is bytes and not messages, so the loop never assumes that
    # what the guest wrote in one call arrives in one read.
    try:
        while True:
            chunk = conn.recv(65536)
            if not chunk:
                break
            conn.sendall(chunk)
    except OSError as why:
        print(f"vsock-echo: connection ended: {why}", file=sys.stderr, flush=True)
    finally:
        conn.close()


def watch_parent():
    # EXIT WHEN WHOEVER STARTED THIS IS GONE.
    #
    # Two of the three harness paths `exec` into QEMU, which replaces the shell that would otherwise
    # have killed this process afterwards - so a listener started there would outlive its run and
    # hold the host port against the next one. When QEMU exits, this process is reparented to init
    # and `getppid()` becomes 1, which is the one signal available to it without a pipe or a
    # pidfile. Checked once a second: a listener that lingers for a second costs nothing, and one
    # that lingers for ever costs the next run its port.
    while True:
        time.sleep(1)
        if os.getppid() == 1:
            print("vsock-echo: the run that started this is gone", file=sys.stderr, flush=True)
            os._exit(0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    if not hasattr(socket, "AF_VSOCK"):
        print("vsock-echo: this host's python has no AF_VSOCK", file=sys.stderr, flush=True)
        return 1
    sock = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind((socket.VMADDR_CID_ANY, args.port))
    except OSError as why:
        print(f"vsock-echo: could not bind port {args.port}: {why}", file=sys.stderr, flush=True)
        return 1
    sock.listen(4)
    print(f"vsock-echo: listening on port {args.port}", file=sys.stderr, flush=True)
    threading.Thread(target=watch_parent, daemon=True).start()
    while True:
        try:
            conn, addr = sock.accept()
        except OSError as why:
            print(f"vsock-echo: accept ended: {why}", file=sys.stderr, flush=True)
            return 0
        print(f"vsock-echo: connection from cid {addr[0]} port {addr[1]}", file=sys.stderr, flush=True)
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    sys.exit(main())
