#!/usr/bin/env python3
"""A ZetaSDR plugin stage: iq in, iq out, every sample multiplied by `params.factor`.

Register it with the absolute path of `python3` and the arguments `-I <path to gain.py>`
(or run the file directly: it is executable). The SDK is found relative to this file, so it
needs neither PYTHONPATH nor the working directory.
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "sdk", "python"))

import zeta_plugin  # noqa: E402

PARAMS_SCHEMA = {
    "type": "object",
    "properties": {"factor": {"type": "number"}},
    "required": ["factor"],
    "additionalProperties": False,
}


def on_frame(params, frame):
    return frame.with_samples([s * params["factor"] for s in frame.samples])


if __name__ == "__main__":
    zeta_plugin.run("gain", ["iq"], ["iq"], PARAMS_SCHEMA, on_frame)
