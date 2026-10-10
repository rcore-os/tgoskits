#!/usr/bin/env python3
"""Exercise the actual recognizer with speech, non-commands and streamed PCM."""

import argparse
import json
import os
import pathlib
import select
import struct
import subprocess
import tempfile
import time
import wave


def invoke(binary, model, source, *, audio=None, exit_code=0):
    arguments = source if isinstance(source, list) else [source]
    result = subprocess.run([str(binary), str(model), *map(str, arguments)], input=audio,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if result.returncode != exit_code:
        raise AssertionError(f"{source}: exit {result.returncode}\n"
                             + result.stderr.decode(errors="replace"))
    events = [json.loads(line) for line in result.stdout.splitlines()]
    assert all(set(e) == {"command", "time"} for e in events), events
    assert all(e["time"] >= 0 for e in events), events
    assert [e["time"] for e in events] == sorted(e["time"] for e in events), events
    return [e["command"] for e in events]


def streamed(binary, model, pcm):
    # One deadline includes pipe backpressure, EOF and final decoding.
    deadline = time.monotonic() + 120
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        process = subprocess.Popen([str(binary), str(model), "--raw-stdin"],
                                   stdin=subprocess.PIPE, stdout=output, stderr=errors,
                                   bufsize=0)
        try:
            fd = process.stdin.fileno()
            os.set_blocking(fd, False)
            offset = 0
            while offset < len(pcm):
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([], [fd], [], remaining)[1]:
                    raise TimeoutError("streamed PCM write deadline exceeded")
                try:
                    offset += os.write(fd, pcm[offset:offset + 317])
                except (BlockingIOError, InterruptedError):
                    continue
            process.stdin.close()
            status = process.wait(timeout=max(0, deadline - time.monotonic()))
            errors.seek(0)
            assert status == 0, (status, errors.read().decode(errors="replace"))
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdin.close()
        output.seek(0)
        return [json.loads(line)["command"] for line in output]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("model", type=pathlib.Path)
    parser.add_argument("fixtures", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    model = args.model.resolve()
    expected = ["forward", "backward", "left", "right", "stop"]
    commands = args.fixtures / "commands.wav"
    actual = invoke(binary, model, commands)
    assert actual == expected, (actual, expected)
    assert invoke(binary, model, args.fixtures / "noncommands.wav") == []
    with wave.open(str(commands)) as recording:
        assert (recording.getnchannels(), recording.getsampwidth(),
                recording.getframerate()) == (1, 2, 16000)
        pcm = recording.readframes(recording.getnframes())
    assert invoke(binary, model, "--raw-stdin", audio=pcm) == expected
    # A pipe writer may split samples and inference windows at any byte boundary.
    assert streamed(binary, model, pcm) == expected
    assert invoke(binary, model, "--raw-stdin", audio=b"\0" * 96000) == []
    invoke(binary, model, "--raw-stdin", audio=b"", exit_code=1)
    invoke(binary, model, "--raw-stdin", audio=b"\0", exit_code=1)
    invoke(binary, model, "--unsupported", exit_code=2)
    invoke(binary, model / "missing", commands, exit_code=1)
    with tempfile.TemporaryDirectory() as directory:
        raw = pathlib.Path(directory) / "commands.pcm"
        raw.write_bytes(pcm)
        assert invoke(binary, model, ["--raw-file", raw]) == expected
        raw.write_bytes(pcm + b"\0")
        invoke(binary, model, ["--raw-file", raw], exit_code=1)
        invoke(binary, model, ["--raw-file", pathlib.Path(directory) / "missing.pcm"],
               exit_code=1)
        invalid_rate = pathlib.Path(directory) / "rate.wav"
        with wave.open(str(invalid_rate), "wb") as recording:
            recording.setnchannels(1)
            recording.setsampwidth(2)
            recording.setframerate(48000)
            recording.writeframes(pcm)
        invoke(binary, model, invalid_rate, exit_code=1)
        invoke(binary, model, pathlib.Path(directory) / "missing.wav", exit_code=1)
        stereo = pathlib.Path(directory) / "stereo.wav"
        with wave.open(str(stereo), "wb") as recording:
            recording.setnchannels(2)
            recording.setsampwidth(2)
            recording.setframerate(16000)
            recording.writeframes(b"".join(pcm[i:i + 2] * 2 for i in range(0, len(pcm), 2)))
        invoke(binary, model, stereo, exit_code=1)
        encoded = commands.read_bytes()
        invalid = pathlib.Path(directory) / "invalid.wav"
        # The actual reader must reject incomplete containers and sample payloads,
        # not rely on the Sherpa SDK version's handling of malformed input.
        for data in (b"", b"not a WAV", encoded[:11], encoded[:-1],
                     encoded[:4] + struct.pack("<I", 4) + encoded[8:]):
            invalid.write_bytes(data)
            invoke(binary, model, invalid, exit_code=1)
        # Unknown RIFF chunks, including odd-size padding, are not audio samples.
        metadata = pathlib.Path(directory) / "metadata.wav"
        extra = b"JUNK" + struct.pack("<I", 3) + b"abc\0"
        metadata.write_bytes(encoded[:4] + struct.pack("<I", len(encoded) - 8 + len(extra))
                             + encoded[8:12] + extra + encoded[12:])
        assert invoke(binary, model, metadata) == expected
    # Output failures must remain ordinary nonzero exits, not SIGPIPE deaths.
    read_fd, write_fd = os.pipe()
    os.close(read_fd)
    with os.fdopen(write_fd, "wb") as output:
        result = subprocess.run([str(binary), str(model), str(commands)], stdout=output,
                                stderr=subprocess.PIPE, timeout=120)
        assert result.returncode == 1, (result.returncode, result.stderr)
    with open("/dev/full", "wb") as output:
        result = subprocess.run([str(binary), str(model), str(commands)], stdout=output,
                                stderr=subprocess.PIPE, timeout=120)
        assert result.returncode == 1, (result.returncode, result.stderr)
    print("Base input, streamed PCM, error exit and output failure checks passed", flush=True)
    repeated = invoke(binary, model, "--raw-stdin", audio=pcm + pcm)
    assert repeated == expected * 2, (repeated, expected * 2)
    delayed = invoke(binary, model, "--raw-stdin", audio=b"\0" * (10 * 16000 * 2) + pcm + pcm)
    assert delayed == expected * 2, (delayed, expected * 2)
    print("VOICE_COMMANDS_TEST_PASSED")


if __name__ == "__main__":
    main()
