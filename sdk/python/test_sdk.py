"""Unit tests for zeta_plugin: its codec against hand-built protocol 1 bytes, and `run`.

Run with `python3 -I -m unittest discover -s sdk/python` (the plugin crate's tests do).
"""

import io
import json
import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import zeta_plugin  # noqa: E402
from zeta_plugin import Frame, ProtocolError, encode_control, encode_frame, read_message  # noqa: E402


def u32(n):
    return struct.pack("<I", n)


def data_frame(header, body):
    """`0x00 | u32 LE header_len | header | u32 LE body_len | body`, built by hand."""
    return b"\x00" + u32(len(header)) + header + u32(len(body)) + body


# The host's encoding (serde field order, the payload's keys sorted) of an iq frame of
# [1+2j, -0.5+0.25j].
IQ_HEADER = (
    b'{"type":"frame","stream":"s1","kind":"iq","source":"src","job":null,"channel":"c1",'
    b'"start_ns":10,"end_ns":20,'
    b'"payload":{"center_hz":7100000,"sample_rate_hz":48000,"samples":2}}'
)
# cf32_le: I then Q, f32 little-endian: 1.0, 2.0, -0.5, 0.25.
IQ_BODY = bytes.fromhex("0000803f" "00000040" "000000bf" "0000803e")
IQ = data_frame(IQ_HEADER, IQ_BODY)

AUDIO_HEADER = (
    b'{"type":"frame","stream":"s1","kind":"audio","source":"src","job":"j1","channel":null,'
    b'"start_ns":10,"end_ns":20,"payload":{"peak":0.5,"sample_rate_hz":12000,"samples":2}}'
)
AUDIO = data_frame(AUDIO_HEADER, bytes.fromhex("0000803e" "000000bf"))  # 0.25, -0.5

EVENT = data_frame(
    b'{"type":"frame","stream":"s1","kind":"event","source":"src","job":null,"channel":null,'
    b'"start_ns":10,"end_ns":20,"payload":{"freq_hz":7074000,"label":"ft8"}}',
    b"",
)


def decode(data):
    return read_message(io.BytesIO(data))


class Codec(unittest.TestCase):
    def test_decodes_iq_frame(self):
        f = decode(IQ)
        self.assertIsInstance(f, Frame)
        self.assertEqual(f.kind, "iq")
        self.assertEqual(f.samples, [complex(1, 2), complex(-0.5, 0.25)])
        self.assertEqual(f.header["stream"], "s1")
        self.assertEqual((f.header["start_ns"], f.header["end_ns"]), (10, 20))
        self.assertEqual(f.payload, {"center_hz": 7100000, "sample_rate_hz": 48000, "samples": 2})

    def test_iq_round_trips_byte_for_byte(self):
        self.assertEqual(encode_frame(decode(IQ)), IQ)

    def test_encodes_hand_built_iq_frame(self):
        header = json.loads(IQ_HEADER)
        f = Frame("iq", header, [complex(1, 2), complex(-0.5, 0.25)])
        self.assertEqual(encode_frame(f), IQ)

    def test_audio_round_trips(self):
        f = decode(AUDIO)
        self.assertEqual(f.samples, [0.25, -0.5])
        self.assertEqual(encode_frame(f), AUDIO)

    def test_event_round_trips(self):
        f = decode(EVENT)
        self.assertIsNone(f.samples)
        self.assertEqual(f.payload, {"freq_hz": 7074000, "label": "ft8"})
        self.assertEqual(encode_frame(f), EVENT)

    def test_text_frame_has_empty_body(self):
        f = Frame("text", {"stream": "s1", "payload": "hi"})
        self.assertTrue(encode_frame(f).endswith(u32(0)))

    def test_with_samples_updates_count_and_copies(self):
        f = decode(IQ)
        g = f.with_samples([complex(0.5, 1)])
        self.assertEqual(g.samples, [complex(0.5, 1)])
        self.assertEqual(g.payload["samples"], 1)
        self.assertEqual(g.header["payload"]["samples"], 1)
        self.assertEqual(f.payload["samples"], 2)
        self.assertEqual(len(encode_frame(g)), len(IQ) - 8)

    def test_with_samples_updates_audio_peak(self):
        g = decode(AUDIO).with_samples([0.1, -0.75])
        self.assertEqual(g.payload["peak"], 0.75)

    def test_control_lines(self):
        hello = encode_control(
            {
                "type": "hello",
                "protocol": 1,
                "kind": "gain",
                "inputs": ["iq"],
                "outputs": ["iq"],
                "params_schema": {"type": "object", "additionalProperties": False},
            }
        )
        self.assertEqual(
            hello,
            b'{"type":"hello","protocol":1,"kind":"gain","inputs":["iq"],"outputs":["iq"],'
            b'"params_schema":{"type":"object","additionalProperties":false}}\n',
        )
        self.assertEqual(decode(b'{"type":"lagged","missed":7}\n'), {"type": "lagged", "missed": 7})
        self.assertIsNone(decode(b""))

    def test_refuses_malformed_input(self):
        cases = [
            (b"A", "unexpected byte 0x41"),
            (IQ[:-3], "inside the body"),
            (data_frame(IQ_HEADER, IQ_BODY[:8]), "2 samples has a 8-byte body"),
            (b"\x00" + u32(0), "header length 0"),
            (b'{"type":"stop"}', "inside the control line"),
            (b"{" + b" " * zeta_plugin.MAX_CONTROL + b"}\n", "limit"),
        ]
        for data, want in cases:
            with self.assertRaises(ProtocolError) as cm:
                decode(data)
            self.assertIn(want, str(cm.exception))


def drive(on_frame, host_bytes):
    """Run a plugin over `host_bytes` with stdio swapped; its exit code and what it wrote."""
    out = io.BytesIO()
    saved = sys.stdin, sys.stdout
    # Kept until `out` is read: a text wrapper closes its buffer when it is dropped.
    wrappers = io.TextIOWrapper(io.BytesIO(host_bytes)), io.TextIOWrapper(out)
    sys.stdin, sys.stdout = wrappers
    code = 0
    try:
        zeta_plugin.run("gain", ["iq"], ["iq"], {"type": "object", "additionalProperties": False}, on_frame)
    except SystemExit as e:
        code = e.code
    finally:
        sys.stdin, sys.stdout = saved
    return code, io.BytesIO(out.getvalue())


RUN = encode_control(
    {"type": "run", "protocol": 1, "params": {"factor": 2}, "inputs": [{"kind": "iq"}],
     "outputs": [{"stream": "out7", "kind": "iq"}]}
)


class Run(unittest.TestCase):
    def test_hello_then_frames_on_the_run_output(self):
        def double(params, frame):
            return frame.with_samples([s * params["factor"] for s in frame.samples])

        closed = encode_control({"type": "closed", "reason": "input closed"})
        code, out = drive(double, RUN + IQ + closed)
        self.assertEqual(code, 0)
        hello = read_message(out)
        self.assertEqual(hello["type"], "hello")
        self.assertEqual(hello["kind"], "gain")
        f = read_message(out)
        self.assertEqual(f.header["stream"], "out7")
        self.assertEqual(f.samples, [complex(2, 4), complex(-1, 0.5)])
        self.assertEqual((f.header["start_ns"], f.payload["center_hz"]), (10, 7100000))
        self.assertIsNone(read_message(out))

    def test_stop_before_run_ends_cleanly(self):
        code, out = drive(lambda p, f: None, encode_control({"type": "stop"}))
        self.assertEqual(code, 0)
        self.assertEqual(read_message(out)["type"], "hello")
        self.assertIsNone(read_message(out))

    def test_callback_exception_is_an_error_message_and_exit_1(self):
        def bad(params, frame):
            raise ValueError("bad block")

        code, out = drive(bad, RUN + IQ)
        self.assertEqual(code, 1)
        read_message(out)  # hello
        self.assertEqual(read_message(out), {"type": "error", "message": "ValueError: bad block"})


if __name__ == "__main__":
    unittest.main()
