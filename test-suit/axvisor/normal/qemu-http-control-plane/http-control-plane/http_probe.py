#!/usr/bin/env python3
"""Host-side probe asset for the AxVisor management HTTP control plane.

Case asset for the `http-control-plane` test case
(`test-suit/axvisor/normal/qemu-http-control-plane/`). It owns the *test
content* — the concrete requests, the default guest's fixture (which now
lives in the shared config library), and the
assertions — and can evolve independently of the axbuild runner.

The generic axbuild probe runner
(`scripts/axbuild/src/axvisor/test/http_probe.rs`) executes this script after
the QEMU hostfwd port is reachable, then treats the exit code as the verdict:
0 = all assertions passed, nonzero = a step failed. The script dials the axum
management API running *inside* the AxVisor guest through QEMU user-mode
networking hostfwd. Nothing in the hypervisor knows a test is running.

Environment (set by the generic runner):

    AXVISOR_HTTP_BASE            http://127.0.0.1:<host_port> (forwarded)
    AXVISOR_BUILTIN_CONFIG_DIR   directory holding packaged `vm-1.toml`
                                 (default: this file's directory)
    AXVISOR_HTTP_CASE_DIR        case directory holding test fixtures
                                 (default: this file's directory)
    AXVISOR_HTTP_CONNECT_TIMEOUT seconds for the initial reachability wait
    AXVISOR_HTTP_REQUEST_TIMEOUT seconds per HTTP request

Optional probe-only override:

    AXVISOR_HTTP_COMPLETION_TIMEOUT seconds for the completion-awaited routes
                                 (default 120)

The probe drives the whole `/api/vms` lifecycle contract in one boot —
error mapping and the pause/resume cycles, the Stopped→start
restart, and the destroy-then-recreate resource re-acquire regression —
mirroring `os/axvisor/doc/http-control-plane-quickstart.md`:

    GET    /api/vms            -> 200            (list; id=1 present)
    GET    /api/vms/1          -> 200 ready      (detail; id/name/cpu_num/vcpu_states/guest_entry_count)
    GET    /api/vms/not-an-id  -> 404            (non-numeric id)
    GET    /api/vms/999        -> 404            (unknown VM)
    DELETE /api/vms/999        -> 404            (unknown VM; no auth header required)
    POST   /api/vms/create {}  -> 400            (missing toml)
    POST   /api/vms/create <bad toml> -> 400     (invalid TOML)
    POST   /api/vms/create <fields>   -> 409     (a file the config names is not in place)
    GET    /api/vms/browse            -> 200     (a file candidate for a `file` field, with length)
    POST   /api/vms/create <toml>     -> 409     (a device's backing file is not in place)
    POST   /api/vms/create <fields>   -> 200     (file in place: created, config written to the guest tree, then removed)
    POST   /api/vms/999/start  -> 404            (unknown VM)
    POST   /api/vms/999/stop   -> 404            (unknown VM)
    POST   /api/vms/999/pause  -> 404            (unknown VM)
    POST   /api/vms/999/resume -> 404            (unknown VM)
    POST   /api/vms/create     -> 409            (id=1 already registered)
    POST   /api/vms/1/pause    -> 409            (pause from Ready)
    POST   /api/vms/1/resume   -> 409            (resume from Ready)
    POST   /api/vms/1/start    -> 200 -> running (async=false; start awaits completion)
    POST   /api/vms/1/start    -> 409            (already running; start is not idempotent)
    POST   /api/vms/1/resume   -> 200 -> running (already running; idempotent, async=false)
    POST   /api/vms/1/pause    -> 200 -> paused  (async=false; waits for quiescence)
    POST   /api/vms/1/pause    -> 200 -> paused  (already paused; idempotent, no new park)
    POST   /api/vms/1/resume   -> 200 -> running (async=false; genuine wake)
    POST   /api/vms/1/resume   -> 200 -> running (already running; idempotent)
    POST   /api/vms/1/pause    -> 200 -> paused  (second suspend/wake cycle)
    POST   /api/vms/1/resume   -> 200 -> running (guest re-entered)
    POST   /api/vms/1/stop     -> 200 -> stopped (async=false; waits for teardown)
    POST   /api/vms/1/stop     -> 200 -> stopped (already stopped; idempotent)
    POST   /api/vms/1/start    -> 200 -> running (restart from Stopped; fresh run)
    POST   /api/vms/1/stop     -> 200 -> stopped (restarted run stops again)
    DELETE /api/vms/1          -> 204 -> 404     (gone)
    POST   /api/vms/create     -> 200 {id:1}     (recreate after delete)
    POST   /api/vms/create     -> 409            (id=1 re-registered)
    POST   /api/vms/1/start    -> 200 -> running (recreated VM usable)
    POST   /api/vms/1/stop     -> 200 -> stopped
    DELETE /api/vms/1          -> 204 -> 404     (cleanup)

Completion semantics (grounded in `os/axvisor/src/control/transport/api/vm.rs`
and the `virtualization/axvm/src/control.rs` owner):

  * Every lifecycle action waits for its manager operation to complete before responding,
    so a 200 already implies the owner postcondition and every response reports
    `"async": false`. The probe still polls the detail for the terminal status
    where the runtime publishes it after the operation returns.
  * Reaching an already-satisfied target is idempotent for `pause`/`resume`/
    `stop` (HTTP 200): a repeated pause while `Paused` returns 200 without
    requesting a duplicate park, a repeated resume while `Running` and a
    repeated stop while `Stopped` return 200 without a new wake/teardown.
    `start` is deliberately *not* idempotent: starting an already-`Running` VM
    stays a 409 conflict.
  * Restarting from `Stopped` is a supported transition (the old scheduling
    limitation is gone): `start` from `Stopped` returns 200 and the fresh run
    must actually enter the guest before it is stopped again.

The fixture pins the vCPU to Core 1 (`phys_cpu_ids = [1]`) with the management
console on Core 0, so a resume must wake a vCPU parked on a *non-primary*
pinned CPU. A status flip is not enough evidence that a pause/resume actually
worked: a broken wake path could report `running` without the vCPU ever
re-entering the guest. To distinguish a genuine wake from a status flip, the
probe reads the HTTP-exposed `guest_entry_count` field of the VM detail: the
hypervisor's vCPU run loop increments it after every guest (re-)entry (once the
guest has actually entered and exited), so it is independent re-execution
evidence. The probe therefore asserts, after *every* resume, that
`guest_entry_count` strictly advanced — so a wake that only flips status makes
the probe exit nonzero and fails the case.

The same `Paused` status has the dual problem on the *pause* side: the probe
also reads the HTTP-exposed `guest_park_count` — the hypervisor increments it
once each time a vCPU actually observes the suspended state and parks. (This
*observes* a vCPU park — it is a VM-level aggregate, not a per-vCPU value, and
is **not** a per-vCPU quiescence API.) After *every* pause, the probe polls
until `guest_park_count` strictly advanced before sending the resume, so a
pause that never completes (or a status-only pause) makes the probe exit
nonzero and fail the case. While the VM is `Paused` the counters must stay
frozen, so the probe also asserts that a repeated pause does not advance
`guest_park_count` and that no guest re-entry happens until the resume.

Because `guest_entry_count` is published only after a *successful* guest
(re-)entry, a broken wake path or a faulting resume that never re-enters the
guest cannot advance it — making this probe the deterministic regression for
the failed-entry path as well. The counters are per-run: the owner publishes
`guest_entry_count == 0` and `guest_park_count == 0` while `Stopped` (the run
record is dropped) and restarts them from zero on the next run, so the
Stopped→start restart reads the fresh counters and requires the new run to
enter the guest before the next stop.

The last recreate -> start -> stop -> delete block is the resource re-acquire
regression: it proves destroy freed guest memory, vCPUs, devices, and the
registry entry so a fresh VM can be rebuilt from the installed boot files.
The create body uses the exact packaged `vm-1.toml` supplied through
`AXVISOR_BUILTIN_CONFIG_DIR`; its paths refer to `/guest/builtin/images` on
the disk root after the host initramfs has been detached and released.
"""

import json
import re
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("AXVISOR_HTTP_BASE", "http://127.0.0.1:8080").rstrip("/")
CASE_DIR = os.environ.get(
    "AXVISOR_HTTP_CASE_DIR", os.path.dirname(os.path.abspath(__file__))
)
CONNECT_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "5"))
# Lifecycle routes block inside the handler until the owner publishes its
# postcondition, so they get the full transition deadline instead of the short
# per-request timeout used by metadata/error checks and poll loops.
COMPLETION_TIMEOUT = float(
    os.environ.get("AXVISOR_HTTP_COMPLETION_TIMEOUT", "120")
)
# Deadline for VM state transitions (boot, pause, stop, delete): must stay well
# below the case `timeout` (600s) so a stuck transition fails on the probe, not
# on the QEMU timeout.
POLL_DEADLINE = 120.0
POLL_INTERVAL = 1.0


def request(method, path, body=None, timeout=None):
    """One HTTP request; returns (status, parsed JSON or None).

    The control plane has no authentication: no request carries an
    Authorization header, and each handler's own contract decides the status.

    `timeout` defaults to `REQUEST_TIMEOUT`; the completion-awaited lifecycle
    routes pass `COMPLETION_TIMEOUT` because their handler blocks until the
    owner's postcondition holds.

    A JSON `body` is sent with `Content-Type: application/json`. A non-2xx
    response is not an error here — the caller asserts the status. A transport
    error (connection refused/reset/timeout while the guest server is coming up
    or mid-transition) raises RuntimeError for the caller to retry or fail.
    """
    if timeout is None:
        timeout = REQUEST_TIMEOUT
    headers = {}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = body.encode("utf-8")
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.status
            raw = resp.read()
    except urllib.error.HTTPError as err:
        status = err.code
        raw = err.read()
    except urllib.error.URLError as err:
        raise RuntimeError("request %s %s failed: %s" % (method, path, err.reason))
    except OSError as err:
        # `resp.read()` raises a bare `socket.timeout` (an OSError) that the
        # URLError handler above does not wrap. QEMU's user-mode hostfwd accepts
        # the host-side connection as soon as QEMU starts, before the in-guest
        # management server binds, so a first request can stall to the request
        # timeout. Converting it to a retryable RuntimeError here lets the poll
        # loops retry instead of crashing the probe in the boot window.
        raise RuntimeError("request %s %s failed: %s" % (method, path, err))
    if not raw:
        return status, None
    return status, json.loads(raw.decode("utf-8"))


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
    this probe never creates, so every call stays side-effect free.
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
    print("  http probe: manifest links all served (%d)" % calls)


def request_raw(method, path, headers=None, body=None):
    """One request with explicit headers, returning (status, headers, payload).

    The chunk endpoint is the only place where the media type and the range
    header are part of the contract, so the probe has to be able to send them —
    and has to be able to send the *wrong* ones, which is how the refusal before
    the body is read gets asserted.
    """
    req = urllib.request.Request(
        BASE + path, data=body, headers=headers or {}, method=method
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            # The header object, not a dict: HTTP/1 lowercases header names on
            # the wire, and the offset header has to be found either way.
            return resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.headers or {}, err.read()
    except (OSError, urllib.error.URLError) as err:
        raise RuntimeError("request %s %s failed: %s" % (method, path, err))


def check_file_transfer():
    """Drive one real transfer: stage it, interrupt it, resume it, place it.

    The transfer is the one capability whose contract is a *sequence*, so the
    probe walks the sequence rather than poking one endpoint: a chunk that
    arrives out of order has to be refused with the offset it should have used,
    and a body whose frame shape is wrong has to be refused before it is read.

    The file it places is not cleaned up, and does not need to be: QEMU boots the
    root filesystem with `snapshot=on`, so nothing this probe writes survives the
    run. The assertions below therefore hold whether the image carries a file of
    that name or not — and the one that always holds is the second placement,
    which is refused precisely because the bytes are already at the final path.
    """
    parent = "/guest"
    folder = "probe-transfer"
    name = "probe-transfer.bin"
    payload = b"axvisor-transfer-probe\n" * 64
    total = len(payload)
    session = "probe-transfer"
    second = "probe-transfer-second"

    # An unknown session is a 404, not a guess: that is what a client which lost
    # its id has to see.
    status, _, _ = request_raw("HEAD", "/api/files/%s-absent" % session)
    check("HEAD /api/files (unknown session)", status, 404)

    # The target folder is *chosen*: a transfer into a directory that is not
    # there is refused instead of quietly creating it, because what the operator
    # picked and what the bytes landed in have to be the same directory.
    status, _, body = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps(
            {"id": "probe-missing-dir", "directory": parent + "/not-there", "total": total}
        ).encode("utf-8"),
    )
    check("POST /api/files (missing directory)", status, 404)
    if "not-there" not in body.decode("utf-8", "replace"):
        raise AssertionError("the refusal does not name the missing directory")

    # Making one is its own operation, one level at a time: the second call is a
    # conflict, so the plane never invents a different name either.
    status, _, _ = request_raw(
        "POST",
        "/api/files/dirs",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"parent": parent, "name": folder}).encode("utf-8"),
    )
    check("POST /api/files/dirs", status, 200)
    status, _, _ = request_raw(
        "POST",
        "/api/files/dirs",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"parent": parent, "name": folder}).encode("utf-8"),
    )
    check("POST /api/files/dirs (already there)", status, 409)
    directory = parent + "/" + folder

    status, headers, body = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps(
            {"id": session, "directory": directory, "total": total}
        ).encode("utf-8"),
    )
    check("POST /api/files", status, 200)
    opened = json.loads(body.decode("utf-8"))
    check("POST /api/files state", opened.get("state"), "uploading")
    check("POST /api/files offset header", headers.get("Upload-Offset"), "0")

    # Re-opening the same target is idempotent: it answers the session it has,
    # with the bytes that are really staged. A target that disagrees with what
    # the bytes were staged *under* is a conflict that names the truth, because
    # silently re-pointing the session would stream this folder's bytes into the
    # one the session first named.
    status, _, _ = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"id": session, "directory": directory, "total": total}).encode("utf-8"),
    )
    check("POST /api/files (same target again)", status, 200)
    status, _, body = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps(
            {"id": session, "directory": parent + "/linux", "total": total}
        ).encode("utf-8"),
    )
    check("POST /api/files (same id, other directory)", status, 409)
    if directory not in body.decode("utf-8", "replace"):
        raise AssertionError("the conflict does not name the directory the bytes are staged in")
    status, _, _ = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"id": session, "directory": directory, "total": total + 1}).encode(
            "utf-8"
        ),
    )
    check("POST /api/files (same id, other length)", status, 409)

    # The frame shape is checked before the body is read, so both of these are
    # refused without a chunk reaching the staging area.
    status, _, _ = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={"Content-Type": "text/plain", "Content-Range": "bytes 0-1/%d" % total},
        body=b"xx",
    )
    check("PATCH /api/files (wrong media type)", status, 400)
    status, _, _ = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "0-1/%d" % total,
        },
        body=b"xx",
    )
    check("PATCH /api/files (malformed range)", status, 400)

    # Lengths are converted to the target's usize before the staging file is
    # opened, and the inclusive range arithmetic is checked before the body is
    # read. Both hostile values must be rejected without changing the session.
    huge_total = 1 << 63
    status, _, _ = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps(
            {"id": "probe-huge-total", "directory": directory, "total": huge_total}
        ).encode("utf-8"),
    )
    check("POST /api/files (unrepresentable length)", status, 413)
    status, _, _ = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "bytes 0-%d/*" % sys.maxsize,
        },
        body=b"x",
    )
    check("PATCH /api/files (overflowing range)", status, 413)

    # A chunk that starts where the disk is not: the offset in the answer is the
    # one the client continues from, which is why the offset is read from the
    # file rather than from a counter.
    status, headers, _ = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "bytes 4-%d/%d" % (3 + len(payload), total),
        },
        body=payload,
    )
    check("PATCH /api/files (offset mismatch)", status, 409)
    check("PATCH /api/files conflict offset", headers.get("Upload-Offset"), "0")

    split = total // 2
    status, headers, body = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "bytes 0-%d/%d" % (split - 1, total),
        },
        body=payload[:split],
    )
    if status != 200:
        raise AssertionError(
            "PATCH /api/files (first chunk) returned %s: %s"
            % (status, body.decode("utf-8", "replace"))
        )
    check("PATCH /api/files (first chunk)", status, 200)
    check("PATCH /api/files first offset", headers.get("Upload-Offset"), str(split))

    status, headers, _ = request_raw("HEAD", "/api/files/%s" % session)
    check("HEAD /api/files (interrupted)", status, 200)
    check("HEAD /api/files offset", headers.get("Upload-Offset"), str(split))

    status, headers, _ = request_raw(
        "PATCH",
        "/api/files/%s" % session,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "bytes %d-%d/%d" % (split, total - 1, total),
        },
        body=payload[split:],
    )
    check("PATCH /api/files (second chunk)", status, 200)
    check("PATCH /api/files final offset", headers.get("Upload-Offset"), str(total))

    status, body = request("GET", "/api/files")
    check("GET /api/files", status, 200)
    listed = [entry for entry in body["files"] if entry.get("id") == session]
    if not listed:
        raise AssertionError("the completed session is missing from GET /api/files")
    check("GET /api/files state", listed[0].get("state"), "uploaded")
    check("GET /api/files written", listed[0].get("written"), total)

    # The placement step. A previous run on a cached image already put the file
    # there, which is the conflict the plane is supposed to report, so both
    # outcomes are correct — and the *second* placement below is the assertion
    # that holds either way: it is refused because the bytes are at the final
    # path, which is a real lookup in the guest filesystem.
    status, _, _ = request_raw(
        "POST",
        "/api/files/%s/place" % session,
        headers={"Content-Type": "application/json"},
        body=json.dumps({"name": name}).encode("utf-8"),
    )
    if status not in (200, 409):
        raise AssertionError("POST /api/files/place returned %s" % status)
    print("  http probe: place -> %d" % status)

    status, _, _ = request_raw(
        "POST",
        "/api/files",
        headers={"Content-Type": "application/json"},
        body=json.dumps({"id": second, "directory": directory, "total": total}).encode(
            "utf-8"
        ),
    )
    check("POST /api/files (second session)", status, 200)
    status, headers, _ = request_raw(
        "PATCH",
        "/api/files/%s" % second,
        headers={
            "Content-Type": "application/octet-stream",
            "Content-Range": "bytes 0-%d/%d" % (total - 1, total),
        },
        body=payload,
    )
    check("PATCH /api/files (second session)", status, 200)
    status, _, body = request_raw(
        "POST",
        "/api/files/%s/place" % second,
        headers={"Content-Type": "application/json"},
        body=json.dumps({"name": name}).encode("utf-8"),
    )
    check("POST /api/files/place (target exists)", status, 409)
    if name not in body.decode("utf-8", "replace"):
        raise AssertionError("the conflict does not name the existing target: %r" % body)

    # A placed file is not undone by forgetting its session: those bytes are the
    # file a guest config points at.
    status, _, _ = request_raw("DELETE", "/api/files/%s" % session)
    check("DELETE /api/files (placed)", status, 409)

    # The second session still only has staged bytes, so it is cleaned up here.
    status, _, _ = request_raw("DELETE", "/api/files/%s" % second)
    check("DELETE /api/files (staged)", status, 204)
    status, body = request("GET", "/api/files")
    check("GET /api/files (after drop)", status, 200)
    if any(entry.get("id") == second for entry in body["files"]):
        raise AssertionError("a dropped session is still listed")
    print("  http probe: transfer staged, interrupted, resumed and placed")



def fields_body(kernel_path, **overrides):
    """The form's create body: only the fields a form itself must type.

    The optional fields (`cpu_num`, `guest_type`, `cmdline`)
    stay absent unless a caller overrides them: the template fills them from
    its declared defaults, so a successful create is also proof those defaults
    reach a bootable guest, and the field set stays the declaration's to
    change rather than this probe's to restate.
    """
    fields = {
        "id": 4243,
        "name": "probe-fields",
        "kernel_path": kernel_path,
        # Hexadecimal text, the way a guest configuration writes it.
        "entry_point": "0x8020_0000",
        "kernel_load_addr": "0x8020_0000",
        "memory_base": "0x8000_0000",
        "memory_mb": 256,
    }
    fields.update(overrides)
    return json.dumps({"fields": fields})


def check_create_form(packaged_kernel_path):
    """The dashboard schema matches the current GuestConfig form contract."""
    status, body = request("GET", "/api/vms/schema")
    check("GET /api/vms/schema", status, 200)
    fields = {field["name"]: field for field in body.get("fields", [])}
    expected = {
        "id", "name", "guest_type", "cpu_num", "entry_point", "kernel_path",
        "kernel_load_addr", "cmdline", "memory_base", "memory_mb", "rootfs_path",
    }
    if set(fields) != expected:
        raise AssertionError("schema fields are %r" % (sorted(fields),))
    for required in (
        "id", "name", "kernel_path", "entry_point",
        "kernel_load_addr", "memory_base", "memory_mb",
    ):
        if not fields[required].get("required"):
            raise AssertionError("schema does not require `%s`" % required)
    if fields["kernel_path"].get("type") != "file":
        raise AssertionError("kernel_path is not a file field: %r" % fields["kernel_path"])
    if "console=" not in str(fields["cmdline"].get("default")):
        raise AssertionError("cmdline has no console default: %r" % fields["cmdline"])
    print("  http probe: schema advertises %d fields" % len(fields))

    status, _ = request(
        "POST",
        "/api/vms/create",
        fields_body(packaged_kernel_path, cpu_num=1 << 63),
    )
    check("POST /api/vms/create (oversized cpu count)", status, 400)
    status, _ = request(
        "POST",
        "/api/vms/create",
        fields_body(packaged_kernel_path, memory_mb=0),
    )
    check("POST /api/vms/create (zero memory)", status, 400)

    status, body = request("GET", "/api/vms/browse?path=/guest/builtin/images")
    check("GET /api/vms/browse (file candidates)", status, 200)
    listed = {entry["path"]: entry for entry in body.get("files", [])}
    kernel = listed.get(packaged_kernel_path)
    if kernel is None or not isinstance(kernel.get("size"), int) or kernel["size"] <= 0:
        raise AssertionError(
            "the packaged kernel %s is not offered as a file candidate: %r"
            % (packaged_kernel_path, body)
        )

    absent = "/guest/builtin/images/missing-form-kernel"
    status, body = request("POST", "/api/vms/create", fields_body(absent))
    check("POST /api/vms/create (fields, image missing)", status, 409)
    if absent not in json.dumps(body):
        raise AssertionError("missing form image was not named: %r" % body)

    status, body = request(
        "POST",
        "/api/vms/create",
        fields_body(packaged_kernel_path),
        timeout=COMPLETION_TIMEOUT,
    )
    check("POST /api/vms/create (fields, image present)", status, 200)
    check("created VM id", body.get("id"), 4243)
    status, _ = request("DELETE", "/api/vms/4243", timeout=COMPLETION_TIMEOUT)
    check("DELETE /api/vms/4243", status, 204)
    print("  http probe: form validation and creation use the current API")


def check(label, actual, expected):
    """Assert a status code, printing a progress line."""
    if actual != expected:
        raise AssertionError("%s returned %s, expected %s" % (label, actual, expected))
    print("  http probe: %s -> %s (expect %s)" % (label, actual, expected))


def vm_status(body):
    """Extract the top-level `status` string of a VM detail body."""
    if not isinstance(body, dict):
        raise AssertionError("VM detail response was not a JSON object")
    status = body.get("status")
    if not isinstance(status, str):
        raise AssertionError("VM detail response had no status string: %r" % (body,))
    return status


def check_vm_status(label, body, expected):
    status = vm_status(body)
    print("  http probe: %s -> status %s (expect %s)" % (label, status, expected))
    if status != expected:
        raise AssertionError(
            "%s reported status %s, expected %s" % (label, status, expected)
        )


def guest_entry_count(body):
    """Extract the hypervisor's per-run guest (re-)entry count.

    The vCPU run loop increments this *only* after a successful guest
    (re-)entry (once the guest has actually entered and exited); a failed entry
    that returns `Err` before the guest runs does not advance it. It is a VM-level
    aggregate (shared by every vCPU task, not per-vCPU), so it proves at least
    one vCPU re-executed — independent proof that the guest actually
    re-executed. The owner publishes 0 while the VM is `Stopped` (no run) and
    starts a fresh run's counters at 0, so a restart re-reads the counter.
    """
    if not isinstance(body, dict):
        raise AssertionError("VM detail response was not a JSON object")
    count = body.get("guest_entry_count")
    if not isinstance(count, int):
        raise AssertionError(
            "VM detail response had no integer guest_entry_count: %r" % (body,)
        )
    return count


def poll_guest_entries(vm_id, expected_min):
    """Poll `GET /api/vms/{id}` until `guest_entry_count >= expected_min`.

    Used to confirm the guest actually re-entered after a resume (or a fresh
    start), not merely that the HTTP status flipped. Fails on the poll deadline
    so a broken wake path makes the probe exit nonzero.
    """
    start = time.monotonic()
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] guest_entry_count never reached >= %d within %.0fs"
                % (vm_id, expected_min, POLL_DEADLINE)
            )
        try:
            status, body = request("GET", "/api/vms/%d" % vm_id)
            if status == 200 and guest_entry_count(body) >= expected_min:
                print(
                    "  http probe: VM[%d] -> guest_entry_count %d (>= %d)"
                    % (vm_id, guest_entry_count(body), expected_min)
                )
                return
        except (RuntimeError, AssertionError):
            pass
        time.sleep(POLL_INTERVAL)


def guest_park_count(body):
    """Extract the hypervisor's per-run count of vCPU park events.

    The counter increments once each time a vCPU actually observes the paused
    state and parks, so the probe must wait for it to advance after a pause
    before resuming: a resume sent while the vCPU is still running the guest is
    absorbed (the vCPU never parked, so it never re-enters either) and would
    make the resume re-entry evidence ambiguous. This counter *observes* a vCPU
    park — it is a VM-level aggregate (not per-vCPU) — while the `Paused`
    snapshot published by the owner is the actual completion signal.
    """
    if not isinstance(body, dict):
        raise AssertionError("VM detail response was not a JSON object")
    count = body.get("guest_park_count")
    if not isinstance(count, int):
        raise AssertionError(
            "VM detail response had no integer guest_park_count: %r" % (body,)
        )
    return count


def poll_guest_parks(vm_id, expected_min):
    """Poll `GET /api/vms/{id}` until `guest_park_count >= expected_min`.

    Confirms the vCPU actually observed the paused state and parked — not merely
    that the HTTP status flipped to `paused` — so the subsequent resume is a
    genuine wake from a parked vCPU. Fails on the poll deadline so a pause that
    never completes (or a status-only pause) makes the probe exit nonzero.
    """
    start = time.monotonic()
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] guest_park_count never reached >= %d within %.0fs"
                % (vm_id, expected_min, POLL_DEADLINE)
            )
        try:
            status, body = request("GET", "/api/vms/%d" % vm_id)
            if status == 200 and guest_park_count(body) >= expected_min:
                print(
                    "  http probe: VM[%d] -> guest_park_count %d (>= %d)"
                    % (vm_id, guest_park_count(body), expected_min)
                )
                return
        except (RuntimeError, AssertionError):
            pass
        time.sleep(POLL_INTERVAL)


def check_action(label, body, ok_expected, async_expected):
    """Assert a lifecycle action response's `ok` and `async` markers."""
    if not isinstance(body, dict):
        raise AssertionError("%s had no JSON body" % (label,))
    ok = body.get("ok")
    is_async = body.get("async")
    print(
        "  http probe: %s -> ok=%r async=%r (expect ok=%r async=%r)"
        % (label, ok, is_async, ok_expected, async_expected)
    )
    if ok != ok_expected:
        raise AssertionError(
            "%s reported ok=%r, expected %r" % (label, ok, ok_expected)
        )
    if is_async != async_expected:
        raise AssertionError(
            "%s reported async=%r, expected %r" % (label, is_async, async_expected)
        )


def check_frozen_counters(label, body, entries, parks):
    """Assert the paused VM's counters did not advance (no spurious progress)."""
    actual_entries = guest_entry_count(body)
    actual_parks = guest_park_count(body)
    print(
        "  http probe: %s -> entry=%d park=%d (expect %d/%d)"
        % (label, actual_entries, actual_parks, entries, parks)
    )
    if actual_parks != parks:
        raise AssertionError(
            "%s advanced guest_park_count %d -> %d while already paused"
            % (label, parks, actual_parks)
        )
    if actual_entries != entries:
        raise AssertionError(
            "%s advanced guest_entry_count %d -> %d while paused"
            % (label, entries, actual_entries)
        )


def list_has_vm(body, vm_id):
    """Whether a `GET /api/vms` body lists a VM with the given id."""
    return isinstance(body, list) and any(
        isinstance(item, dict) and item.get("id") == vm_id for item in body
    )


def poll_ready():
    """Poll `GET /api/vms` until it returns 200 or the connect deadline passes.

    The runner's TCP port wait proves the guest is listening, but the axum
    router may still be wiring up, so the first request is retried here.
    """
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
                return
        except RuntimeError:
            pass
        time.sleep(POLL_INTERVAL)


def poll_vm_status(vm_id, expected):
    """Poll `GET /api/vms/{id}` until its status equals `expected`."""
    start = time.monotonic()
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] never became %s within %.0fs" % (vm_id, expected, POLL_DEADLINE)
            )
        try:
            status, body = request("GET", "/api/vms/%d" % vm_id)
            if status == 200 and vm_status(body) == expected:
                print("  http probe: VM[%d] -> %s" % (vm_id, expected))
                return
        except (RuntimeError, AssertionError):
            # A non-200 or transport error during a transition (e.g. the VM is
            # being torn down) is transient; keep polling until the deadline.
            pass
        time.sleep(POLL_INTERVAL)


def poll_vm_gone(vm_id):
    """Poll `GET /api/vms/{id}` until it returns 404 (the VM was deleted)."""
    start = time.monotonic()
    while True:
        if time.monotonic() - start > POLL_DEADLINE:
            raise AssertionError(
                "VM[%d] never disappeared within %.0fs" % (vm_id, POLL_DEADLINE)
            )
        try:
            status, _ = request("GET", "/api/vms/%d" % vm_id)
            if status == 404:
                print("  http probe: VM[%d] -> gone" % vm_id)
                return
        except RuntimeError:
            pass
        time.sleep(POLL_INTERVAL)


def main():
    config_dir = os.environ.get("AXVISOR_BUILTIN_CONFIG_DIR", CASE_DIR)
    config_path = os.path.join(config_dir, "vm-1.toml")
    with open(config_path, "r", encoding="utf-8") as f:
        vm_config = f.read()
    if "/guest/builtin/images/" not in vm_config:
        raise AssertionError(
            "VM recreate must use installed built-in boot assets: %s" % config_path
        )
    kernel_match = re.search(
        r'(?m)^\s*kernel_path\s*=\s*"([^\"]+)"', vm_config
    )
    if kernel_match is None:
        raise AssertionError("the packaged VM config has no kernel_path: %s" % config_path)
    packaged_kernel_path = kernel_match.group(1)
    create_body = json.dumps({"toml": vm_config})
    bad_body = json.dumps({"toml": "this is not [[ valid toml {{{"})

    # 1. Readiness: the runner already waited for the TCP port; retry the first
    #    request briefly in case the axum router is still binding.
    poll_ready()
    print("  http probe: guest management server reachable")

    # 1b. Capability declaration: the unified `web` feature exposes the
    #     management API, file transfer, host facts, and both console panels.
    status, body = request("GET", "/api/manifest")
    check("GET /api/manifest", status, 200)
    check("GET /api/manifest proto", body.get("proto"), 1)
    panels = body.get("panels")
    if not isinstance(panels, list):
        raise AssertionError("GET /api/manifest panels was not a list: %r" % (body,))
    kinds = [panel.get("kind") for panel in panels]
    check("GET /api/manifest panel kinds", kinds, ["vms", "files", "host", "console", "shell"])
    check("GET /api/manifest vms verbs", panels[0].get("verbs"), ["read", "write"])
    if not panels[0].get("root"):
        raise AssertionError("GET /api/manifest vms panel had no root: %r" % (panels[0],))
    check_manifest_links(panels)
    check_file_transfer()
    check_create_form(packaged_kernel_path)
    status, body = request("GET", "/api/consoles")
    check("GET /api/consoles", status, 200)
    if not any(console.get("route") == "axvisor" for console in body):
        raise AssertionError("management console is missing: %r" % body)

    # 2. List: the default VM (id 1) is registered and `Ready`.
    status, body = request("GET", "/api/vms")
    check("GET /api/vms", status, 200)
    if not list_has_vm(body, 1):
        raise AssertionError("GET /api/vms did not list the default VM id=1")

    # 3. Detail of the default VM: identity, shape, and ready status.
    status, body = request("GET", "/api/vms/1")
    check("GET /api/vms/1", status, 200)
    check_vm_status("GET /api/vms/1", body, "ready")
    if body.get("id") != 1:
        raise AssertionError("GET /api/vms/1 did not report id=1")
    if body.get("name") != "linux-http-control-plane":
        raise AssertionError("GET /api/vms/1 did not report the fixture name")
    if body.get("cpu_num") != 1:
        raise AssertionError("GET /api/vms/1 did not report cpu_num=1")
    if not isinstance(body.get("vcpu_states"), list) or not body["vcpu_states"]:
        raise AssertionError("GET /api/vms/1 reported an empty vcpu_states array")
    # The re-execution and pause-park evidence fields must be present on the
    # Ready VM too (counts 0 until the first guest entry / first pause).
    if not isinstance(body.get("guest_entry_count"), int):
        raise AssertionError(
            "GET /api/vms/1 reported no integer guest_entry_count: %r" % (body,)
        )
    if not isinstance(body.get("guest_park_count"), int):
        raise AssertionError(
            "GET /api/vms/1 reported no integer guest_park_count: %r" % (body,)
        )

    # 4-5. Error path: non-numeric and unknown ids are 404.
    status, _ = request("GET", "/api/vms/not-an-id")
    check("GET /api/vms/not-an-id", status, 404)
    status, _ = request("GET", "/api/vms/999")
    check("GET /api/vms/999", status, 404)

    # 6. The first control-plane version has no authentication gate: a mutating
    #    route without an Authorization header reaches its handler and is judged
    #    by the route's own contract. An unknown id is 404, not 401.
    status, _ = request("DELETE", "/api/vms/999")
    check("DELETE /api/vms/999 (no auth header)", status, 404)

    # 7-8. Create validates its body: a missing `toml` and an invalid TOML
    #        document both reject with 400.
    status, _ = request("POST", "/api/vms/create", body="{}")
    check("POST /api/vms/create (missing toml)", status, 400)
    status, _ = request("POST", "/api/vms/create", body=bad_body)
    check("POST /api/vms/create (invalid toml)", status, 400)

    # 9-12. Writes to an unknown VM are 404.
    status, _ = request("POST", "/api/vms/999/start")
    check("POST /api/vms/999/start", status, 404)
    status, _ = request("POST", "/api/vms/999/stop")
    check("POST /api/vms/999/stop", status, 404)
    status, _ = request("POST", "/api/vms/999/pause")
    check("POST /api/vms/999/pause", status, 404)
    status, _ = request("POST", "/api/vms/999/resume")
    check("POST /api/vms/999/resume", status, 404)

    # 13. Duplicate create while id=1 is registered conflicts.
    status, _ = request("POST", "/api/vms/create", body=create_body)
    check("POST /api/vms/create (duplicate id=1)", status, 409)

    # 14-15. Pause/resume are only valid from Running/Paused respectively; a
    #        `Ready` (not started) VM rejects both with 409.
    status, _ = request("POST", "/api/vms/1/pause")
    check("POST /api/vms/1/pause (from Ready)", status, 409)
    status, _ = request("POST", "/api/vms/1/resume")
    check("POST /api/vms/1/resume (from Ready)", status, 409)

    # 21. Start the default VM: `start` awaits completion (`async=false`), then
    #     poll the detail into `running`.
    status, body = request(
        "POST", "/api/vms/1/start", timeout=COMPLETION_TIMEOUT
    )
    check("POST /api/vms/1/start", status, 200)
    check_action("POST /api/vms/1/start", body, True, False)
    poll_vm_status(1, "running")
    # The guest must have actually entered: the vCPU run loop increments
    # `guest_entry_count` on its first guest entry, independent of the status.
    poll_guest_entries(1, 1)
    status, body = request("GET", "/api/vms/1")
    # `guest_entry_count` advances on every guest exit while the VM runs, so the
    # re-entry baseline is read after the vCPU parks (in the pause blocks
    # below): only a resume that re-enters the guest moves the counter past the
    # frozen value, so a status-only resume fails the assertion.
    parks = guest_park_count(body)

    # 22. Re-starting an already-running VM conflicts: `start` is not an
    #     idempotent target transition.
    status, _ = request("POST", "/api/vms/1/start")
    check("POST /api/vms/1/start (already running)", status, 409)

    # 23. Resume while already `Running` is an idempotent no-op (200, sync).
    status, body = request("POST", "/api/vms/1/resume")
    check("POST /api/vms/1/resume (already running)", status, 200)
    check_action("POST /api/vms/1/resume (already running)", body, True, False)

    # 24. Pause waits for the owner (`async=false`): the handler returns after the owner
    #     accepts and completes it. The `Paused` snapshot then appears only after the owner
    #     parked every vCPU and quieted devices/ports/timers, so polling for
    #     `paused` is the real completion signal.
    status, body = request("POST", "/api/vms/1/pause")
    check("POST /api/vms/1/pause", status, 200)
    check_action("POST /api/vms/1/pause", body, True, False)
    poll_vm_status(1, "paused")
    # Park evidence: wait until a vCPU has actually observed the paused state.
    poll_guest_parks(1, parks + 1)
    parks = parks + 1
    # Sample the re-entry counter now that the vCPU has parked and the value is
    # frozen. The resume assertion requires the counter to advance past this
    # baseline, so a status-only resume (no re-entry) makes the poll time out.
    status, body = request("GET", "/api/vms/1")
    check_vm_status("GET /api/vms/1 (paused)", body, "paused")
    entries = guest_entry_count(body)

    # 25. Pausing an already-paused VM is an idempotent no-op (200): it must not
    #     request a duplicate park, and the parked VM's counters must stay
    #     frozen.
    status, body = request("POST", "/api/vms/1/pause")
    check("POST /api/vms/1/pause (already paused)", status, 200)
    check_action("POST /api/vms/1/pause (already paused)", body, True, False)
    status, body = request("GET", "/api/vms/1")
    check_vm_status("GET /api/vms/1 (still paused)", body, "paused")
    check_frozen_counters("POST /api/vms/1/pause (already paused)", body, entries, parks)

    # 26. Resume is synchronous (`async=false`): the handler awaits completion,
    #     the status flips back to `Running`, and the parked vCPU is woken to
    #     re-enter the guest.
    status, body = request(
        "POST", "/api/vms/1/resume", timeout=COMPLETION_TIMEOUT
    )
    check("POST /api/vms/1/resume", status, 200)
    check_action("POST /api/vms/1/resume", body, True, False)
    poll_vm_status(1, "running")
    # Genuine wake: the parked vCPU re-entered the guest, advancing
    # `guest_entry_count`. A status flip without re-entry would not advance it
    # and make this poll hit its deadline.
    poll_guest_entries(1, entries + 1)
    entries = entries + 1

    # 27. Resuming an already-running VM is an idempotent no-op (200).
    status, body = request("POST", "/api/vms/1/resume")
    check("POST /api/vms/1/resume (already running)", status, 200)
    check_action("POST /api/vms/1/resume (already running)", body, True, False)

    # 28-29. Second suspend/wake cycle: a parked vCPU is woken and re-parked
    #        repeatedly, so the wake path must converge every time.
    status, body = request("POST", "/api/vms/1/pause")
    check("POST /api/vms/1/pause (cycle 2)", status, 200)
    check_action("POST /api/vms/1/pause (cycle 2)", body, True, False)
    poll_vm_status(1, "paused")
    # Pause-completion on the second cycle too: wait for the vCPU to actually
    # park before resuming.
    poll_guest_parks(1, parks + 1)
    parks = parks + 1
    # Re-sample the frozen re-entry baseline for the second cycle.
    status, body = request("GET", "/api/vms/1")
    check_vm_status("GET /api/vms/1 (paused cycle 2)", body, "paused")
    entries = guest_entry_count(body)
    status, body = request(
        "POST", "/api/vms/1/resume", timeout=COMPLETION_TIMEOUT
    )
    check("POST /api/vms/1/resume (cycle 2)", status, 200)
    check_action("POST /api/vms/1/resume (cycle 2)", body, True, False)
    poll_vm_status(1, "running")
    # The wake path must converge every cycle: re-entry advances the count.
    poll_guest_entries(1, entries + 1)
    entries = entries + 1

    # 30. Stop waits for the owner (`async=false`): the `stopped` state arrives only
    #     after the owner has fully torn the run down (vCPUs joined, devices and
    #     memory retired), which drops the per-run counters back to zero.
    status, body = request("POST", "/api/vms/1/stop")
    check("POST /api/vms/1/stop", status, 200)
    check_action("POST /api/vms/1/stop", body, True, False)
    poll_vm_status(1, "stopped")
    status, body = request("GET", "/api/vms/1")
    check_vm_status("GET /api/vms/1 (stopped)", body, "stopped")
    if guest_entry_count(body) != 0 or guest_park_count(body) != 0:
        raise AssertionError(
            "stopped VM did not publish the fresh per-run zero counters: %r" % (body,)
        )
    print("  http probe: VM[1] stopped -> per-run counters reset to 0/0")

    # 31. Stopping an already-stopped VM is an idempotent success (200) and must
    #     not resurrect any run resource.
    status, body = request("POST", "/api/vms/1/stop")
    check("POST /api/vms/1/stop (already stopped)", status, 200)
    check_action("POST /api/vms/1/stop (already stopped)", body, True, False)
    status, body = request("GET", "/api/vms/1")
    check_vm_status("GET /api/vms/1 (still stopped)", body, "stopped")
    if guest_entry_count(body) != 0 or guest_park_count(body) != 0:
        raise AssertionError(
            "repeated stop resurrected run counters: %r" % (body,)
        )

    # 32. Restart from Stopped is a supported transition (the old scheduling
    #     limitation is gone): the fresh run must actually enter the guest
    #     before it is stopped again. The counters reset to zero at stop, so a
    #     fresh entry is itself the restart's success evidence.
    status, body = request(
        "POST", "/api/vms/1/start", timeout=COMPLETION_TIMEOUT
    )
    check("POST /api/vms/1/start (from Stopped)", status, 200)
    check_action("POST /api/vms/1/start (from Stopped)", body, True, False)
    poll_vm_status(1, "running")
    poll_guest_entries(1, 1)

    # 33. The restarted run stops again cleanly.
    status, body = request("POST", "/api/vms/1/stop")
    check("POST /api/vms/1/stop (restarted run)", status, 200)
    check_action("POST /api/vms/1/stop (restarted run)", body, True, False)
    poll_vm_status(1, "stopped")

    # 34. Delete the stopped VM, then poll until it is gone.
    status, _ = request("DELETE", "/api/vms/1", timeout=COMPLETION_TIMEOUT)
    check("DELETE /api/vms/1", status, 204)
    poll_vm_gone(1)

    # 35. Recreate after delete: the embedded image is matched by id, so a
    #     fresh create with the same config succeeds and registers id 1 again.
    status, body = request(
        "POST",
        "/api/vms/create",
        body=create_body,
        timeout=COMPLETION_TIMEOUT,
    )
    check("POST /api/vms/create (recreate)", status, 200)
    if not isinstance(body, dict) or body.get("id") != 1:
        raise AssertionError("recreate did not return id=1")
    poll_vm_status(1, "ready")

    # 36. The re-registered id conflicts with a second create.
    status, _ = request("POST", "/api/vms/create", body=create_body)
    check("POST /api/vms/create (recreate duplicate)", status, 409)

    # 37-38. The recreated VM must be fully usable, not merely re-registered:
    #        destroy must have freed guest memory, vCPUs, devices, and the
    #        registry entry so a fresh VM can be rebuilt and run from the same
    #        embedded image. This is the resource re-acquire regression.
    status, _ = request(
        "POST", "/api/vms/1/start", timeout=COMPLETION_TIMEOUT
    )
    check("POST /api/vms/1/start (recreated)", status, 200)
    poll_vm_status(1, "running")
    # The recreated runtime's vCPU must actually enter the guest, proving the
    # fresh build is runnable (not just registered as `Running`).
    poll_guest_entries(1, 1)
    status, _ = request("POST", "/api/vms/1/stop")
    check("POST /api/vms/1/stop (recreated)", status, 200)
    poll_vm_status(1, "stopped")

    # 39. Cleanup: leave the hypervisor without a registered VM.
    status, _ = request("DELETE", "/api/vms/1", timeout=COMPLETION_TIMEOUT)
    check("DELETE /api/vms/1 (cleanup)", status, 204)
    poll_vm_gone(1)

    print("  http probe: full control-plane contract passed")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as exc:
        print("  http probe: FAILED: %s" % exc, file=sys.stderr)
        sys.exit(1)
    except Exception as exc:
        print("  http probe: ERROR: %s" % exc, file=sys.stderr)
        sys.exit(2)
