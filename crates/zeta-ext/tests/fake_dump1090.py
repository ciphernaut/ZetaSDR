"""Test stand-in for dump1090, driven by the adsb_decode stage's tests.

Parses --net-sbs-port, listens on 127.0.0.1 at that port and accepts one
connection, then reads SC16 samples from stdin. Once 19 200 bytes have
arrived it writes $FAKE_SBS_LINE plus a newline to the connection and keeps
reading stdin until EOF. FAKE_EXIT_AFTER=1 makes it exit 3 right after
writing the line; FAKE_PIDFILE names a file to write its pid to at start.
"""

import os
import socket
import sys

THRESHOLD = 19_200


def main() -> int:
    args = sys.argv[1:]
    port = int(args[args.index("--net-sbs-port") + 1])
    pidfile = os.environ.get("FAKE_PIDFILE")
    if pidfile:
        with open(pidfile, "w") as f:
            f.write(str(os.getpid()))
    line = os.environ.get("FAKE_SBS_LINE", "")
    exit_after = os.environ.get("FAKE_EXIT_AFTER") == "1"

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(1)
    conn, _ = srv.accept()

    total = 0
    sent = False
    stdin = sys.stdin.buffer
    while True:
        chunk = stdin.read1(65536)
        if not chunk:
            break
        total += len(chunk)
        if not sent and total >= THRESHOLD:
            conn.sendall((line + "\n").encode())
            sent = True
            sys.stderr.write(f"fake_dump1090: sent line after {total} bytes\n")
            if exit_after:
                return 3
    sys.stderr.write(f"fake_dump1090: stdin closed after {total} bytes\n")
    conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
