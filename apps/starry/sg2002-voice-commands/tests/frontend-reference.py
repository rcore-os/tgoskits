#!/usr/bin/env python3
"""CPU reference for the real frontend IPC contract, not a hardware emulator."""
import os
import struct
import sys

import numpy as np
import onnxruntime as ort


def write(data):
    while data:
        count = os.write(1, data)
        data = data[count:]


def read(count):
    data = bytearray()
    while len(data) < count:
        chunk = os.read(0, count - len(data))
        if not chunk:
            if not data:
                return None
            raise ValueError("truncated frame")
        data.extend(chunk)
    return data


options = ort.SessionOptions()
options.intra_op_num_threads = 1
options.inter_op_num_threads = 1
session = ort.InferenceSession(sys.argv[1], sess_options=options, providers=["CPUExecutionProvider"])
write(struct.pack("<5I", 0x46545056, 3600, 7296, 2048, 7296))
while True:
    x = read(3600 * 4)
    if x is None:
        break
    if os.environ.get("VOICE_TEST_EXIT_ON_REQUEST"):
        sys.exit(23)
    cache = read(7296 * 4)
    if cache is None:
        raise ValueError("missing state")
    values = {"x": np.frombuffer(x, "<f4").reshape(1, 45, 80),
              "embed_states": np.frombuffer(cache, "<f4").reshape(1, 128, 3, 19)}
    for output in session.run(None, values):
        write(output.astype("<f4").tobytes())
