"""Test stand-in for an external decoder program, driven by external_decode's tests.

Reads its input from stdin, or from the file after `--in` (a fifo). Once
$FAKE_AFTER_BYTES bytes (default 1024; 0 for at once) have arrived it writes
the lines of $FAKE_LINES (newline-separated), `{bytes}` in them replaced by the
byte count so far, `{head}` by the first 8 input bytes in hex and `{in}` by the
input path (`-` for stdin), then keeps
reading until EOF and exits 0. $FAKE_VIA is `stdout` (the default) or
`tcp:<flag>`: listen on 127.0.0.1 at the port after <flag> in the arguments,
accept one connection and write the lines there. FAKE_EXIT_AFTER=1 makes it
exit 3 right after writing the lines. FAKE_PRINT_ENV=1 writes, instead of
$FAKE_LINES, one JSON line `{"text": <the sorted names of its environment,
space-separated>, "n": <how many>}`.
"""

import json
import os
import socket
import sys


def main() -> int:
    args = sys.argv[1:]
    lines = os.environ.get("FAKE_LINES", "")
    if os.environ.get("FAKE_PRINT_ENV") == "1":
        names = sorted(os.environ)
        lines = json.dumps({"text": " ".join(names), "n": len(names)})
    after = int(os.environ.get("FAKE_AFTER_BYTES", "1024"))
    via = os.environ.get("FAKE_VIA", "stdout")
    exit_after = os.environ.get("FAKE_EXIT_AFTER") == "1"

    out = sys.stdout.buffer
    if via.startswith("tcp:"):
        port = int(args[args.index(via[4:]) + 1])
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", port))
        srv.listen(1)
        conn, _ = srv.accept()
        out = conn.makefile("wb")

    in_path = args[args.index("--in") + 1] if "--in" in args else "-"
    src = open(in_path, "rb") if in_path != "-" else sys.stdin.buffer
    total = 0
    head = b""
    sent = False

    def send() -> None:
        text = lines.replace("{bytes}", str(total)).replace("{head}", head.hex()).replace("{in}", in_path)
        out.write((text + "\n").encode())
        out.flush()
        sys.stderr.write(f"fake_lines: sent lines after {total} bytes\n")
        sys.stderr.flush()

    if after == 0:
        send()
        sent = True
        if exit_after:
            return 3
    while True:
        chunk = src.read1(65536)
        if not chunk:
            break
        if len(head) < 8:
            head = (head + chunk)[:8]
        total += len(chunk)
        if not sent and total >= after:
            send()
            sent = True
            if exit_after:
                return 3
    sys.stderr.write(f"fake_lines: input closed after {total} bytes\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
