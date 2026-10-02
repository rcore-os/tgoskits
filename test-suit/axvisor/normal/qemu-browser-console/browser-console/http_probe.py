#!/usr/bin/env python3
"""Validate the minimal Axvisor browser console through QEMU hostfwd."""

import base64
import json
import os
import socket
import struct
import time
import urllib.error
import urllib.parse
import urllib.request


BASE = os.environ.get("AXVISOR_HTTP_BASE", "http://127.0.0.1:8080").rstrip("/")
CONNECT_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "5"))
POLL_INTERVAL = 0.05
BURST_CHARACTER_COUNT = 96
BURST_CHARACTER_INTERVAL = 0.001
BURST_FRAME_LIMIT = 24


def get(path):
    """Return one HTTP GET status and body without treating errors as fatal."""
    request = urllib.request.Request(BASE + path, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def expect_status(label, actual, expected):
    if actual != expected:
        raise AssertionError("%s returned %d, expected %d" % (label, actual, expected))
    print("  browser console probe: %s -> %d" % (label, actual))


def poll_ready():
    """Wait for guest HTTP, not merely QEMU's already-open hostfwd socket."""
    deadline = time.monotonic() + CONNECT_TIMEOUT
    last_error = None
    while time.monotonic() < deadline:
        try:
            status, _ = get("/api/consoles")
            if status == 200:
                print("  browser console probe: guest HTTP server reachable")
                return
            last_error = "HTTP %d" % status
        except (OSError, urllib.error.URLError) as error:
            last_error = str(error)
        time.sleep(POLL_INTERVAL)
    raise AssertionError("guest HTTP server did not become ready: %s" % last_error)


class WebSocket:
    """Small RFC 6455 client sufficient for the in-guest console checks."""

    def __init__(self, stream, buffered):
        self.stream = stream
        self.buffered = buffered

    def recv_exact(self, length):
        while len(self.buffered) < length:
            chunk = self.stream.recv(length - len(self.buffered))
            if not chunk:
                raise AssertionError("WebSocket closed while receiving a frame")
            self.buffered += chunk
        output = self.buffered[:length]
        self.buffered = self.buffered[length:]
        return output

    def recv_frame(self):
        first, second = self.recv_exact(2)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", self.recv_exact(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self.recv_exact(8))[0]
        if second & 0x80:
            mask = self.recv_exact(4)
            payload = bytes(
                byte ^ mask[index % 4]
                for index, byte in enumerate(self.recv_exact(length))
            )
        else:
            payload = self.recv_exact(length)
        return opcode, payload

    def send_binary(self, payload):
        mask = os.urandom(4)
        length = len(payload)
        if length < 126:
            header = bytes([0x82, 0x80 | length])
        elif length <= 0xFFFF:
            header = bytes([0x82, 0x80 | 126]) + struct.pack("!H", length)
        else:
            header = bytes([0x82, 0x80 | 127]) + struct.pack("!Q", length)
        masked = bytes(
            byte ^ mask[index % 4] for index, byte in enumerate(payload)
        )
        self.stream.sendall(header + mask + masked)

    def close(self):
        self.stream.close()


def open_websocket(path, origin=None):
    parsed = urllib.parse.urlsplit(BASE)
    host = parsed.hostname
    port = parsed.port or 80
    authority = "%s:%d" % (host, port)
    origin = origin or BASE
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    stream = socket.create_connection((host, port), timeout=REQUEST_TIMEOUT)
    stream.settimeout(REQUEST_TIMEOUT)
    request = (
        "GET %s HTTP/1.1\r\n"
        "Host: %s\r\n"
        "Origin: %s\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Key: %s\r\n\r\n"
    ) % (path, authority, origin, key)
    stream.sendall(request.encode("ascii"))
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = stream.recv(4096)
        if not chunk:
            raise AssertionError("HTTP server closed during WebSocket upgrade")
        response += chunk
    response_head, buffered = response.split(b"\r\n\r\n", 1)
    return WebSocket(stream, buffered), response_head + b"\r\n\r\n"


def expect_upgrade(path, expected, origin=None):
    websocket, response = open_websocket(path, origin)
    prefix = ("HTTP/1.1 %d" % expected).encode("ascii")
    if not response.startswith(prefix):
        websocket.close()
        raise AssertionError("%s upgrade returned %r" % (path, response))
    return websocket


def receive_until(websocket, marker):
    output = b""
    deadline = time.monotonic() + REQUEST_TIMEOUT
    while marker not in output:
        if time.monotonic() >= deadline:
            raise AssertionError("WebSocket output did not contain %r" % marker)
        opcode, payload = websocket.recv_frame()
        if opcode in (1, 2):
            output += payload
        elif opcode == 8:
            raise AssertionError("WebSocket closed before output marker %r" % marker)
    return output


def receive_until_counting_frames(websocket, marker):
    output = b""
    frames = 0
    deadline = time.monotonic() + REQUEST_TIMEOUT
    while marker not in output:
        if time.monotonic() >= deadline:
            raise AssertionError("WebSocket output did not contain %r" % marker)
        opcode, payload = websocket.recv_frame()
        if opcode in (1, 2):
            output += payload
            frames += 1
        elif opcode == 8:
            raise AssertionError("WebSocket closed before output marker %r" % marker)
    return output, frames


def check_gateway():
    """The console gateway of a build without a dashboard.

    This case builds `browser-console` alone: it is the terminal gateway the
    dashboard drives, and it deliberately owns no page. `/` and `/assets/*` stay
    404 here, which is what keeps "the UI is the embedded dashboard" true — the
    dashboard case (`qemu-web-ui`) asserts the other half of that contract.
    """
    status, _ = get("/")
    expect_status("GET / (no dashboard in this build)", status, 404)
    status, _ = get("/assets/xterm.js")
    expect_status("GET /assets/xterm.js (page asset is gone)", status, 404)

    status, body = get("/api/consoles")
    expect_status("GET /api/consoles", status, 200)
    consoles = json.loads(body.decode("utf-8"))
    expected = [{"route": "axvisor", "name": "Axvisor", "attached": False}]
    if consoles != expected:
        raise AssertionError("console snapshot returned %r, expected %r" % (consoles, expected))

    status, _ = get("/api/vms")
    expect_status("GET /api/vms without http-axum", status, 404)

    # The capability declaration has to describe this build: a console-only
    # build offers both terminals and no VM management, so a browser that built
    # its navigation from the manifest never calls a route this build lacks.
    status, body = get("/api/manifest")
    expect_status("GET /api/manifest", status, 200)
    manifest = json.loads(body.decode("utf-8"))
    if manifest.get("proto") != 1:
        raise AssertionError("manifest proto was %r, expected 1" % (manifest.get("proto"),))
    panels = manifest.get("panels")
    if not isinstance(panels, list):
        raise AssertionError("manifest panels was not a list: %r" % (manifest,))
    if [panel.get("kind") for panel in panels] != ["console", "shell"]:
        raise AssertionError("manifest panel kinds were %r" % (panels,))
    for panel in panels:
        if panel.get("verbs") != ["read", "write", "stream"]:
            raise AssertionError("manifest panel %r had unexpected verbs" % (panel,))
        if not panel.get("title"):
            raise AssertionError("manifest panel %r had no title" % (panel,))
        if not panel.get("root"):
            raise AssertionError("manifest panel %r had no root" % (panel,))
    check_manifest_links(panels)
    print("  browser console probe: GET /api/manifest -> console + shell")


def request_status(method, path):
    """One request, returning only the status.

    The link check asks whether a route answers the method it declares, so the
    body is not parsed: an empty `POST` body is refused by the JSON extractor
    with a plain-text 4xx, which is still a registered route answering. Transport
    errors are retried while the guest server is busy.
    """
    deadline = time.monotonic() + CONNECT_TIMEOUT
    while True:
        request = urllib.request.Request(BASE + path, method=method)
        try:
            with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
                return response.status
        except urllib.error.HTTPError as error:
            return error.code
        except (OSError, urllib.error.URLError) as error:
            if time.monotonic() > deadline:
                raise AssertionError("%s %s never answered: %s" % (method, path, error))
            time.sleep(POLL_INTERVAL)


def check_manifest_links(panels):
    """Assert every declared link is served, not merely declared.

    A link whose method its route does not implement answers 405, so calling
    each declared link is what makes the declaration falsifiable: a table entry
    whose method and route disagree cannot pass. `{endpoint}` is filled with the
    management lane, so every call stays side-effect free.
    """
    calls = 0
    for panel in panels:
        links = panel.get("links")
        if not isinstance(links, list) or not links:
            raise AssertionError("manifest panel %r declared no links" % (panel,))
        for link in links:
            if not link.get("name") or not link.get("verb"):
                raise AssertionError("manifest link had no name/verb: %r" % (link,))
            path = link["href"].replace("{endpoint}", "axvisor")
            status = request_status(link["method"], path)
            if status == 405:
                raise AssertionError(
                    "%s link %s %s is declared but not served"
                    % (panel.get("kind"), link["method"], path)
                )
            calls += 1
    print("  browser console probe: manifest links all served (%d)" % calls)


def check_websocket():
    websocket = expect_upgrade("/ws/axvisor", 101)
    receive_until(websocket, b"Welcome to AxVisor Browser Shell!")
    websocket.send_binary(b"help\r")
    help_output = receive_until(websocket, b"axvisor:$ ")
    if b"ArceOS Shell - Available Commands:" not in help_output:
        raise AssertionError("WebSocket help output was incomplete")
    print("  browser console probe: Axvisor WebSocket -> interactive")

    for _ in range(BURST_CHARACTER_COUNT):
        websocket.send_binary(b"x")
        time.sleep(BURST_CHARACTER_INTERVAL)
    websocket.send_binary(b"\r")
    burst_output, burst_frames = receive_until_counting_frames(websocket, b"axvisor:$ ")
    if b"x" * BURST_CHARACTER_COUNT not in burst_output:
        raise AssertionError("WebSocket burst output lost or reordered echoed bytes")
    if burst_frames > BURST_FRAME_LIMIT:
        raise AssertionError(
            "WebSocket burst used %d frames for %d characters, expected at most %d"
            % (burst_frames, BURST_CHARACTER_COUNT, BURST_FRAME_LIMIT)
        )
    print(
        "  browser console probe: %d paced bytes -> %d output frames"
        % (BURST_CHARACTER_COUNT, burst_frames)
    )

    duplicate = expect_upgrade("/ws/axvisor", 409)
    duplicate.close()
    print("  browser console probe: duplicate Axvisor WebSocket -> 409")

    rejected = expect_upgrade("/ws/axvisor", 403, "https://unrelated.example")
    rejected.close()
    print("  browser console probe: cross-origin WebSocket -> 403")

    missing = expect_upgrade("/ws/vm-1", 404)
    missing.close()
    print("  browser console probe: absent VM WebSocket -> 404")

    websocket.close()
    deadline = time.monotonic() + REQUEST_TIMEOUT
    while True:
        reopened, response = open_websocket("/ws/axvisor")
        if response.startswith(b"HTTP/1.1 101"):
            reopened.close()
            break
        reopened.close()
        if not response.startswith(b"HTTP/1.1 409") or time.monotonic() >= deadline:
            raise AssertionError("released Axvisor WebSocket did not reopen: %r" % response)
        time.sleep(POLL_INTERVAL)
    print("  browser console probe: released Axvisor WebSocket -> reusable")


def main():
    poll_ready()
    check_gateway()
    check_websocket()
    print("  browser console probe: PASS")


if __name__ == "__main__":
    main()
