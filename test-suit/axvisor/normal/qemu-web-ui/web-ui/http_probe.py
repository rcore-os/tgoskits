#!/usr/bin/env python3
"""Host-side probe asset for the AxVisor management dashboard.

Case asset for the `qemu-web-ui` test case
(`test-suit/axvisor/normal/qemu-web-ui/`). It owns the test content — the
concrete requests and assertions — and can evolve independently of the axbuild
runner.

The generic axbuild probe runner
(`scripts/axbuild/src/axvisor/test/http_probe.rs`) executes this script after
the QEMU hostfwd port is reachable, then treats the exit code as the verdict:
0 = all assertions passed, nonzero = a step failed. The script dials the axum
management API running *inside* AxVisor through QEMU user-mode networking
hostfwd. Nothing in the hypervisor knows a test is running.

This build is `web-ui` + `browser-console` with `no-auto-start`, so the
dashboard, the terminal gateway and the VM registry are all live at once and
the default guest stays `Ready` until the probe starts it. The probe covers the
contract a browser depends on, not a browser:

    GET    /                      -> 200   (dashboard shell; CSP + no-cache + nosniff)
    GET    /assets/{hashed}       -> 200   (every asset the shell references and
                                            every chunk + stylesheet those refer
                                            to, all immutable; the graph has to
                                            hold the lazy panel chunks and a
                                            stylesheet)
    GET    /no-such-page          -> 404   (no SPA catch-all)
    GET    /assets/no-such.js     -> 404   (asset table is exact)
    <bundle>                      -> holds every endpoint the UI calls
    GET    /api/manifest          -> 200   (vms + console + shell panels)
    GET    /api/consoles          -> 200   (management lane + the default VM's lane,
                                            each with a boolean `attached`)
    GET    /ws/axvisor            -> 101   (management shell: help output)
    GET    /ws/vm-1               -> 101   (guest lane greeting)
    GET    /ws/events             -> 101   (snapshot, then created/removed frames)
    /ws/vm-1                      -> rejected input while the guest is stopped
    POST   /api/vms/1/start       -> 200   (guest really enters: guest_entry_count)
    /ws/vm-1                      -> guest shell runs a typed command
    POST   /api/vms/1/stop        -> 200   (guest settles, console lane kept,
                                            input rejected again)
    DELETE /api/vms/1             -> 204   (registry, lane and event frame follow)
    POST   /api/vms/create        -> 200   (recreate from the fixture TOML)
    DELETE /api/vms/1             -> 204   (cleanup)

The bundle assertions are the reason this case exists beyond the API cases: a
stale or mismatched `web-ui/dist` still builds and still serves a page, but it
would call endpoints this contract check pins down. They are byte-level checks
on the served asset, so they run without a browser.

Environment (set by the generic runner):

    AXVISOR_HTTP_BASE            http://127.0.0.1:<host_port> (forwarded)
    AXVISOR_HTTP_CASE_DIR        case directory holding `vm-linux-alpine.toml`
    AXVISOR_HTTP_CONNECT_TIMEOUT seconds for the initial reachability wait
    AXVISOR_HTTP_REQUEST_TIMEOUT seconds per HTTP request
"""

import base64
import json
import os
import re
import select
import socket
import struct
import time
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("AXVISOR_HTTP_BASE", "http://127.0.0.1:8080").rstrip("/")
CASE_DIR = os.environ.get(
    "AXVISOR_HTTP_CASE_DIR", os.path.dirname(os.path.abspath(__file__))
)
CONNECT_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "5"))
# Deadline for VM state transitions (guest entry, delete): well below the case
# `timeout` so a stuck transition fails on the probe, not on QEMU.
POLL_DEADLINE = 120.0
POLL_INTERVAL = 1.0
# The event watcher samples the registry every 250ms, so a frame takes a moment.
EVENT_TIMEOUT = 60.0
# The guest fixture boots a Linux kernel and a BusyBox initramfs; its shell
# prompt is the precondition for the input check.
GUEST_BOOT_DEADLINE = 90.0

# The guest shell writes this when it is ready for input; the status query on the
# same line is what a terminal has to answer for line editing to proceed.
GUEST_PROMPT = b"~ #"
TERMINAL_STATUS_QUERY = b"\x1b[6n"
TERMINAL_STATUS_REPLY = b"\x1b[1;1R"
# Typed command whose result cannot be found in its own text: echoing the line
# back contains `111*111`, never `12321`, so seeing `12321` proves the guest shell
# evaluated it — not that the host echoed keystrokes somewhere.
GUEST_INPUT_COMMAND = b"echo $((111*111))\r"
GUEST_INPUT_RESULT = b"12321"
# The lane tells a browser why a keystroke went nowhere while the guest is not
# running. Silent rejection is what makes a working terminal look broken.
GUEST_INPUT_REJECTED = b"is not running; input was dropped"
# Second guest command and its result, used by the lane-isolation check. The
# value cannot appear in the command text, so seeing it proves the guest ran it.
GUEST_ISOLATION_COMMAND = b"echo $((222*222))\r"
GUEST_ISOLATION_RESULT = b"49284"

# The default guest (`web-ui/vm-linux-alpine.toml`), kept `Ready` by `no-auto-start`.
DEFAULT_VM_ID = 1
# The one path the bundle has to carry: `GET /api/manifest` is the bootstrap, and
# it cannot come from the manifest itself. Every other path the dashboard calls
# is read out of that response, so this is the whole list by design — a bundle
# that does not carry it cannot learn anything else.
BUNDLE_ENDPOINTS = (b"/api/manifest",)

# The operations each panel declares, as the dashboard relies on them: the shell
# opens a panel per kind, the VM panel drives the whole lifecycle and the pool,
# the console panel lists lanes and streams one, and the management panel streams
# its own lane. Pinning the names here is what keeps the declaration and the UI
# from drifting apart in either direction: a link the UI needs but nobody
# declares fails the case, and a declared link that stops being served fails it
# too (`check_manifest_links`).
MANIFEST_LINKS = {
    "vms": [
        "browse",
        "create",
        "delete",
        "detail",
        "events",
        "list",
        "pause",
        "pool",
        "pool_save",
        "resume",
        "schema",
        "start",
        "stop",
    ],
    "console": ["list", "stream"],
    "shell": ["stream"],
    # One upload split into the steps an interrupted transfer needs: the
    # dashboard drags a file through exactly these operations.
    "files": ["browse", "drop", "list", "mkdir", "open", "place", "resume", "send"],
}
MANIFEST_ROOTS = {
    "vms": "/api/vms",
    "files": "/api/files",
    "console": "/api/consoles",
    "shell": "/ws",
}
IMMUTABLE_CACHE = "public, max-age=31536000, immutable"

# How the emitted bundle names the assets it needs. vite lists the chunks in the
# preload map as bare `assets/<name>` entries, and imports the panel chunks by
# relative name (`import("./chunk.js")`), so both forms have to be followed —
# following only the shell would miss every lazily loaded panel.
ASSET_REFERENCE_PATTERNS = (
    re.compile(rb"/?assets/([A-Za-z0-9._-]+)"),
    re.compile(rb"""import\(\s*["']\./([A-Za-z0-9._-]+)["']\s*\)"""),
)


def asset_references(payload):
    """Every asset path `payload` refers to, as absolute `/assets/...` paths."""
    found = set()
    for pattern in ASSET_REFERENCE_PATTERNS:
        found.update(b"/assets/" + name for name in pattern.findall(payload))
    return found


def request(method, path, body=None):
    """One HTTP request; returns (status, parsed JSON or None).

    The control plane has no authentication, so no request carries an
    Authorization header. A non-2xx response is not an error here — the caller
    asserts the status. A transport error (the single-threaded server is busy
    building a VM) raises RuntimeError for the caller to retry or verify.
    """
    headers = {}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = body.encode("utf-8")
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            status = resp.status
            raw = resp.read()
    except urllib.error.HTTPError as err:
        status = err.code
        raw = err.read()
    except urllib.error.URLError as err:
        raise RuntimeError("request %s %s failed: %s" % (method, path, err.reason))
    except OSError as err:
        # `resp.read()` raises a bare socket timeout that the URLError handler
        # does not wrap (QEMU hostfwd accepts before the in-guest server binds).
        raise RuntimeError("request %s %s failed: %s" % (method, path, err))
    if not raw:
        return status, None
    return status, json.loads(raw.decode("utf-8"))


def get(path, label):
    """One GET, retried until the deadline (the server blocks while creating)."""
    start = time.monotonic()
    while True:
        try:
            return request("GET", path)
        except RuntimeError as error:
            if time.monotonic() - start > POLL_DEADLINE:
                raise AssertionError("%s: %s (and it never recovered)" % (label, error))
            time.sleep(POLL_INTERVAL)


def check(label, actual, expected):
    if actual != expected:
        raise AssertionError("%s was %r, expected %r" % (label, actual, expected))
    print("  web-ui probe: %s -> %r" % (label, actual))


def expect_status(label, actual, expected):
    if actual != expected:
        raise AssertionError("%s returned %s, expected %s" % (label, actual, expected))
    print("  web-ui probe: %s -> %s" % (label, actual))


def poll_ready():
    """Poll `GET /api/vms` until it answers 200 or the connect deadline passes."""
    start = time.monotonic()
    while True:
        if time.monotonic() - start > CONNECT_TIMEOUT:
            raise AssertionError(
                "guest management HTTP server never became reachable within %.0fs"
                % CONNECT_TIMEOUT
            )
        try:
            status, _ = request("GET", "/api/vms")
            if status == 200:
                print("  web-ui probe: guest management HTTP server reachable")
                return
        except RuntimeError:
            pass
        time.sleep(POLL_INTERVAL)


def raw_get(path):
    """GET without JSON parsing; returns (status, headers, body).

    The dashboard serves HTML and JavaScript and its response headers are part
    of the contract, so this bypasses the JSON helper. Header names are
    lowercased because HTTP header names are case-insensitive.
    """
    req = urllib.request.Request(BASE + path, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            return (
                resp.status,
                {name.lower(): value for name, value in resp.headers.items()},
                resp.read(),
            )
    except urllib.error.HTTPError as err:
        return err.code, {k.lower(): v for k, v in err.headers.items()}, err.read()
    except (urllib.error.URLError, OSError) as err:
        raise RuntimeError("GET %s failed: %s" % (path, err))


class WebSocket:
    """Minimal RFC 6455 client for the in-guest socket checks."""

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
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.stream.sendall(header + mask + masked)

    def close(self):
        self.stream.close()


def open_websocket(path):
    parsed = urllib.parse.urlsplit(BASE)
    host = parsed.hostname
    port = parsed.port or 80
    authority = "%s:%d" % (host, port)
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    stream = socket.create_connection((host, port), timeout=REQUEST_TIMEOUT)
    stream.settimeout(REQUEST_TIMEOUT)
    handshake = (
        "GET %s HTTP/1.1\r\n"
        "Host: %s\r\n"
        "Origin: %s\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Key: %s\r\n\r\n"
    ) % (path, authority, BASE, key)
    stream.sendall(handshake.encode("ascii"))
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = stream.recv(4096)
        if not chunk:
            raise AssertionError("HTTP server closed during WebSocket upgrade")
        response += chunk
    response_head, buffered = response.split(b"\r\n\r\n", 1)
    return WebSocket(stream, buffered), response_head


def expect_upgrade(path, expected):
    websocket, response = open_websocket(path)
    if not response.startswith(("HTTP/1.1 %d" % expected).encode("ascii")):
        websocket.close()
        raise AssertionError("%s upgrade returned %r" % (path, response))
    return websocket


def receive_text(websocket, deadline):
    """One Text frame (the event channel sends JSON texts and nothing else)."""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AssertionError("no event frame arrived before the deadline")
        ready, _, _ = select.select([websocket.stream], [], [], remaining)
        if not ready:
            raise AssertionError("no event frame arrived before the deadline")
        opcode, payload = websocket.recv_frame()
        if opcode == 1:
            return json.loads(payload.decode("utf-8"))
        if opcode == 8:
            raise AssertionError("the event socket closed before the frame arrived")


def expect_event(websocket, expected_type, vm_id, deadline, label):
    """Read frames until the expected one arrives, ignoring the others."""
    while True:
        frame = receive_text(websocket, deadline)
        if frame.get("type") == expected_type and frame.get("id") == vm_id:
            print("  web-ui probe: %s -> %r" % (label, frame))
            return frame


def receive_until_prompt(websocket, timeout=REQUEST_TIMEOUT):
    """Wait for the management shell prompt, whatever cwd it reports.

    The shell prints `axvisor:<cwd>$ `, and the cwd is empty when the build has
    no root filesystem, so a fixed marker would only match one of the two builds.
    """
    output = b""
    deadline = time.monotonic() + timeout
    while not re.search(rb"axvisor:[^\r\n]*\$ ", output):
        if time.monotonic() >= deadline:
            raise AssertionError("management shell prompt never arrived: %r" % (output[-200:],))
        opcode, payload = websocket.recv_frame()
        if opcode in (1, 2):
            output += payload
            continue
        if opcode == 8:
            raise AssertionError("WebSocket closed before the shell prompt")
    return output


def receive_until(websocket, marker, timeout=REQUEST_TIMEOUT):
    output = b""
    deadline = time.monotonic() + timeout
    while marker not in output:
        if time.monotonic() >= deadline:
            raise AssertionError("WebSocket output did not contain %r" % marker)
        opcode, payload = websocket.recv_frame()
        if opcode in (1, 2):
            output += payload
            continue
        if opcode == 8:
            raise AssertionError("WebSocket closed before output marker %r" % marker)
    return output


def receive_some(websocket, deadline):
    """Next data frame, answering the terminal status query a shell may send.

    BusyBox's line editor asks the *terminal* for the cursor position and waits
    for the answer, so a console that never replies can look exactly like a
    console whose input is broken.
    """
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AssertionError("no console output arrived before the deadline")
        ready, _, _ = select.select([websocket.stream], [], [], remaining)
        if not ready:
            continue
        opcode, payload = websocket.recv_frame()
        if opcode == 8:
            raise AssertionError("the console closed before the output arrived")
        if opcode not in (1, 2):
            continue
        if TERMINAL_STATUS_QUERY in payload:
            websocket.send_binary(TERMINAL_STATUS_REPLY)
        return payload


def drain_available(websocket, seconds):
    """Everything that arrives within `seconds`; an idle lane returns b""."""
    out = b""
    deadline = time.monotonic() + seconds
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return out
        ready, _, _ = select.select([websocket.stream], [], [], remaining)
        if not ready:
            return out
        opcode, payload = websocket.recv_frame()
        if opcode in (1, 2):
            out += payload


def receive_output_until(websocket, marker, deadline, label):
    """Accumulate console output until it contains `marker`, or fail."""
    output = b""
    while marker not in output:
        if time.monotonic() >= deadline:
            raise AssertionError(
                "%s: %r never arrived; last output was %r" % (label, marker, output[-400:])
            )
        output += receive_some(websocket, deadline)
    print("  web-ui probe: %s -> %r" % (label, marker.decode("utf-8", "replace")))
    return output


def console_routes(label):
    """Fetch `/api/consoles` as {route: display name}."""
    status, body = get("/api/consoles", label)
    expect_status(label, status, 200)
    if not isinstance(body, list):
        raise AssertionError("%s did not return a JSON array: %r" % (label, body))
    routes = {}
    for console in body:
        if not isinstance(console, dict):
            raise AssertionError("%s listed a non-object console: %r" % (label, console))
        routes[console.get("route")] = console.get("name")
    print("  web-ui probe: %s -> %r" % (label, sorted(routes)))
    return routes


def lane_attached(label, route):
    """`attached` for one lane, checking the field on every reported console.

    The dashboard needs this fact to explain a refused terminal: a browser
    WebSocket hides the server's 409 (lane already taken) behind an anonymous
    1006 close, so a missing or non-boolean field turns "close the other page"
    back into "typing does nothing".
    """
    status, body = get("/api/consoles", label)
    expect_status(label, status, 200)
    if not isinstance(body, list):
        raise AssertionError("%s did not return a JSON array: %r" % (label, body))
    table = {}
    for console in body:
        if not isinstance(console, dict):
            raise AssertionError("%s listed a non-object console: %r" % (label, console))
        attached = console.get("attached")
        if not isinstance(attached, bool):
            raise AssertionError(
                "%s reported attached=%r for lane %r"
                % (label, attached, console.get("route"))
            )
        table[console.get("route")] = attached
    if route not in table:
        raise AssertionError("%s has no lane %r: %r" % (label, route, sorted(table)))
    print("  web-ui probe: %s -> %s attached=%r" % (label, route, table[route]))
    return table[route]


def poll_lane_attached(label, route, expected, deadline):
    """Wait until one lane reports `expected`; a released lane has to come back."""
    while True:
        if lane_attached(label, route) == expected:
            return
        if time.monotonic() >= deadline:
            raise AssertionError("%s never became attached=%r" % (label, expected))
        time.sleep(POLL_INTERVAL)


def poll_consoles_for(label, expected_routes, deadline):
    """Wait until the lane table holds exactly `expected_routes`.

    Only the route set is compared: the display name is the guest's configured
    name, which a case fixture may change without making this probe wrong.
    """
    while True:
        routes = console_routes(label)
        if sorted(routes) == sorted(expected_routes):
            for route, name in routes.items():
                if not name:
                    raise AssertionError("%s reported no name for %s" % (label, route))
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                "%s never reached %r (last saw %r)" % (label, sorted(expected_routes), sorted(routes))
            )
        time.sleep(POLL_INTERVAL)


def vm_detail(vm_id, label):
    status, body = get("/api/vms/%d" % vm_id, label)
    expect_status(label, status, 200)
    return body


def check_dashboard():
    """The dashboard shell, its hashed assets, the headers, and the bundle wiring."""
    status, headers, page = raw_get("/")
    expect_status("GET /", status, 200)
    for marker in (b'<div id="root">', "<title>AxVisor 管理台</title>".encode("utf-8")):
        if marker not in page:
            raise AssertionError("dashboard shell is missing %r" % marker)
    if "text/html" not in headers.get("content-type", ""):
        raise AssertionError("GET / served %r" % headers.get("content-type"))
    if headers.get("cache-control") != "no-cache":
        raise AssertionError("GET / Cache-Control=%r" % headers.get("cache-control"))
    if headers.get("x-content-type-options") != "nosniff":
        raise AssertionError("GET / is missing nosniff")
    csp = headers.get("content-security-policy", "")
    if "default-src 'self'" not in csp or "frame-ancestors 'none'" not in csp:
        raise AssertionError("GET / Content-Security-Policy=%r" % csp)

    # Every asset the shell references must resolve, and so must every chunk
    # those assets lazily import: the panels are separate chunks, so the entry
    # only carries their URLs. A missing chunk would break the dashboard at the
    # moment a user opens its panel, which is exactly what this crawl prevents.
    fetched = {}
    pending = asset_references(page)
    if not pending:
        raise AssertionError("dashboard shell references no assets")
    while pending:
        path = pending.pop().decode("utf-8")
        if path in fetched:
            continue
        status, headers, payload = raw_get(path)
        expect_status("GET %s" % path, status, 200)
        if not payload:
            raise AssertionError("%s served an empty body" % path)
        if headers.get("cache-control") != IMMUTABLE_CACHE:
            raise AssertionError("%s Cache-Control=%r" % (path, headers.get("cache-control")))
        if headers.get("x-content-type-options") != "nosniff":
            raise AssertionError("%s is missing nosniff" % path)
        content_type = headers.get("content-type", "")
        if path.endswith(".js"):
            if "javascript" not in content_type:
                raise AssertionError("%s served as %r" % (path, content_type))
        elif path.endswith(".css") and "css" not in content_type:
            raise AssertionError("%s served as %r" % (path, content_type))
        fetched[path] = headers
        for reference in asset_references(payload):
            if reference not in fetched:
                pending.add(reference)
    scripts = sorted(path for path in fetched if path.endswith(".js"))
    stylesheets = sorted(path for path in fetched if path.endswith(".css"))
    # The panels are lazily imported chunks, so the graph always holds more than
    # the shell script, and it always holds the stylesheet that comes with the
    # terminal chunk. A graph without them means the crawl stopped following the
    # bundle's references: the check would keep passing while covering almost
    # nothing, which is worse than failing.
    if len(scripts) < 2:
        raise AssertionError(
            "the shell is the only script in the asset graph: %r" % (scripts,)
        )
    if not stylesheets:
        raise AssertionError("the asset graph carries no stylesheet")
    print("  web-ui probe: asset graph resolved -> %d files" % len(fetched))
    for path in sorted(fetched):
        print("  web-ui probe:   asset %s" % path)

    # No SPA catch-all: an unknown path keeps the router's 404, and the asset
    # table is exact rather than a directory listing.
    status, _, _ = raw_get("/no-such-dashboard-page")
    expect_status("GET /no-such-dashboard-page", status, 404)
    status, _, _ = raw_get("/assets/no-such-asset.js")
    expect_status("GET /assets/no-such-asset.js", status, 404)

    # The served bundle must be the one wired to this contract. What the UI is
    # wired to is now the manifest, so the bundle only has to carry the bootstrap
    # path; the operations themselves are checked against the declaration above,
    # which is where they are actually read from. The path has to appear as a
    # quoted literal: a substring match would also accept a longer path that
    # happens to start with it, and the point is that this bundle carries it.
    union = b"".join(raw_get(path)[2] for path in scripts)
    missing = [
        path
        for path in BUNDLE_ENDPOINTS
        if b'"%s"' % path not in union and b"'%s'" % path not in union
    ]
    if missing:
        raise AssertionError("the served bundle is missing %r" % (missing,))
    print("  web-ui probe: bundle carries the bootstrap path")


def request_status(method, path):
    """One request, returning only the status.

    The link check asks whether a route answers the method it declares, so the
    body is not parsed: an empty `POST` body is refused by the JSON extractor
    with a plain-text 4xx, which is still a registered route answering. Transport
    errors are retried while the guest server is busy.
    """
    deadline = time.monotonic() + POLL_DEADLINE
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
    each declared link is what makes the declaration falsifiable. `{id}` is
    filled with a VM id this probe never creates and `{endpoint}` with the
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
            path = link["href"].replace("{id}", "4242").replace("{endpoint}", "axvisor")
            status = request_status(link["method"], path)
            if status == 405:
                raise AssertionError(
                    "%s link %s %s is declared but not served"
                    % (panel.get("kind"), link["method"], path)
                )
            calls += 1
    print("  web ui probe: manifest links all served (%d)" % calls)


def check_manifest():
    """The capability declaration has to describe this build: all three panels."""
    status, body = get("/api/manifest", "GET /api/manifest")
    expect_status("GET /api/manifest", status, 200)
    if not isinstance(body, dict):
        raise AssertionError("GET /api/manifest did not return an object: %r" % (body,))
    check("manifest proto", body.get("proto"), 1)
    panels = body.get("panels")
    if not isinstance(panels, list):
        raise AssertionError("manifest panels was not a list: %r" % (body,))
    declared = {panel.get("kind"): panel.get("verbs") for panel in panels}
    check("manifest panel kinds", sorted(declared), ["console", "files", "shell", "vms"])
    check("files panel verbs", declared["files"], ["read", "write"])
    check("vms panel verbs", declared["vms"], ["read", "write"])
    check("console panel verbs", declared["console"], ["read", "write", "stream"])
    check("shell panel verbs", declared["shell"], ["read", "write", "stream"])
    for panel in panels:
        if not isinstance(panel.get("root"), str) or not panel["root"]:
            raise AssertionError("manifest panel had no root: %r" % (panel,))
        check(
            "%s panel root" % panel["kind"],
            panel["root"],
            MANIFEST_ROOTS[panel["kind"]],
        )
        check(
            "%s panel links" % panel["kind"],
            sorted(link.get("name") for link in panel.get("links", [])),
            MANIFEST_LINKS[panel["kind"]],
        )
    check_manifest_links(panels)


def check_terminals():
    """The lanes the dashboard renders, plus the two sockets behind them."""
    routes = console_routes("GET /api/consoles (default VM ready)")
    if sorted(routes) != ["axvisor", "vm-1"]:
        raise AssertionError("a Ready guest has no lane? consoles were %r" % (routes,))
    # The guest lane is named after the configured VM, not after its id: the
    # dashboard shows this string, so it has to come from the registry.
    check("guest lane name", routes["vm-1"], "linux-web-ui")
    # Nothing has attached yet, so no lane may claim to be taken.
    check("idle guest lane", lane_attached("GET /api/consoles (idle lanes)", "vm-1"), False)

    shell = expect_upgrade("/ws/axvisor", 101)
    receive_until(shell, b"Welcome to AxVisor Browser Shell!")
    # A held lane is the one fact a refused browser cannot see for itself.
    check("held management lane", lane_attached("GET /api/consoles (shell held)", "axvisor"), True)
    # A real command, not just a greeting: the web shell drives the same
    # interpreter as the board console, and its table must show the same VM the
    # REST list reports.
    shell.send_binary(b"vm list\r")
    output = receive_until_prompt(shell)
    for marker in (b"VM ID", b"linux-web-ui"):
        if marker not in output:
            raise AssertionError("`vm list` output is missing %r: %r" % (marker, output))
    print("  web-ui probe: /ws/axvisor -> interactive management shell")

    guest = expect_upgrade("/ws/vm-1", 101)
    receive_until(guest, b"browser console attached to VM 1")
    print("  web-ui probe: /ws/vm-1 -> guest console greeting")

    # The lanes are exclusive: the dashboard also opens one socket per lane, so a
    # second subscriber has to be refused rather than silently sharing the input.
    duplicate = expect_upgrade("/ws/axvisor", 409)
    duplicate.close()
    print("  web-ui probe: duplicate /ws/axvisor -> 409")

    shell.close()
    guest.close()
    # The management lane must be reusable once its subscriber is gone, otherwise
    # the dashboard could only ever be opened once per boot.
    deadline = time.monotonic() + POLL_DEADLINE
    while True:
        reopened, response = open_websocket("/ws/axvisor")
        if response.startswith(b"HTTP/1.1 101"):
            reopened.close()
            break
        reopened.close()
        if not response.startswith(b"HTTP/1.1 409") or time.monotonic() >= deadline:
            raise AssertionError("released /ws/axvisor did not reopen: %r" % response)
        time.sleep(POLL_INTERVAL)
    print("  web-ui probe: released management lane -> reusable")


def check_lifecycle(events):
    """Drive the registry the dashboard renders, watching the event channel."""
    # Attach before the guest starts: a console lane is live as soon as the VM
    # exists, and the input check below needs the lane to be watching while the
    # fixture guest boots and reaches its shell.
    guest = expect_upgrade("/ws/vm-1", 101)
    receive_until(guest, b"browser console attached to VM 1")
    check(
        "guest lane held",
        lane_attached("GET /api/consoles (guest lane held)", "vm-1"),
        True,
    )

    # The lane is open but the guest is still `Ready`: bytes typed now have no
    # running guest to read them, and the browser has to be told that instead of
    # seeing its keystrokes vanish.
    guest.send_binary(b"x")
    receive_output_until(
        guest,
        GUEST_INPUT_REJECTED,
        time.monotonic() + REQUEST_TIMEOUT * 3,
        "input rejected while the guest is stopped",
    )

    status, body = request("POST", "/api/vms/%d/start" % DEFAULT_VM_ID)
    expect_status("POST /api/vms/1/start", status, 200)
    if body.get("ok") is not True:
        raise AssertionError("start returned %r" % (body,))

    # A start is accepted before the vCPU runs: the counter is the proof that the
    # guest actually entered, and the event frame is the proof that the browser
    # list learns about it without polling.
    frame = expect_event(
        events, "status", DEFAULT_VM_ID, time.monotonic() + EVENT_TIMEOUT, "start event frame"
    )
    if frame.get("status") not in ("running", "ready"):
        raise AssertionError("start event reported %r" % (frame,))

    deadline = time.monotonic() + POLL_DEADLINE
    while True:
        detail = vm_detail(DEFAULT_VM_ID, "GET /api/vms/1 (waiting for guest entry)")
        if (detail.get("guest_entry_count") or 0) >= 1 and detail.get("status") == "running":
            print(
                "  web-ui probe: VM[1] running, guest_entry_count=%d"
                % detail["guest_entry_count"]
            )
            break
        if time.monotonic() >= deadline:
            raise AssertionError("VM[1] never entered the guest: %r" % (detail,))
        time.sleep(POLL_INTERVAL)

    # vCPU affinity is a *bitmask* (`Option<usize>` on the AxVisor side) and the
    # fixture pins vCPU 0 to Core 1 (`phys_cpu_ids = [1]`), so the detail has to
    # report 2 — not a CPU id, not a list. The dashboard decodes this mask; a
    # `number[]` reading is what blanked the page.
    vcpus = detail.get("vcpu_states")
    if not isinstance(vcpus, list) or not vcpus:
        raise AssertionError("GET /api/vms/1 reported no vcpu_states: %r" % (detail,))
    masks = [vcpu.get("phys_cpu_set") for vcpu in vcpus]
    for mask in masks:
        if mask is not None and not isinstance(mask, int):
            raise AssertionError("phys_cpu_set was %r, expected a bitmask or null" % (mask,))
    check("vcpu affinity mask", masks[0], 2)

    # Input path: bytes sent to the guest lane have to reach the guest's UART and
    # be executed there. A start that only flips the VMM status leaves the
    # console mux's own "running" set empty, which silently drops every byte a
    # browser types — the queue accepts it and nobody answers.
    receive_output_until(
        guest, GUEST_PROMPT, time.monotonic() + GUEST_BOOT_DEADLINE, "guest shell prompt"
    )
    guest.send_binary(GUEST_INPUT_COMMAND)
    receive_output_until(
        guest, GUEST_INPUT_RESULT, time.monotonic() + GUEST_BOOT_DEADLINE, "guest ran typed command"
    )

    # Wiring: the management lane and a guest lane are two independent byte
    # streams -- the dashboard shows them as separate panes, and a browser that
    # reads one must never see the other's traffic. Both are attached at once
    # here, which is the only way that claim can fail if routing is wrong.
    shell = expect_upgrade("/ws/axvisor", 101)
    receive_until_prompt(shell)
    guest.send_binary(GUEST_ISOLATION_COMMAND)
    receive_output_until(
        guest,
        GUEST_ISOLATION_RESULT,
        time.monotonic() + GUEST_BOOT_DEADLINE,
        "guest ran a second command with the management lane attached",
    )
    stray = drain_available(shell, 2.0)
    if GUEST_ISOLATION_RESULT in stray:
        raise AssertionError("guest output leaked into the management lane: %r" % (stray[-200:],))
    shell.send_binary(b"vm list\r")
    receive_output_until(
        shell, b"VM ID", time.monotonic() + REQUEST_TIMEOUT * 3, "management lane lists VMs"
    )
    stray = drain_available(guest, 2.0)
    if b"VM ID" in stray or b"linux-web-ui" in stray:
        raise AssertionError("management output leaked into the guest lane: %r" % (stray[-200:],))
    shell.close()
    print("  web-ui probe: management and guest lanes carry separate streams")

    # `stop` settles the guest but keeps it registered, so its console lane stays
    # in place and input is still routed -- and still rejected. This is also the
    # second proof of the rejection notice: it has to be re-armed by the
    # successful input above, or the browser would hear nothing this time.
    status, body = request("POST", "/api/vms/%d/stop" % DEFAULT_VM_ID)
    expect_status("POST /api/vms/1/stop", status, 200)
    deadline = time.monotonic() + POLL_DEADLINE
    while True:
        detail = vm_detail(DEFAULT_VM_ID, "GET /api/vms/1 (waiting for stop)")
        if detail.get("status") == "stopped":
            print("  web-ui probe: VM[1] stopped, console lane kept")
            break
        if time.monotonic() >= deadline:
            raise AssertionError("VM[1] never stopped: %r" % (detail,))
        time.sleep(POLL_INTERVAL)
    guest.send_binary(b"x")
    receive_output_until(
        guest,
        GUEST_INPUT_REJECTED,
        time.monotonic() + REQUEST_TIMEOUT * 3,
        "input rejected again after the guest stopped",
    )

    guest.close()
    poll_lane_attached(
        "GET /api/consoles (guest lane released)",
        "vm-1",
        False,
        time.monotonic() + POLL_DEADLINE,
    )

    status, _ = request("DELETE", "/api/vms/%d" % DEFAULT_VM_ID)
    expect_status("DELETE /api/vms/1", status, 204)
    expect_event(
        events, "removed", DEFAULT_VM_ID, time.monotonic() + EVENT_TIMEOUT, "close event frame"
    )
    poll_consoles_for(
        "GET /api/consoles (guest closed)", ["axvisor"], time.monotonic() + POLL_DEADLINE
    )
    print("  web-ui probe: closing the guest released its console lane")


def check_recreate(events):
    """Recreate the default guest from the fixture, the way the panel does."""
    with open(os.path.join(CASE_DIR, "vm-linux-alpine.toml"), "r", encoding="utf-8") as handle:
        vm_config = handle.read()
    status, body = request(
        "POST", "/api/vms/create", json.dumps({"toml": vm_config})
    )
    expect_status("POST /api/vms/create", status, 200)
    check("created id", body.get("id"), DEFAULT_VM_ID)
    expect_event(
        events, "created", DEFAULT_VM_ID, time.monotonic() + EVENT_TIMEOUT, "create event frame"
    )
    poll_consoles_for(
        "GET /api/consoles (guest recreated)",
        ["axvisor", "vm-1"],
        time.monotonic() + POLL_DEADLINE,
    )
    status, _ = request("DELETE", "/api/vms/%d" % DEFAULT_VM_ID)
    expect_status("DELETE /api/vms/1 (cleanup)", status, 204)


def main():
    poll_ready()
    check_dashboard()
    check_manifest()

    events = expect_upgrade("/ws/events", 101)
    snapshot = receive_text(events, time.monotonic() + EVENT_TIMEOUT)
    if snapshot.get("type") != "snapshot":
        raise AssertionError("first event frame was %r, expected a snapshot" % (snapshot,))
    listed = {vm.get("id"): vm.get("status") for vm in snapshot.get("vms", [])}
    check("event snapshot", listed, {DEFAULT_VM_ID: "ready"})

    check_terminals()
    check_lifecycle(events)
    check_recreate(events)
    events.close()
    print("  web-ui probe: PASS")


if __name__ == "__main__":
    main()
