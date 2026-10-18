"""Plugin protocol 1: the byte stream between the host and an external stage. Pure codec,
no processes.

A child process speaks it on stdin (host to plugin) and stdout (plugin to host); stderr
is free text that the host copies to its own stderr prefixed `[plugin <kind>] `. Each
message begins with one byte:

- `0x7B` (`{`): a **control message**, one compact UTF-8 JSON object ending in `0x0A`, at
  most 65 536 bytes including the newline.
- `0x00`: a **data frame**: `u32` little-endian header length `H` (1..=65 536), `H` bytes
  of UTF-8 JSON header, `u32` little-endian body length `B` (0..=16 777 216), `B` body
  bytes.
- any other byte: protocol error `unexpected byte 0x<hh> at message start`.

Control messages are tagged by `"type"` (snake_case), the tag the websocket's
`WsMessage` uses. Plugin to host: `{"type":"hello","protocol":1,"kind":"<k>",
"inputs":["iq"],"outputs":["iq"],"params_schema":{…}}` (the first message, within
5000 ms of spawn) and `{"type":"error","message":"<m>"}`. Host to plugin:
`{"type":"run","protocol":1,"params":{…},"inputs":[{"kind":"iq"}],
"outputs":[{"stream":"<id>","kind":"iq"}]}`, `{"type":"lagged","missed":N}` and
`{"type":"closed","reason":"<r>"}` (byte-identical to the websocket's), and
`{"type":"stop"}`. Unknown fields are refused.

A data frame's header is exactly the websocket's frame message:
`{"type":"frame","stream","kind","source","job","channel","start_ns","end_ns","payload"}`
with `payload` the frame summary for the kind. Body by kind:
- `iq`: payload `{"sample_rate_hz","center_hz","samples":N}`, body `N × 8` bytes,
  interleaved `f32` little-endian I then Q (SigMF `cf32_le`); the block's `t0` is
  `start_ns`.
- `audio`: payload `{"sample_rate_hz","samples":N,"peak"}`, body `N × 4` bytes `f32` LE.
- `event`: payload is the event, `B = 0`.
- `text`: payload is the string, `B = 0`.
- `video` and `sink` are refused in protocol 1.

`B` must match `N`, and an `iq` or `audio` `sample_rate_hz` must not be 0. Every length is checked before anything is allocated or indexed: a
malformed message is a [`WireError`] naming what was wrong, never a panic.
"""

# ---------------------------------------------------------------------------------------
# The Python SDK for that protocol (standard library only). A plugin calls `run` with its
# kind, ports, params schema and an `on_frame(params, frame)` callback; `run` says hello,
# waits for `run`, and calls back once per input frame until the input closes.
# ---------------------------------------------------------------------------------------

import json
import struct
import sys
import traceback

__all__ = [
    "PROTOCOL",
    "MAX_CONTROL",
    "MAX_HEADER",
    "MAX_BODY",
    "Frame",
    "ProtocolError",
    "encode_control",
    "encode_frame",
    "read_message",
    "run",
]

#: The protocol version a `hello` and a `run` carry.
PROTOCOL = 1
#: Longest control message, newline included, in bytes.
MAX_CONTROL = 65536
#: Longest data-frame header, in bytes.
MAX_HEADER = 65536
#: Longest data-frame body, in bytes.
MAX_BODY = 16777216

_CONTROL_START = 0x7B
_FRAME_START = 0x00
_U32 = struct.Struct("<I")
# Longest error message sent; JSON escaping can grow a character to 12 bytes.
_MAX_ERROR = 4096


class ProtocolError(Exception):
    """Bytes that are not protocol 1."""


def _json(value):
    # Compact, like serde_json; NaN and infinities are not JSON and are refused.
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


class Frame:
    """One data frame.

    `kind` is `"iq"`, `"audio"`, `"event"` or `"text"`; `header` is the frame header as a
    dict (`stream`, `source`, `job`, `channel`, `start_ns`, `end_ns`, ...); `samples` is a
    `list[complex]` for iq, a `list[float]` for audio and `None` otherwise; `payload` is
    `header["payload"]`: the iq or audio summary, the event value or the text.
    """

    __slots__ = ("kind", "header", "samples", "payload")

    def __init__(self, kind, header, samples=None, payload=None):
        self.kind = kind
        self.header = header
        self.samples = samples
        self.payload = header.get("payload") if payload is None else payload

    def with_samples(self, samples):
        """A copy carrying `samples`, with `payload["samples"]` (and audio's `peak`) updated."""
        samples = list(samples)
        payload = dict(self.payload)
        payload["samples"] = len(samples)
        if self.kind == "audio":
            peak = 0.0
            for s in samples:
                if s != s:  # NaN
                    peak = None
                    break
                peak = max(peak, abs(s))
            payload["peak"] = peak
        header = dict(self.header)
        header["payload"] = payload
        return Frame(self.kind, header, samples, payload)

    def __repr__(self):
        n = None if self.samples is None else len(self.samples)
        return "Frame(kind=%r, stream=%r, samples=%r)" % (self.kind, self.header.get("stream"), n)


def encode_control(message):
    """A control message (a dict with `"type"`) as its compact JSON line."""
    line = _json(message) + b"\n"
    if len(line) > MAX_CONTROL:
        raise ProtocolError(
            "control line of %d bytes exceeds the %d-byte limit" % (len(line), MAX_CONTROL)
        )
    return line


def encode_frame(frame):
    """A `Frame` as a data frame: `0x00`, header length, header, body length, body."""
    header = dict(frame.header)
    header["type"] = "frame"
    header["kind"] = frame.kind
    header["payload"] = frame.payload
    if frame.kind == "iq":
        flat = []
        for s in frame.samples:
            flat.append(s.real)
            flat.append(s.imag)
        body = struct.pack("<%df" % len(flat), *flat)
    elif frame.kind == "audio":
        body = struct.pack("<%df" % len(frame.samples), *frame.samples)
    elif frame.kind in ("event", "text"):
        body = b""
    else:
        raise ProtocolError("%s frames are not carried by plugin protocol 1" % frame.kind)
    head = _json(header)
    if len(head) > MAX_HEADER:
        raise ProtocolError("header of %d bytes exceeds the %d-byte limit" % (len(head), MAX_HEADER))
    if len(body) > MAX_BODY:
        raise ProtocolError("body of %d bytes exceeds the %d-byte limit" % (len(body), MAX_BODY))
    return b"".join((b"\x00", _U32.pack(len(head)), head, _U32.pack(len(body)), body))


def _read_exact(stream, n, what):
    data = stream.read(n) if n else b""
    if len(data) < n:
        raise ProtocolError("stream ended inside the %s: %d of %d bytes" % (what, len(data), n))
    return data


def read_message(stream):
    """Read one message from a binary stream.

    Returns a dict for a control message, a `Frame` for a data frame, or `None` on EOF at a
    message boundary. Raises `ProtocolError` for anything malformed.
    """
    first = stream.read(1)
    if not first:
        return None
    if first[0] == _CONTROL_START:
        # At most MAX_CONTROL bytes with the newline, the `{` already read.
        line = first + stream.readline(MAX_CONTROL - 1)
        if not line.endswith(b"\n"):
            if len(line) >= MAX_CONTROL:
                raise ProtocolError("control line exceeds the %d-byte limit" % MAX_CONTROL)
            raise ProtocolError("stream ended inside the control line")
        message = json.loads(line)
        if not isinstance(message, dict) or "type" not in message:
            raise ProtocolError("control message has no type")
        return message
    if first[0] != _FRAME_START:
        raise ProtocolError("unexpected byte 0x%02x at message start" % first[0])

    (h,) = _U32.unpack(_read_exact(stream, 4, "header length"))
    if h == 0 or h > MAX_HEADER:
        raise ProtocolError("header length %d is not 1..=%d" % (h, MAX_HEADER))
    header = json.loads(_read_exact(stream, h, "header"))
    kind = header.get("kind")
    payload = header.get("payload")
    (b,) = _U32.unpack(_read_exact(stream, 4, "body length"))
    if b > MAX_BODY:
        raise ProtocolError("body of %d bytes exceeds the %d-byte limit" % (b, MAX_BODY))
    if kind == "iq":
        width = 8
    elif kind == "audio":
        width = 4
    elif kind in ("event", "text"):
        width = 0
    else:
        raise ProtocolError("%s frames are not carried by plugin protocol 1" % kind)
    n = payload.get("samples", 0) if width else 0
    if n * width != b:
        raise ProtocolError("%s frame of %d samples has a %d-byte body" % (kind, n, b))
    body = _read_exact(stream, b, "body")
    if kind == "iq":
        flat = struct.unpack("<%df" % (2 * n), body)
        samples = [complex(flat[2 * i], flat[2 * i + 1]) for i in range(n)]
    elif kind == "audio":
        samples = list(struct.unpack("<%df" % n, body))
    else:
        samples = None
    return Frame(kind, header, samples, payload)


def run(kind, inputs, outputs, params_schema, on_frame, on_lagged=None):
    """Be a plugin: say hello, wait for `run`, call back per frame until the input closes.

    `on_frame(params, frame)` returns `None`, a `Frame` or a list of `Frame`s; each is sent
    on the first output stream of its kind. `on_lagged(params, missed)` is told how many
    input frames the host dropped. Returns on `closed`, `stop` or end of input (the process
    should then exit 0). An exception in a callback is sent to the host as
    `{"type":"error","message":"<ExceptionName>: <message>"}` and the process exits 1.

    Stdout carries the protocol, so while `run` runs `sys.stdout` is pointed at stderr:
    `print` goes to the host's log instead of corrupting the stream.
    """
    # Held for the whole run: dropping the last reference to a text wrapper closes its buffer.
    real_stdout = sys.stdout
    out = real_stdout.buffer
    stdin = sys.stdin.buffer
    sys.stdout = sys.stderr

    def send(data):
        out.write(data)
        out.flush()

    send(
        encode_control(
            {
                "type": "hello",
                "protocol": PROTOCOL,
                "kind": kind,
                "inputs": list(inputs),
                "outputs": list(outputs),
                "params_schema": params_schema,
            }
        )
    )
    try:
        start = read_message(stdin)
        if start is None or (isinstance(start, dict) and start["type"] == "stop"):
            return
        if not isinstance(start, dict) or start["type"] != "run":
            raise ProtocolError("expected run, got %r" % (start,))
        if start.get("protocol") != PROTOCOL:
            raise ProtocolError("host speaks protocol %r, not %d" % (start.get("protocol"), PROTOCOL))
        params = start.get("params")
        ports = start.get("outputs", [])

        def emit(result):
            if result is None:
                return
            for f in [result] if isinstance(result, Frame) else result:
                port = next((p for p in ports if p["kind"] == f.kind), None)
                if port is None:
                    raise ProtocolError("no %s output to send a frame on" % f.kind)
                send(encode_frame(Frame(f.kind, dict(f.header, stream=port["stream"]), f.samples, f.payload)))

        while True:
            m = read_message(stdin)
            if m is None:
                return
            if isinstance(m, Frame):
                emit(on_frame(params, m))
            elif m["type"] == "lagged":
                if on_lagged is not None:
                    emit(on_lagged(params, m["missed"]))
            elif m["type"] in ("closed", "stop"):
                return
            else:
                raise ProtocolError("unexpected control message %r" % m["type"])
    except BrokenPipeError:
        # The host has gone; there is no one to tell.
        sys.exit(1)
    except Exception as e:  # noqa: BLE001 - every failure is reported to the host
        traceback.print_exc()  # to stderr, which the host logs
        message = "%s: %s" % (type(e).__name__, e)
        try:
            # Cut to fit a control line whatever the exception said.
            send(encode_control({"type": "error", "message": message[:_MAX_ERROR]}))
        except Exception:  # noqa: BLE001
            pass
        sys.exit(1)
    finally:
        try:
            out.flush()
        except Exception:  # noqa: BLE001
            pass
