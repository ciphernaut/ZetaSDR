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
space-separated>, "n": <how many>}`. FAKE_WAV=1 makes it read a 44-byte WAV
header first (exiting 3 unless it starts `RIFF....WAVE`), print
`{"wav": {"rate_hz": R, "bits": B, "channels": C}}` as its first line, then count
only the bytes after the header and exit 3 should a second `RIFF` follow.
$FAKE_REPEAT_BYTES (default 0: never) writes the lines again each time that many
more input bytes have arrived after the last write.
`--lines-file PATH` takes the lines from that file instead of $FAKE_LINES, which is
node-wide when a node runs this as a mode row: two rows differ by their `args`.
"""

import json
import os
import socket
import struct
import sys

WAV_HEADER_LEN = 44


def read_wav_header(src, out) -> bool:
    """Read the 44-byte header and print its line; False when it is no WAV header."""
    hdr = b""
    while len(hdr) < WAV_HEADER_LEN:
        chunk = src.read(WAV_HEADER_LEN - len(hdr))
        if not chunk:
            break
        hdr += chunk
    if len(hdr) < WAV_HEADER_LEN or hdr[0:4] != b"RIFF" or hdr[8:12] != b"WAVE":
        sys.stderr.write(f"fake_lines: error: wav header (got {hdr[:12].hex()})\n")
        return False
    channels, rate = struct.unpack_from("<HI", hdr, 22)
    (bits,) = struct.unpack_from("<H", hdr, 34)
    line = json.dumps({"wav": {"rate_hz": rate, "bits": bits, "channels": channels}})
    out.write((line + "\n").encode())
    out.flush()
    return True


def main() -> int:
    args = sys.argv[1:]
    lines = os.environ.get("FAKE_LINES", "")
    if "--lines-file" in args:
        with open(args[args.index("--lines-file") + 1]) as f:
            lines = f.read().rstrip("\n")
    if os.environ.get("FAKE_PRINT_ENV") == "1":
        names = sorted(os.environ)
        lines = json.dumps({"text": " ".join(names), "n": len(names)})
    after = int(os.environ.get("FAKE_AFTER_BYTES", "1024"))
    via = os.environ.get("FAKE_VIA", "stdout")
    exit_after = os.environ.get("FAKE_EXIT_AFTER") == "1"
    repeat = int(os.environ.get("FAKE_REPEAT_BYTES", "0"))
    next_at = 0

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
    wav = os.environ.get("FAKE_WAV") == "1"
    if wav and not read_wav_header(src, out):
        return 3
    total = 0
    head = b""
    sent = False
    tail = b""

    def send() -> None:
        text = lines.replace("{bytes}", str(total)).replace("{head}", head.hex()).replace("{in}", in_path)
        out.write((text + "\n").encode())
        out.flush()
        sys.stderr.write(f"fake_lines: sent lines after {total} bytes\n")
        sys.stderr.flush()

    if after == 0:
        send()
        sent = True
        next_at = repeat
        if exit_after:
            return 3
    while True:
        chunk = src.read1(65536)
        if not chunk:
            break
        if wav:
            if b"RIFF" in tail + chunk:
                sys.stderr.write("fake_lines: error: a second wav header\n")
                return 3
            tail = (tail + chunk)[-3:]
        if len(head) < 8:
            head = (head + chunk)[:8]
        total += len(chunk)
        if not sent and total >= after:
            send()
            sent = True
            next_at = total + repeat
            if exit_after:
                return 3
        elif sent and repeat and total >= next_at:
            send()
            next_at = total + repeat
    sys.stderr.write(f"fake_lines: input closed after {total} bytes\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
