#!/usr/bin/env python3
"""Host-side probe asset for the diskless Axvisor management dashboard.

Case asset for the `qemu-web-ui/web-ui-diskless` test case
(`test-suit/axvisor/normal/qemu-web-ui/web-ui-diskless/`). It owns the test
content and can evolve independently of the axbuild runner; the generic probe
runner (`scripts/axbuild/src/axvisor/test/http_probe.rs`) executes it after the
QEMU hostfwd port is reachable and treats the exit code as the verdict.

This case runs the same `web-ui` feature build as the NVMe-root case but with
no host disk at all: QEMU attaches no block device and the boot arguments
request no `root=`, so Axvisor keeps the initramfs memory root. Everything the
probe touches therefore has to come out of the host initramfs bundle:

    GET    /                      -> 200   (dashboard shell from the memory root;
                                            CSP + no-cache + nosniff)
    GET    /assets/{hashed}       -> 200   (the full asset graph the shell
                                            references, all immutable)
    GET    /no-such-page          -> 404   (no SPA catch-all)
    GET    /assets/no-such.js     -> 404   (served tree is exact)
    GET    /api/manifest          -> 200   (same panel set as the disk case)
    GET    /ws/axvisor            -> 101   (management lane greeting)

The guest lifecycle, the event channel and the terminal exclusivity rules are
covered by the NVMe-root case; this probe deliberately stops at what the
memory root has to provide.
"""

import base64
import json
import os
import re
import socket
import time
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("AXVISOR_HTTP_BASE", "http://127.0.0.1:8080").rstrip("/")
CONNECT_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "5"))
POLL_INTERVAL = 1.0

IMMUTABLE_CACHE = "public, max-age=31536000, immutable"

# Same expectations as the NVMe-root case: the panel set comes from the
# compiled capability table, not from the guest registry, so a diskless boot
# with the same features declares the same panels.
MANIFEST_PANEL_KINDS = ["console", "files", "host", "shell", "vms"]

# The one path the bundle has to carry: `GET /api/manifest` is the bootstrap
# endpoint and cannot come from the manifest itself.
BUNDLE_ENDPOINTS = (b"/api/manifest",)

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
    """One HTTP request; returns (status, parsed JSON or None)."""
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
    except (urllib.error.URLError, OSError) as err:
        raise RuntimeError("request %s %s failed: %s" % (method, path, err))
    if not raw:
        return status, None
    return status, json.loads(raw.decode("utf-8"))


def raw_get(path):
    """GET without JSON parsing; returns (status, headers, body)."""
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


def expect_status(label, actual, expected):
    if actual != expected:
        raise AssertionError("%s returned %s, expected %s" % (label, actual, expected))
    print("  web-ui diskless probe: %s -> %s" % (label, actual))


def poll_ready():
    """Poll `GET /api/vms` until it answers 200 or the connect deadline passes."""
    start = time.monotonic()
    while True:
        if time.monotonic() - start > CONNECT_TIMEOUT:
            raise AssertionError(
                "management HTTP server never became reachable within %.0fs"
                % CONNECT_TIMEOUT
            )
        try:
            status, body = request("GET", "/api/vms")
            if status == 200:
                if body != []:
                    raise AssertionError("diskless boot registered guests: %r" % (body,))
                print("  web-ui diskless probe: management HTTP server reachable")
                return
        except RuntimeError:
            pass
        time.sleep(POLL_INTERVAL)


def check_dashboard():
    """The dashboard shell, its whole hashed asset graph, and the headers."""
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

    # Crawl the full asset graph: the shell references lazily imported panel
    # chunks, so following only the shell would leave most of the dashboard
    # unverified. Every hop must resolve from the memory root.
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
        if path.endswith(".js") and "javascript" not in content_type:
            raise AssertionError("%s served as %r" % (path, content_type))
        if path.endswith(".css") and "css" not in content_type:
            raise AssertionError("%s served as %r" % (path, content_type))
        fetched[path] = payload
        for reference in asset_references(payload):
            if reference not in fetched:
                pending.add(reference)
    scripts = sorted(path for path in fetched if path.endswith(".js"))
    stylesheets = sorted(path for path in fetched if path.endswith(".css"))
    if len(scripts) < 2:
        raise AssertionError("the shell is the only script in the asset graph: %r" % (scripts,))
    if not stylesheets:
        raise AssertionError("the asset graph carries no stylesheet")
    print(
        "  web-ui diskless probe: asset graph resolved from the memory root -> %d files"
        % len(fetched)
    )

    # No SPA catch-all and no directory listing: unknown names keep the 404.
    status, _, _ = raw_get("/no-such-dashboard-page")
    expect_status("GET /no-such-dashboard-page", status, 404)
    status, _, _ = raw_get("/assets/no-such-asset.js")
    expect_status("GET /assets/no-such-asset.js", status, 404)

    # The served bundle must be the one wired to this contract: the bootstrap
    # path has to appear as a quoted literal in the entry scripts.
    union = b"".join(fetched[path] for path in scripts)
    missing = [
        path
        for path in BUNDLE_ENDPOINTS
        if b'"%s"' % path not in union and b"'%s'" % path not in union
    ]
    if missing:
        raise AssertionError("the served bundle is missing %r" % (missing,))
    print("  web-ui diskless probe: bundle carries the bootstrap path")


def check_manifest():
    """The capability declaration matches the same-feature disk case."""
    status, body = request("GET", "/api/manifest")
    expect_status("GET /api/manifest", status, 200)
    if not isinstance(body, dict):
        raise AssertionError("GET /api/manifest did not return an object: %r" % (body,))
    if body.get("proto") != 1:
        raise AssertionError("manifest proto was %r" % (body.get("proto"),))
    panels = body.get("panels")
    if not isinstance(panels, list):
        raise AssertionError("manifest panels was not a list: %r" % (panels,))
    declared = sorted(panel.get("kind") for panel in panels)
    if declared != MANIFEST_PANEL_KINDS:
        raise AssertionError(
            "diskless manifest panels %r, expected %r" % (declared, MANIFEST_PANEL_KINDS)
        )
    print("  web-ui diskless probe: manifest panels -> %r" % (declared,))


def check_management_lane():
    """The management shell lane lives on the memory root too."""
    parsed = urllib.parse.urlsplit(BASE)
    authority = "%s:%d" % (parsed.hostname, parsed.port or 80)
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    stream = socket.create_connection((parsed.hostname, parsed.port), timeout=REQUEST_TIMEOUT)
    stream.settimeout(REQUEST_TIMEOUT)
    handshake = (
        "GET /ws/axvisor HTTP/1.1\r\n"
        "Host: %s\r\n"
        "Origin: %s\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Key: %s\r\n\r\n"
    ) % (authority, BASE, key)
    stream.sendall(handshake.encode("ascii"))
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = stream.recv(4096)
        if not chunk:
            raise AssertionError("HTTP server closed during WebSocket upgrade")
        response += chunk
    if not response.startswith(b"HTTP/1.1 101"):
        raise AssertionError("/ws/axvisor upgrade returned %r" % (response[:64],))
    stream.close()
    print("  web-ui diskless probe: /ws/axvisor -> 101")


def main():
    poll_ready()
    check_dashboard()
    check_manifest()
    check_management_lane()
    print("  web-ui diskless probe: PASS")


if __name__ == "__main__":
    main()
