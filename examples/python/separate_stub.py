#!/usr/bin/env -S python3 -I
"""A ZetaSDR plugin stage that fills the source-separation slot: audio in, audio out.

Params: `{"model": string}`. This stub passes the audio through unchanged and prints
`separate: model <m> (stub)` once to stderr (the host's log).

A real separator (Demucs or a similar vocal-stem model, on a GPU host) replaces the body of
`on_frame`: it receives 48 kHz mono f32 frames and returns the vocal stem at the same sample
rate and the same length (`frame.with_samples(stem)`). Seconds of latency are acceptable:
`vad` and `transcribe` work on the frames' `t0`, not on arrival time. Load the model once,
lazily, on the first frame.

Register it with `--plugin separate=<absolute path of this file>` (it is executable, and
finds the SDK relative to its own file), then replace the `denoise` node of an attached
transcription job with `{"kind":"separate","params":{"model":"..."}}` through `graph get`
and `graph put`.
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "sdk", "python"))

import zeta_plugin  # noqa: E402

PARAMS_SCHEMA = {
    "type": "object",
    "properties": {"model": {"type": "string"}},
    "required": ["model"],
    "additionalProperties": False,
}

_announced = False


def on_frame(params, frame):
    global _announced
    if not _announced:
        _announced = True
        print("separate: model %s (stub)" % params["model"], file=sys.stderr, flush=True)
    return frame  # a real separator returns frame.with_samples(vocal_stem)


if __name__ == "__main__":
    zeta_plugin.run("separate", ["audio"], ["audio"], PARAMS_SCHEMA, on_frame)
