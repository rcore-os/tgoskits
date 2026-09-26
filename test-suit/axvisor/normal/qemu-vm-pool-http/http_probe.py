#!/usr/bin/env python3
"""Host-side probe asset for the AxVisor control plane over the VM pool.

Case asset for the `qemu-vm-pool-http` test case
(`test-suit/axvisor/normal/qemu-vm-pool-http/`). It owns the test content — the
concrete requests and assertions — and can evolve independently of the axbuild
runner.

The generic axbuild probe runner
(`scripts/axbuild/src/axvisor/test/http_probe.rs`) executes this script after
the QEMU hostfwd port is reachable, then treats the exit code as the verdict:
0 = all assertions passed, nonzero = a step failed. The script dials the axum
management API running *inside* the AxVisor guest through QEMU user-mode
networking hostfwd. Nothing in the hypervisor knows a test is running.

Environment (set by the generic runner):

    AXVISOR_HTTP_BASE            http://127.0.0.1:<host_port> (forwarded)
    AXVISOR_HTTP_CASE_DIR        case directory holding the `sh/` fixtures
    AXVISOR_HTTP_CONNECT_TIMEOUT seconds for the initial reachability wait
    AXVISOR_HTTP_REQUEST_TIMEOUT seconds per HTTP request

The case boots with no default guest and a pool directory holding three complete
guest configs plus one unusable file, all injected into the guest filesystem by
the `sh/` asset pipeline. The probe drives the whole browser control plane in one
boot:

    GET    /api/manifest           -> 200 vms + console + shell panels
    GET    /api/consoles           -> 200, management console only (no guest yet)
    GET    /api/vms                -> 200 []          (nothing created at startup)
    GET    /api/vms/pool           -> 200             (3 entries + 1 issue, TOML verbatim)
    GET    /api/vms/browse         -> 200             (folders are folders, `/usr/bin` is startable)
    GET    /api/vms/browse         -> 200             (a missing folder is an empty listing + reason)
    POST   /api/vms/create         -> 400             (a config path that does not exist)
    POST   /api/vms/pool           -> 400             (a name that would escape the directory)
    POST   /api/vms/pool           -> 400             (text that is not a guest config)
    POST   /api/vms/99/start       -> 404             (neither registered nor pooled)
    POST   /api/vms/1/start        -> 200 running     (created on demand from its entry)
    GET    /api/consoles           -> 200             (the new guest console appeared)
    POST   /api/vms/1/start        -> 409             (already running)
    POST   /api/vms/2/start        -> 200 running     (second guest starts beside the first)
    POST   /api/vms/3/start        -> 200 running
    GET    /api/vms                -> 200             (exactly the three, all running)
    GET    /api/vms/pool           -> 200             (entries survive being started)
    GET    /ws/events              -> 101 snapshot     (then created/removed frames)
    DELETE /api/vms/2              -> 204             (close; a `removed` frame follows)
    GET    /api/vms/2              -> 404             (gone from the registry)
    GET    /api/consoles           -> 200             (its console lane is gone too)
    POST   /api/vms/2/start        -> 200 running     (restart from the same entry,
                                                       announced as `created`)
    POST   /api/vms/create × 8     -> 200             (fill every guest console lane)
    GET    /api/consoles           -> 200              (8 guests + management)
    POST   /api/vms/1/start        -> 503             (no console lane left)
    GET    /api/vms/1              -> 404             (and no half-created VM behind it)
    DELETE /api/vms/{filler}       -> 204             (frees one lane)
    POST   /api/vms/1/start        -> 200 running     (the freed lane is reusable)
    DELETE /api/vms/{1,filler×7}   -> 204             (close everything)
    GET    /api/vms                -> 200 []          (registry empty again)
    GET    /api/vms/pool           -> 200             (pool unchanged throughout)

Every start is additionally checked against `guest_entry_count` from the VM
detail: the hypervisor's vCPU run loop increments it only after the guest has
actually (re-)entered, so a start that merely flips a status without running the
guest cannot pass. The pool listing is compared against the fixture files in
`sh/` byte for byte, so a listing that invents or mangles configs cannot pass,
and entries are matched by id rather than by position so the assertions do not
depend on the directory's enumeration order. The browser surfaces are checked
against the registry rather than against each other: consoles must appear and
disappear with the VM, the manifest must only declare panels this build can
serve, and the event frames must follow the create/remove cycle.
"""

import base64
import json
import os
import select
import socket
import struct
import sys
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
# Deadline for VM state transitions (boot, delete): stays well below the case
# `timeout` (600s) so a stuck transition fails on the probe, not on QEMU.
POLL_DEADLINE = 120.0
POLL_INTERVAL = 1.0
# The event watcher samples the registry every 250ms, so a frame can take a
# moment to arrive; this is far above that and only bounds a broken channel.
EVENT_TIMEOUT = 60.0

# The pool entries the `sh/` pipeline injected, keyed by VM id.
POOL_ENTRIES = {
    1: "pool-guest-1.toml",
    2: "pool-guest-2.toml",
    3: "pool-guest-3.toml",
}
BROKEN_ENTRY = "pool-broken.toml"

# Guest console lanes the build provides, excluding the management console. The
# lane-limit phase fills all of them with cheap configs (a 16M guest region, no
# device, never started) and then starts a pool entry to observe the refusal.
GUEST_CONSOLE_LANES = 8
FILLER_ID_BASE = 21
# A filler only has to be created, never run: it exists to occupy one console
# lane, so its region is far smaller than a bootable guest's.
FILLER_TOML = """[base]
id = {vm_id}
name = "lane-filler-{vm_id}"
cpu_num = 1
phys_cpu_ids = [1]

[kernel]
entry_point = 0x8020_0000
image_location = "fs"
kernel_path = "/guest/arceos/arceos-ivc-publisher.bin"
kernel_load_addr = 0x8020_0000
dtb_load_addr = 0x8000_0000
memory_regions = [[0x8000_0000, 0x1000000, 0x7, 0]]
"""


def request(method, path, body=None):
    """One HTTP request; returns (status, parsed JSON or None).

    The control plane has no authentication, so no request carries an
    Authorization header. A non-2xx response is not an error here — the caller
    asserts the status. A transport error (connection refused/reset/timeout
    while the guest server is coming up, or while it is busy creating a guest)
    raises RuntimeError for the caller to retry or verify.
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
        # `resp.read()` raises a bare socket timeout (an OSError) that the
        # URLError handler does not wrap. QEMU's user-mode hostfwd accepts the
        # host-side connection before the in-guest server binds, so a first
        # request can stall; retry instead of crashing in the boot window.
        raise RuntimeError("request %s %s failed: %s" % (method, path, err))
    if not raw:
        return status, None
    return status, json.loads(raw.decode("utf-8"))


def get(path, label):
    """One GET, retried until the deadline.

    The management API runs on a single-threaded runtime *inside* the guest and
    a create or start request builds the VM synchronously, so the server stops
    answering while it reads a guest kernel image and builds the VM. A read that
    lands in that window times out at the transport level; retrying keeps the
    probe deterministic without turning a busy server into a failure.
    """
    start = time.monotonic()
    while True:
        try:
            return request("GET", path)
        except RuntimeError as error:
            if time.monotonic() - start > POLL_DEADLINE:
                raise AssertionError("%s: %s (and it never recovered)" % (label, error))
            time.sleep(POLL_INTERVAL)


def check(label, actual, expected):
    """Assert an observed value, printing a progress line."""
    if actual != expected:
        raise AssertionError("%s was %r, expected %r" % (label, actual, expected))
    print("  pool http probe: %s -> %r" % (label, actual))


def expect_status(label, actual, expected):
    """Assert a status code, printing a progress line."""
    if actual != expected:
        raise AssertionError("%s returned %s, expected %s" % (label, actual, expected))
    print("  pool http probe: %s -> %s" % (label, actual))


def poll_ready():
    """Poll `GET /api/vms` until it returns 200 or the connect deadline passes."""
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
                print("  pool http probe: guest management HTTP server reachable")
                return
        except RuntimeError:
            pass
        time.sleep(POLL_INTERVAL)


def fixture_text(name):
    """Read one injected fixture from the case directory on the host."""
    path = os.path.join(CASE_DIR, "sh", name)
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read()


def pool_body(label):
    """Fetch the pool listing, asserting 200 and the expected shape."""
    status, body = get("/api/vms/pool", label)
    expect_status(label, status, 200)
    if not isinstance(body, dict):
        raise AssertionError("%s did not return a JSON object: %r" % (label, body))
    for field in ("directory", "sources", "entries", "issues"):
        if field not in body:
            raise AssertionError("%s had no %s field: %r" % (label, field, body))
    if not isinstance(body["entries"], list) or not isinstance(body["issues"], list):
        raise AssertionError("%s had non-list entries/issues: %r" % (label, body))
    return body


def check_pool_matches_fixtures(label, body):
    """Assert the listing holds exactly the injected valid entries, verbatim.

    Entries are matched by id, so the check does not depend on the directory's
    enumeration order. Each reported `toml` must equal the fixture file byte for
    byte: the pool is the authority for the config, so a listing that drops,
    reorders into the wrong ids, or rewrites configs cannot pass.
    """
    entries = {entry.get("id"): entry for entry in body["entries"]}
    if sorted(entries) != sorted(POOL_ENTRIES):
        raise AssertionError(
            "%s listed ids %r, expected %r"
            % (label, sorted(entries), sorted(POOL_ENTRIES))
        )
    for vm_id, fixture in POOL_ENTRIES.items():
        entry = entries[vm_id]
        for field in ("id", "name", "path", "toml", "source"):
            if field not in entry:
                raise AssertionError("%s entry %d had no %s: %r" % (label, vm_id, field, entry))
        # Every entry says which folder it came from, which is what makes a
        # config in the second folder traceable instead of anonymous.
        if entry["source"] != body["directory"]:
            raise AssertionError(
                "%s entry %d came from %r, expected %r"
                % (label, vm_id, entry["source"], body["directory"])
            )
        expected_toml = fixture_text(fixture)
        if entry["toml"] != expected_toml:
            raise AssertionError(
                "%s entry %d returned a TOML body that differs from %s"
                % (label, vm_id, fixture)
            )
        if not isinstance(entry["name"], str) or not entry["name"]:
            raise AssertionError("%s entry %d had no name: %r" % (label, vm_id, entry))
    print(
        "  pool http probe: %s -> ids %r with TOML verbatim from sh/"
        % (label, sorted(entries))
    )


def check_pool_issue(label, body):
    """Assert the unusable fixture is reported with its kind and path."""
    issues = [
        issue
        for issue in body["issues"]
        if isinstance(issue, dict) and issue.get("path", "").endswith(BROKEN_ENTRY)
    ]
    if len(issues) != 1:
        raise AssertionError(
            "%s reported %d issues for %s, expected 1: %r"
            % (label, len(issues), BROKEN_ENTRY, body["issues"])
        )
    issue = issues[0]
    check("%s issue kind" % label, issue.get("kind"), "invalid-toml")
    if not isinstance(issue.get("detail"), str) or not issue["detail"]:
        raise AssertionError("%s issue had no detail text: %r" % (label, issue))
    print("  pool http probe: %s -> %s reported as %r" % (label, BROKEN_ENTRY, issue["kind"]))


def check_folder_browsing():
    """Browsing lists folders as folders and configs as parsed candidates.

    Both halves matter: a folder reported as an unreadable file is a browse that
    cannot be walked, and a config listed as a folder is a browse that cannot be
    used. A directory that is not there is a visible empty listing with a reason,
    not a failed request.
    """
    status, body = get("/api/vms/browse?path=/usr", "GET /api/vms/browse?path=/usr")
    expect_status("GET /api/vms/browse?path=/usr", status, 200)
    check("browse path", body.get("path"), "/usr")
    check("browse parent", body.get("parent"), "/")
    listed = {item.get("path"): item.get("name") for item in body.get("directories", [])}
    if "/usr/bin" not in listed:
        raise AssertionError("browse did not list /usr/bin as a folder: %r" % (body,))
    for issue in body.get("issues", []):
        if issue.get("path") == "/usr/bin":
            raise AssertionError(
                "browse reported the pool folder as unusable (%s): %r"
                % (issue.get("kind"), issue)
            )
    print("  pool http probe: GET /api/vms/browse?path=/usr -> %r" % sorted(listed))

    # The pool folder itself holds the case's fixtures, so browsing it must
    # offer the same startable ids the pool lists.
    status, body = get("/api/vms/browse?path=/usr/bin", "GET /api/vms/browse?path=/usr/bin")
    expect_status("GET /api/vms/browse?path=/usr/bin", status, 200)
    check("browse has no subdirectories", body.get("directories"), [])
    check(
        "browse lists the pooled ids",
        sorted(entry.get("id") for entry in body.get("entries", [])),
        sorted(POOL_ENTRIES),
    )
    check(
        "browse reports the unusable file",
        sorted(issue.get("kind") for issue in body.get("issues", [])),
        ["invalid-toml"],
    )

    status, body = get(
        "/api/vms/browse?path=/no/such/directory",
        "GET /api/vms/browse?path=/no/such/directory",
    )
    expect_status("GET /api/vms/browse?path=/no/such/directory", status, 200)
    check("absent folder has no entries", body.get("entries"), [])
    check(
        "absent folder reports why",
        [issue.get("kind") for issue in body.get("issues", [])],
        ["directory-unavailable"],
    )


def check_config_inputs():
    """Creating from a path and writing to the pool reject what they cannot use.

    Only refusals are exercised here: a valid save would add a file to the case's
    pool directory and change what every later phase lists, and the accepted path
    is already covered by `sh/` provisioning. The refusals are the part that must
    not regress — a config path that is not there and a pool name that could
    escape its directory alike have to be errors.
    """
    status, _ = request(
        "POST", "/api/vms/create", json.dumps({"path": "/no/such/config.toml"})
    )
    expect_status("POST /api/vms/create (absent path)", status, 400)

    # A directory is not a config either: the path form reads a file.
    status, _ = request("POST", "/api/vms/create", json.dumps({"path": "/usr/bin"}))
    expect_status("POST /api/vms/create (directory path)", status, 400)

    status, _ = request("POST", "/api/vms/create", json.dumps({}))
    expect_status("POST /api/vms/create (no body fields)", status, 400)

    valid_toml = fixture_text(POOL_ENTRIES[1])
    for name in ("", "guest", "../escape.toml", ".hidden.toml"):
        status, _ = request(
            "POST", "/api/vms/pool", json.dumps({"name": name, "toml": valid_toml})
        )
        expect_status("POST /api/vms/pool (name %r)" % name, status, 400)
    status, _ = request(
        "POST",
        "/api/vms/pool",
        json.dumps({"name": "broken.toml", "toml": "base = { id = 1,"}),
    )
    expect_status("POST /api/vms/pool (unparsable toml)", status, 400)


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
    print("  pool http probe: %s -> %r" % (label, sorted(routes)))
    return routes


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
    each declared link is what makes the declaration falsifiable: a table entry
    whose method and route disagree cannot pass. `{id}` is filled with a VM id
    this probe never creates and `{endpoint}` with the management lane, so every
    call stays side-effect free.
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
    print("  pool http probe: manifest links all served (%d)" % calls)


def check_manifest():
    """Assert the capability declaration describes this build.

    The case builds `http-axum` and `browser-console` together, so all three
    panels must be declared, each with the verbs its routes implement. Every
    declared panel is then checked against the route that backs it, so a
    declaration that survives while its code path is dropped cannot pass — the
    same assertions run against builds with fewer features in the
    `http-control-plane` and `browser-console` cases.
    """
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
    for kind, route in (("vms", "/api/vms"), ("console", "/api/consoles")):
        if kind in declared:
            status, _ = get(route, "%s backing route" % kind)
            expect_status("%s panel backing route %s" % (kind, route), status, 200)
    for panel in panels:
        if not isinstance(panel.get("title"), str) or not panel["title"]:
            raise AssertionError("manifest panel had no title: %r" % (panel,))
        if not isinstance(panel.get("root"), str) or not panel["root"]:
            raise AssertionError("manifest panel had no root: %r" % (panel,))
    check_manifest_links(panels)


class WebSocket:
    """Small RFC 6455 client sufficient for the in-guest socket checks."""

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

    def close(self):
        self.stream.close()


def open_websocket(path):
    """Upgrade one WebSocket route from the probe's own origin."""
    parsed = urllib.parse.urlsplit(BASE)
    host = parsed.hostname
    port = parsed.port or 80
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    stream = socket.create_connection((host, port), timeout=REQUEST_TIMEOUT)
    stream.settimeout(REQUEST_TIMEOUT)
    handshake = (
        "GET %s HTTP/1.1\r\n"
        "Host: %s:%d\r\n"
        "Origin: %s\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Key: %s\r\n\r\n"
    ) % (path, host, port, BASE, key)
    stream.sendall(handshake.encode("ascii"))
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = stream.recv(4096)
        if not chunk:
            raise AssertionError("%s closed during the WebSocket upgrade" % path)
        response += chunk
    response_head, buffered = response.split(b"\r\n\r\n", 1)
    if not response_head.startswith(b"HTTP/1.1 101"):
        raise AssertionError("%s upgrade returned %r" % (path, response_head))
    print("  pool http probe: %s upgrade -> 101" % path)
    return WebSocket(stream, buffered)


def expect_websocket(path):
    """Assert one console route upgrades, then close it again.

    This is the cheapest proof that a declared `console`/`shell` panel is backed
    by a real socket in this build.
    """
    websocket = open_websocket(path)
    websocket.close()


def read_json_frame(websocket, deadline):
    """Return the next frame decoded as JSON, or fail once `deadline` passes.

    Waiting for readability with a short `select` rather than relying on the
    socket timeout keeps a quiet channel a clear `TimeoutError` at the frame
    boundary: the caller reports "no event arrived" instead of a transport
    error, and a frame cannot be truncated by a timeout in the middle of it.
    """
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError()
        ready, _, _ = select.select([websocket.stream], [], [], min(remaining, 1.0))
        if not ready:
            continue
        opcode, payload = websocket.recv_frame()
        if opcode == 8:
            raise AssertionError("event socket closed while waiting for a frame")
        if opcode not in (1, 2):
            raise AssertionError("event socket sent unexpected opcode %d" % opcode)
        return json.loads(payload.decode("utf-8"))


def expect_frame(predicate, description):
    """Read frames until `predicate(frame)` holds, or fail on the deadline."""
    deadline = time.monotonic() + EVENT_TIMEOUT
    seen = []
    while True:
        try:
            frame = read_json_frame(EVENTS_SOCKET, deadline)
        except TimeoutError:
            raise AssertionError(
                "no event matched %s within %.0fs; frames seen: %r"
                % (description, EVENT_TIMEOUT, seen)
            )
        seen.append(frame)
        if predicate(frame):
            print("  pool http probe: event %s -> %r" % (description, frame))
            return frame


def vm_status(body):
    if not isinstance(body, dict) or not isinstance(body.get("status"), str):
        raise AssertionError("VM detail response had no status: %r" % (body,))
    return body["status"]


def poll_vm_status(vm_id, expected):
    """Poll `GET /api/vms/{id}` until it reports `expected`.

    `start` returns once the request is accepted, so the running state is
    observed through the detail endpoint. A transition that never arrives fails
    on the poll deadline.
    """
    start = time.monotonic()
    last = None
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] never reached %s within %.0fs (last %r)"
                % (vm_id, expected, POLL_DEADLINE, last)
            )
        status, body = get("/api/vms/%d" % vm_id, "VM[%d] detail" % vm_id)
        if status == 200:
            last = vm_status(body)
            if last == expected:
                print("  pool http probe: VM[%d] -> status %s" % (vm_id, expected))
                return body
        time.sleep(POLL_INTERVAL)


def check_guest_ran(vm_id):
    """Poll until the guest has entered the vCPU run loop at least once.

    `guest_entry_count` is incremented by the hypervisor only after a successful
    guest entry, so this distinguishes a started guest from a start that only
    flipped a status. It must be polled, not read once: `start` is accepted and
    the status flips to `running` before the vCPU task has entered the guest, so
    a single read can observe 0. A guest that never enters fails on the poll
    deadline.
    """
    start = time.monotonic()
    last = None
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] guest_entry_count never reached 1 within %.0fs (last report %r)"
                % (vm_id, POLL_DEADLINE, last)
            )
        status, body = get("/api/vms/%d" % vm_id, "VM[%d] detail" % vm_id)
        if status == 200 and isinstance(body, dict):
            last = body.get("guest_entry_count")
            if isinstance(last, int) and last >= 1:
                print("  pool http probe: VM[%d] -> guest_entry_count %d" % (vm_id, last))
                return
        time.sleep(POLL_INTERVAL)


def start_vm(vm_id, expected_body_status="running"):
    """Start one pool entry through the control API and verify it runs."""
    try:
        status, body = request("POST", "/api/vms/%d/start" % vm_id)
        expect_status("POST /api/vms/%d/start" % vm_id, status, 200)
        check("POST /api/vms/%d/start status" % vm_id, vm_status(body), expected_body_status)
    except RuntimeError as error:
        # Creating the guest synchronously (kernel image read from the host
        # filesystem, VM build, vCPU start) can outlast the request timeout, so
        # a lost response does not mean the request was refused. Decide by the
        # resulting state instead: the VM has to appear and run, or the poll
        # below fails on its deadline.
        print("  pool http probe: POST /api/vms/%d/start -> no response (%s); checking state" % (vm_id, error))
    poll_vm_status(vm_id, "running")
    check_guest_ran(vm_id)


def create_vm(toml):
    """Create one VM from a TOML body, asserting it was accepted."""
    status, body = request("POST", "/api/vms/create", body=json.dumps({"toml": toml}))
    expect_status("POST /api/vms/create (id %s)" % _toml_id(toml), status, 200)
    return body


def _toml_id(toml):
    """Read `base.id` out of a generated config for labelling only."""
    for line in toml.splitlines():
        if line.startswith("id = "):
            return int(line.split("=", 1)[1].strip())
    return "?"


def close_vm(vm_id):
    """Close one guest: `delete` is the close action, not `stop`."""
    try:
        status, _ = request("DELETE", "/api/vms/%d" % vm_id)
        expect_status("DELETE /api/vms/%d" % vm_id, status, 204)
    except RuntimeError as error:
        print(
            "  pool http probe: DELETE /api/vms/%d -> no response (%s); checking state"
            % (vm_id, error)
        )
    # A lost response also covers the case where the delete was accepted: the
    # registry has to show the VM gone either way.
    poll_vm_gone(vm_id)


def poll_vm_gone(vm_id):
    """Poll `GET /api/vms/{id}` until it returns 404."""
    start = time.monotonic()
    last = None
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] was still registered within %.0fs (last status %r)"
                % (vm_id, POLL_DEADLINE, last)
            )
        status, _ = get("/api/vms/%d" % vm_id, "VM[%d] detail" % vm_id)
        if status == 404:
            print("  pool http probe: GET /api/vms/%d after delete -> 404" % vm_id)
            return
        last = status
        time.sleep(POLL_INTERVAL)


def list_report(label):
    """Fetch `GET /api/vms` and return {id: status} for the registered VMs."""
    status, body = get("/api/vms", label)
    expect_status(label, status, 200)
    if not isinstance(body, list):
        raise AssertionError("%s did not return a JSON array: %r" % (label, body))
    report = {}
    for item in body:
        if not isinstance(item, dict):
            raise AssertionError("%s listed a non-object entry: %r" % (label, item))
        report[item.get("id")] = item.get("status")
    print("  pool http probe: %s -> %r" % (label, report))
    return report


EVENTS_SOCKET = None


def phase_pool():
    """The pool is a candidate list: nothing in it is created at startup."""
    check("VM registry at startup", list_report("GET /api/vms"), {})
    check("console routes at startup", sorted(console_routes("GET /api/consoles")), ["axvisor"])

    pool = pool_body("GET /api/vms/pool")
    check("pool directory", pool["directory"], "/usr/bin")
    # The read sources are the folder a config is written to and, last, the guest
    # tree: a config anywhere under the tree is a candidate, and the write folder
    # is read first so it keeps precedence for a taken id.
    check("pool folders", pool["sources"], ["/usr/bin", "/guest"])
    check_pool_matches_fixtures("GET /api/vms/pool", pool)
    check_pool_issue("GET /api/vms/pool", pool)
    check_folder_browsing()
    check_config_inputs()

    # An id that is neither registered nor pooled is unknown, not created.
    status, _ = request("POST", "/api/vms/99/start")
    expect_status("POST /api/vms/99/start", status, 404)


def phase_lifecycle():
    """Start-on-demand, duplicate start, coexistence, close and restart."""
    start_vm(1)
    # The guest console lane follows the registry, not a startup snapshot.
    routes = console_routes("GET /api/consoles after start 1")
    check("console routes after start 1", sorted(routes), ["axvisor", "vm-1"])
    check("guest console name", routes["vm-1"], "pool-guest-1")
    # A declared console panel must be backed by a socket a browser can open.
    expect_websocket("/ws/vm-1")

    status, _ = request("POST", "/api/vms/1/start")
    expect_status("POST /api/vms/1/start (running)", status, 409)

    start_vm(2)
    start_vm(3)
    check(
        "three pooled guests running",
        list_report("GET /api/vms"),
        {1: "running", 2: "running", 3: "running"},
    )
    check(
        "three guest consoles",
        sorted(console_routes("GET /api/consoles after starts")),
        ["axvisor", "vm-1", "vm-2", "vm-3"],
    )

    # Starting is not consuming: every entry is still a candidate.
    body = pool_body("GET /api/vms/pool after starts")
    check_pool_matches_fixtures("GET /api/vms/pool after starts", body)


def phase_events():
    """The event channel follows the same create/close cycle."""
    global EVENTS_SOCKET
    EVENTS_SOCKET = open_websocket("/ws/events")
    snapshot = read_json_frame(EVENTS_SOCKET, time.monotonic() + EVENT_TIMEOUT)
    check("event snapshot type", snapshot.get("type"), "snapshot")
    check(
        "event snapshot list",
        {vm["id"]: vm["status"] for vm in snapshot.get("vms", [])},
        {1: "running", 2: "running", 3: "running"},
    )

    # Close one guest and start it again from the same entry: this is the
    # repeated start/close cycle the pool exists for, observed through both the
    # REST list and the event socket.
    close_vm(2)
    expect_frame(
        lambda frame: frame.get("type") == "removed" and frame.get("id") == 2,
        "removed VM[2]",
    )
    check(
        "registry after closing one guest",
        list_report("GET /api/vms after close"),
        {1: "running", 3: "running"},
    )
    check(
        "console routes after close",
        sorted(console_routes("GET /api/consoles after close")),
        ["axvisor", "vm-1", "vm-3"],
    )
    body = pool_body("GET /api/vms/pool after close")
    check_pool_matches_fixtures("GET /api/vms/pool after close", body)

    start_vm(2)
    expect_frame(
        lambda frame: frame.get("type") == "created" and frame.get("id") == 2,
        "created VM[2]",
    )
    EVENTS_SOCKET.close()
    EVENTS_SOCKET = None


def phase_lane_limit():
    """A full console lane table refuses the start instead of hiding it."""
    # Start from an empty registry so the lane arithmetic is exact: eight
    # fillers take all eight guest lanes and the ninth guest is refused. The
    # guests from the earlier phases have to go first, otherwise the fillers
    # would consume the lanes they still hold.
    for vm_id in (1, 2, 3):
        close_vm(vm_id)
    check(
        "VM registry before filling the lanes",
        list_report("GET /api/vms before filling lanes"),
        {},
    )

    fillers = [FILLER_ID_BASE + index for index in range(GUEST_CONSOLE_LANES)]
    for vm_id in fillers:
        create_vm(FILLER_TOML.format(vm_id=vm_id))
    check(
        "console lanes are full",
        len(console_routes("GET /api/consoles with full lanes")),
        GUEST_CONSOLE_LANES + 1,
    )

    # Every lane is taken by a VM that never runs, so starting a pool entry has
    # to fail with the resource-exhaustion status, and the failed creation must
    # not leave the VM behind.
    status, _ = request("POST", "/api/vms/1/start")
    expect_status("POST /api/vms/1/start (no lane left)", status, 503)
    status, _ = get("/api/vms/1", "VM[1] after the refused start")
    expect_status("GET /api/vms/1 after the refused start", status, 404)

    # Closing one guest frees exactly its lane for the next start.
    close_vm(fillers[0])
    start_vm(1)
    check(
        "console routes after the lane was freed",
        sorted(console_routes("GET /api/consoles after freeing a lane")),
        ["axvisor", "vm-1"] + ["vm-%d" % vm_id for vm_id in fillers[1:]],
    )

    close_vm(1)
    for vm_id in fillers[1:]:
        close_vm(vm_id)
    check("VM registry after closing all guests", list_report("GET /api/vms after close all"), {})
    check(
        "console routes after closing all guests",
        sorted(console_routes("GET /api/consoles after close all")),
        ["axvisor"],
    )
    body = pool_body("GET /api/vms/pool at the end")
    check_pool_matches_fixtures("GET /api/vms/pool at the end", body)


def check_dashboard():
    """The dashboard is served in this build too (`web-ui` plus `fs`).

    This case is about the pool, not about the UI, so the check is deliberately
    minimal: the shell resolves and the page it serves is the embedded bundle
    rather than a directory listing or a 404. The dashboard's own contract is
    asserted by the `qemu-web-ui` case.
    """
    # Raw on purpose: `get()` parses JSON, and this response is HTML.
    deadline = time.monotonic() + POLL_DEADLINE
    while True:
        try:
            with urllib.request.urlopen(BASE + "/", timeout=REQUEST_TIMEOUT) as resp:
                expect_status("GET / (dashboard shell)", resp.status, 200)
                body = resp.read()
            break
        except urllib.error.HTTPError as error:
            raise AssertionError("GET / (dashboard shell) returned %d" % error.code)
        except (urllib.error.URLError, OSError) as error:
            if time.monotonic() >= deadline:
                raise AssertionError("GET / (dashboard shell) failed: %s" % error)
            time.sleep(POLL_INTERVAL)
    for marker in (b'<div id="root">', b"/assets/"):
        if marker not in body:
            raise AssertionError("dashboard shell is missing %r" % marker)
    print("  pool http probe: GET / -> embedded dashboard shell")


def main():
    poll_ready()
    check_dashboard()
    check_manifest()
    expect_websocket("/ws/axvisor")
    phase_pool()
    phase_lifecycle()
    phase_events()
    phase_lane_limit()
    print("  pool http probe: PASS")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as exc:
        print("  pool http probe: FAILED: %s" % exc, file=sys.stderr)
        sys.exit(1)
    except Exception as exc:
        print("  pool http probe: ERROR: %s" % exc, file=sys.stderr)
        sys.exit(2)
