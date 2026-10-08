#!/usr/bin/env python3
"""Check listen.sh process ownership with producer/decoder boundaries, not ALSA."""
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time


BOUNDARY = '''import os, pathlib, signal, sys, time
root = pathlib.Path(os.environ["BOUNDARY_ROOT"])
mode = os.environ["BOUNDARY_MODE"]
role = "capture" if pathlib.Path(sys.argv[0]).name == "arecord" else "decoder"
if role == "capture":
    assert "--fatal-errors" in sys.argv
else:
    assert sys.argv[1] == "--raw-file"
    if mode == "decoder-init-fail":
        sys.exit(13)
    stream = open(sys.argv[2], "rb")
if mode == "term" or (mode == "decoder-fail" and role == "capture"):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
(root / (role + ".pid")).write_text(str(os.getpid()))
if role == "capture":
    os.write(1, b"\\0\\0")
    if mode in ("success", "capture-fail"):
        sys.exit(7 if mode == "capture-fail" else 0)
    while True:
        signal.pause()
if mode == "decoder-fail":
    sys.exit(13)
if mode == "term":
    while True:
        signal.pause()
assert stream.read() == b"\\0\\0"
'''


def check(shell, source, mode, expected):
    with tempfile.TemporaryDirectory(prefix="voice-listen-test-") as directory:
        root = pathlib.Path(directory)
        shutil.copyfile(source, root / "listen.sh")
        for name in ("arecord", "run.sh"):
            path = root / name
            path.write_text(f"#!{sys.executable}\n" + BOUNDARY)
            path.chmod(0o755)
        temporary = root / "tmp"
        temporary.mkdir()
        env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                   TMPDIR=str(temporary), BOUNDARY_ROOT=str(root), BOUNDARY_MODE=mode)
        process = subprocess.Popen([*shell, str(root / "listen.sh")], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        passed = False
        try:
            if mode == "term":
                deadline = time.monotonic() + 5
                while not all((root / (role + ".pid")).exists()
                              for role in ("capture", "decoder")):
                    assert process.poll() is None, "listener exited before readiness"
                    assert time.monotonic() < deadline, "children did not become ready"
                    time.sleep(0.01)
                # Deliberately signal only the parent, never its process group.
                process.send_signal(signal.SIGTERM)
            output, errors = process.communicate(timeout=8)
            assert process.returncode == expected, (mode, process.returncode, output, errors)
            if mode == "decoder-init-fail":
                assert not (root / "capture.pid").exists(), "capture started before FIFO opened"
            assert not list(temporary.iterdir()), "FIFO or temporary directory leaked"
            for path in root.glob("*.pid"):
                pid = int(path.read_text())
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    continue
                raise AssertionError(f"{mode}: child {pid} was not reaped")
            passed = True
        finally:
            # Also bound a failing regression against the old implementation.
            if not passed:
                for path in root.glob("*.pid"):
                    try:
                        os.kill(int(path.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            if process.poll() is None:
                try:
                    process.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=3)
        print(f"PASS {' '.join(shell)} {mode} exit={expected}", flush=True)


def main():
    source = pathlib.Path(__file__).resolve().parents[1] / "listen.sh"
    shells = [["/bin/sh"]]
    busybox = shutil.which("busybox")
    if busybox:
        shells.append([busybox, "sh"])
    for shell in shells:
        for mode, expected in (("success", 0), ("capture-fail", 7),
                               ("decoder-fail", 13), ("decoder-init-fail", 13), ("term", 143)):
            check(shell, source, mode, expected)
    print("VOICE_LISTEN_TEST_PASSED")


if __name__ == "__main__":
    main()
