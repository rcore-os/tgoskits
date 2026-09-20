#!/usr/bin/env python3
"""Check the real memcached text protocol over loopback TCP."""

import os
import socket
import subprocess
import tempfile
import time


def request(command):
    with socket.create_connection(("127.0.0.1", 11211), timeout=5) as client:
        client.sendall(command + b"quit\r\n")
        response = bytearray()
        deadline = time.monotonic() + 10
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("memcached response deadline exceeded")
            client.settimeout(remaining)
            chunk = client.recv(4096)
            if not chunk:
                return bytes(response)
            response.extend(chunk)
            if len(response) > 65536:
                raise RuntimeError("memcached response exceeds 64 KiB")


def expect(command, expected):
    actual = request(command)
    if actual != expected:
        raise RuntimeError(f"{command!r}: expected {expected!r}, got {actual!r}")


def check(server):
    deadline = time.monotonic() + 15
    while True:
        if server.poll() is not None:
            raise RuntimeError(f"memcached exited during startup: {server.returncode}")
        try:
            version = request(b"version\r\n")
            break
        except ConnectionRefusedError:
            if time.monotonic() >= deadline:
                raise TimeoutError("memcached did not start listening")
            time.sleep(0.1)
    if not version.startswith(b"VERSION ") or not version.endswith(b"\r\n"):
        raise RuntimeError(f"invalid version response: {version!r}")

    expect(b"set starry_key 0 0 12\r\nhello_starry\r\n", b"STORED\r\n")
    expect(b"get starry_key\r\n", b"VALUE starry_key 0 12\r\nhello_starry\r\nEND\r\n")
    expect(b"delete starry_key\r\n", b"DELETED\r\n")
    expect(b"get starry_key\r\n", b"END\r\n")
    expect(b"delete starry_key\r\n", b"NOT_FOUND\r\n")

    lines = request(b"stats\r\n").split(b"\r\n")
    if lines[-2:] != [b"END", b""]:
        raise RuntimeError(f"incomplete stats response: {lines!r}")
    stats = {}
    for line in lines[:-2]:
        prefix, name, value = line.split(b" ", 2)
        if prefix != b"STAT" or name in stats:
            raise RuntimeError(f"invalid stats entry: {line!r}")
        stats[name] = value
    if int(stats[b"pid"]) != server.pid:
        raise RuntimeError("stats belong to another memcached process")
    for name, expected in ((b"cmd_set", 1), (b"get_hits", 1), (b"get_misses", 1)):
        if int(stats[name]) != expected:
            raise RuntimeError(f"unexpected {name!r}: {stats[name]!r}")
    if server.poll() is not None:
        raise RuntimeError("memcached exited during protocol checks")


def main():
    print("MEMCACHED_TEST_BEGIN", flush=True)
    with tempfile.TemporaryFile() as log:
        args = ["memcached", "-l", "127.0.0.1", "-p", "11211", "-U", "0",
                "-m", "16", "-c", "16", "-t", "1"]
        if os.geteuid() == 0:
            args.extend(["-u", "root"])
        server = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT)
        try:
            check(server)
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
            log.seek(0)
            print(log.read().decode(errors="replace"), end="", flush=True)
    print("MEMCACHED_TEST_PASSED", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"memcached: {error}", flush=True)
        print("MEMCACHED_TEST_FAILED", flush=True)
        raise SystemExit(1)
