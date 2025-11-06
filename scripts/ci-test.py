#!/usr/bin/env python3

import argparse
import socket
import subprocess
import sys
import threading

parser = argparse.ArgumentParser()
parser.add_argument("arch")

args = parser.parse_args()
arch = args.arch

p = subprocess.Popen(
    [
        "make",
        "ARCH=" + arch,
        "ACCEL=n",
        "justrun",
        "QEMU_ARGS=-monitor none -serial tcp::4444,server=on",
    ],
    stderr=subprocess.PIPE,
    text=True,
)

ready = threading.Event()


def worker():
    for line in p.stderr:
        print(line, file=sys.stderr, end="")
        if "QEMU waiting for connection" in line:
            ready.set()
    ready.set()


thread = threading.Thread(target=worker)
thread.daemon = True
thread.start()

try:
    if not ready.wait(timeout=5):
        raise Exception("QEMU did not start in time")
    if p.poll() is not None:
        raise Exception("QEMU exited prematurely")

    PROMPT = "starry:~#"

    s = socket.create_connection(("localhost", 4444), timeout=5)
    buffer = ""
    while True:
        try:
            b = s.recv(1024).decode("utf-8", errors="ignore")
        except ConnectionError as e:
            print(e)
            break
        if not b:
            break
        print(b, end="")
        buffer += b
        if PROMPT in buffer:
            s.sendall(b"exit\r\n")

    if PROMPT not in buffer:
        raise Exception("Did not reach BusyBox shell prompt")

    print()
    print("\x1b[32m✔ Boot into BusyBox shell\x1b[0m")
except Exception:
    print("\x1b[31m❌ Boot failed or timed out\x1b[0m")
    raise
finally:
    try:
        p.wait(1)
    except subprocess.TimeoutExpired:
        p.terminate()
        p.wait()
