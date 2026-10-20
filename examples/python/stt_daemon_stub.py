#!/usr/bin/env -S python3 -I
"""A stub speech daemon for `zeta serve --stt-socket`: where a Whisper or Vosk wrapper starts.

Usage: stt_daemon_stub.py SOCKET_PATH

One line per connection: `file:<absolute path of a 16 kHz mono s16 WAV>` is answered
`ok:<text>` or `error:<message>`; `languages` is answered `ok:<codes>`. This stub answers a
fixed text and `ok:en,de`. Replace `transcribe()` with a real model.
"""

import os
import socketserver
import sys

TEXT = "hello from the stub daemon"
LANGUAGES = "en,de"


def transcribe(wav):
    """Return the text of the WAV file's bytes. The stub ignores them."""
    return TEXT


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline(65536).decode("utf-8", "replace").strip()
        if line == "languages":
            reply = "ok:" + LANGUAGES
        elif line.startswith("file:"):
            try:
                with open(line[5:], "rb") as f:
                    reply = "ok:" + transcribe(f.read()).replace("\n", " ")
            except OSError as e:
                reply = "error:" + str(e).replace("\n", " ")
        else:
            reply = "error:unknown command"
        self.wfile.write((reply + "\n").encode())


if __name__ == "__main__":
    sock = sys.argv[1]
    if os.path.exists(sock):
        os.unlink(sock)
    with socketserver.ThreadingUnixStreamServer(sock, Handler) as server:
        server.serve_forever()
