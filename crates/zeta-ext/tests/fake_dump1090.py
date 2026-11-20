"""Test stand-in for dump1090, driven by the adsb_decode stage's tests.

Parses --net-sbs-port, listens on 127.0.0.1 at that port and accepts one
connection, then reads SC16 samples from stdin. Once 19 200 bytes have
arrived it writes $FAKE_SBS_LINE plus a newline to the connection and keeps
reading stdin until EOF. FAKE_EXIT_AFTER=1 makes it exit 3 right after
writing the line; FAKE_REPEAT_MS=N writes the line again every N ms of
input read (for a reader that subscribes late); FAKE_PIDFILE names a file to
write its pid to at start.

With a non-zero --net-bo-port it also listens there for one Beast client and,
for each SBS line it writes, sends one Beast Mode S long frame (`1a 33`): a
DF17 identification frame for that line's address (field 5), its parity the
Mode S CRC. Frames written before the client connects wait for it.
"""

import os
import socket
import sys
import threading
import time

THRESHOLD = 19_200
ESC = 0x1A
# The 6-bit character set of an ADS-B identification message.
CHARS = "#ABCDEFGHIJKLMNOPQRSTUVWXYZ##### ###############0123456789######"


def crc24(data: bytes) -> int:
    """The Mode S parity of `data` (generator 0x1FFF409)."""
    crc = 0
    for byte in data:
        crc ^= byte << 16
        for _ in range(8):
            crc <<= 1
            if crc & 0x1000000:
                crc ^= 0x1FFF409
    return crc & 0xFFFFFF


def df17_ident(address: int, callsign: str = "ZETA1") -> bytes:
    """A 14-byte DF17 frame (capability 5), type code 4, for `address`."""
    me = 4 << 51  # type code 4, emitter category 0
    for i, ch in enumerate(callsign.ljust(8)[:8]):
        me |= CHARS.index(ch) << (42 - 6 * i)
    body = bytes([0x8D]) + address.to_bytes(3, "big") + me.to_bytes(7, "big")
    return body + crc24(body).to_bytes(3, "big")


def beast_long(frame: bytes, counter: int) -> bytes:
    """`frame` as a Beast `1a 33` record: 6-byte counter, signal byte, data;
    every `1a` after the type byte doubled."""
    body = counter.to_bytes(6, "big") + bytes([0x80]) + frame
    return bytes([ESC, 0x33]) + body.replace(bytes([ESC]), bytes([ESC, ESC]))


class Beast:
    """The Beast output: one client, accepted in the background."""

    def __init__(self, port: int):
        self.lock = threading.Lock()
        self.conn = None
        self.pending = []
        self.counter = 0
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", port))
        srv.listen(1)
        threading.Thread(target=self.accept, args=(srv,), daemon=True).start()

    def accept(self, srv):
        conn, _ = srv.accept()
        with self.lock:
            self.conn = conn
            for record in self.pending:
                self.send(record)
            self.pending.clear()

    def send(self, record: bytes):
        try:
            self.conn.sendall(record)
        except OSError:
            pass

    def frame_for(self, line: str):
        fields = line.strip().split(",")
        if len(fields) < 5 or not fields[4]:
            return
        self.counter += 12_000_000 // 1000  # 1 ms of the 12 MHz counter per frame
        record = beast_long(df17_ident(int(fields[4], 16)), self.counter)
        with self.lock:
            if self.conn is None:
                self.pending.append(record)
            else:
                self.send(record)


def option(args, name):
    return int(args[args.index(name) + 1]) if name in args else 0


def main() -> int:
    args = sys.argv[1:]
    port = int(args[args.index("--net-sbs-port") + 1])
    pidfile = os.environ.get("FAKE_PIDFILE")
    if pidfile:
        with open(pidfile, "w") as f:
            f.write(str(os.getpid()))
    line = os.environ.get("FAKE_SBS_LINE", "")
    exit_after = os.environ.get("FAKE_EXIT_AFTER") == "1"
    repeat_ms = int(os.environ.get("FAKE_REPEAT_MS", "0"))
    bo_port = option(args, "--net-bo-port")
    beast = Beast(bo_port) if bo_port else None

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(1)
    conn, _ = srv.accept()

    def write_line():
        conn.sendall((line + "\n").encode())
        if beast:
            beast.frame_for(line)

    total = 0
    sent = False
    last = 0.0
    stdin = sys.stdin.buffer
    while True:
        chunk = stdin.read1(65536)
        if not chunk:
            break
        total += len(chunk)
        if not sent and total >= THRESHOLD:
            write_line()
            sent = True
            last = time.monotonic()
            sys.stderr.write(f"fake_dump1090: sent line after {total} bytes\n")
            if exit_after:
                return 3
        elif sent and repeat_ms > 0 and time.monotonic() - last >= repeat_ms / 1000:
            write_line()
            last = time.monotonic()
    sys.stderr.write(f"fake_dump1090: stdin closed after {total} bytes\n")
    conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
