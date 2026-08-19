#!/usr/bin/env python3
"""Host-side probe asset for the AxVisor management HTTP control plane.

Case asset for the `http-control-plane` test case
(`test-suit/axvisor/normal/qemu-http-control-plane/`). It owns the *test
content* — the concrete requests, the `vm-memory.toml` fixture, and the
assertions — and can evolve independently of the axbuild runner.

The generic axbuild probe runner
(`scripts/axbuild/src/axvisor/test/http_probe.rs`) executes this script after
the QEMU hostfwd port is reachable, then treats the exit code as the verdict:
0 = all assertions passed, nonzero = a step failed. The script dials the axum
management API running *inside* the AxVisor guest through QEMU user-mode
networking hostfwd. Nothing in the hypervisor knows a test is running.

Environment (set by the generic runner):

    AXVISOR_HTTP_BASE            http://127.0.0.1:<host_port> (forwarded)
    AXVISOR_HTTP_TOKEN           bearer token for authenticated requests
    AXVISOR_HTTP_CASE_DIR        case directory holding `vm-memory.toml`
                                 (default: this file's directory)
    AXVISOR_HTTP_CONNECT_TIMEOUT seconds for the initial reachability wait
    AXVISOR_HTTP_REQUEST_TIMEOUT seconds per HTTP request

The probe drives the whole `/api/vms` lifecycle contract in one boot,
including the destroy-then-recreate resource re-acquire regression, mirroring
`os/axvisor/doc/http-control-plane-quickstart.md`:

    GET    /api/vms            -> 200            (list; id=1 present)
    GET    /api/vms/1          -> 200 ready      (detail; id/name/cpu_num/vcpu_states)
    GET    /api/vms/not-an-id  -> 404            (non-numeric id)
    GET    /api/vms/999        -> 404            (unknown VM)
    POST   /api/vms/create     -> 401            (no token)
    POST   /api/vms/1/start    -> 401            (no token)
    POST   /api/vms/1/stop     -> 401            (no token)
    DELETE /api/vms/1          -> 401            (no token)
    POST   /api/vms/create {}  -> 400            (missing toml)
    POST   /api/vms/create <bad toml> -> 400     (invalid TOML)
    POST   /api/vms/999/start  -> 404            (auth'd unknown VM)
    POST   /api/vms/999/stop   -> 404            (auth'd unknown VM)
    DELETE /api/vms/999        -> 404            (auth'd unknown VM)
    POST   /api/vms/create     -> 409            (id=1 already registered)
    POST   /api/vms/1/start    -> 200 -> running (async=false)
    POST   /api/vms/1/start    -> 409            (already running)
    POST   /api/vms/1/stop     -> 200 -> stopped (async=true)
    POST   /api/vms/1/start    -> 409            (restart-after-stop)
    DELETE /api/vms/1          -> 204 -> 404     (gone)
    POST   /api/vms/create     -> 200 {id:1}     (recreate after delete)
    POST   /api/vms/create     -> 409            (id=1 re-registered)
    POST   /api/vms/1/start    -> 200 -> running (recreated VM usable)
    POST   /api/vms/1/stop     -> 200 -> stopped
    DELETE /api/vms/1          -> 204 -> 404     (cleanup)

The last recreate -> start -> stop -> delete block is the resource re-acquire
regression: it proves destroy freed guest memory, vCPUs, devices, and the
registry entry so a fresh VM can be rebuilt from the same embedded image.
`vm-memory.toml` is matched by `base.id` against the build-time embedded
images, so the create body carries that file verbatim (the `kernel_path` /
`ramdisk_path` `${workspace}` placeholders are unused at runtime for memory
images).
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("AXVISOR_HTTP_BASE", "http://127.0.0.1:8080").rstrip("/")
TOKEN = os.environ.get("AXVISOR_HTTP_TOKEN", "")
CASE_DIR = os.environ.get(
    "AXVISOR_HTTP_CASE_DIR", os.path.dirname(os.path.abspath(__file__))
)
CONNECT_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "5"))
# Deadline for VM state transitions (boot, stop, delete): must stay well below
# the case `timeout` (600s) so a stuck transition fails on the probe, not on
# the QEMU timeout.
POLL_DEADLINE = 120.0
POLL_INTERVAL = 1.0


def request(method, path, token=None, body=None):
    """One HTTP request; returns (status, parsed JSON or None).

    `token` defaults to `None`: the unauthenticated steps assert the 401
    rejections, and the poll loops mirror the runner's no-token GETs. The
    authenticated steps pass `token=TOKEN` explicitly.

    A JSON `body` is sent with `Content-Type: application/json`. A non-2xx
    response is not an error here — the caller asserts the status. A transport
    error (connection refused/reset/timeout while the guest server is coming up
    or mid-transition) raises RuntimeError for the caller to retry or fail.
    """
    headers = {}
    if token:
        headers["Authorization"] = "Bearer " + token
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
    with open(os.path.join(CASE_DIR, "vm-memory.toml"), "r", encoding="utf-8") as f:
        vm_config = f.read()
    create_body = json.dumps({"toml": vm_config})
    bad_body = json.dumps({"toml": "this is not [[ valid toml {{{"})

    # 1. Readiness: the runner already waited for the TCP port; retry the first
    #    request briefly in case the axum router is still binding.
    poll_ready()
    print("  http probe: guest management server reachable")

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

    # 4-5. Error path: non-numeric and unknown ids are 404.
    status, _ = request("GET", "/api/vms/not-an-id")
    check("GET /api/vms/not-an-id", status, 404)
    status, _ = request("GET", "/api/vms/999")
    check("GET /api/vms/999", status, 404)

    # 6-9. Auth: every mutating route rejects an unauthenticated write with
    #       401, before any VM lookup or body parse.
    status, _ = request("POST", "/api/vms/create")
    check("POST /api/vms/create (no auth)", status, 401)
    status, _ = request("POST", "/api/vms/1/start")
    check("POST /api/vms/1/start (no auth)", status, 401)
    status, _ = request("POST", "/api/vms/1/stop")
    check("POST /api/vms/1/stop (no auth)", status, 401)
    status, _ = request("DELETE", "/api/vms/1")
    check("DELETE /api/vms/1 (no auth)", status, 401)

    # 10-11. Create validates its body: a missing `toml` and an invalid TOML
    #        document both reject with 400.
    status, _ = request("POST", "/api/vms/create", token=TOKEN, body="{}")
    check("POST /api/vms/create (missing toml)", status, 400)
    status, _ = request("POST", "/api/vms/create", token=TOKEN, body=bad_body)
    check("POST /api/vms/create (invalid toml)", status, 400)

    # 12-14. Authenticated writes to an unknown VM are 404.
    status, _ = request("POST", "/api/vms/999/start", token=TOKEN)
    check("POST /api/vms/999/start (auth'd)", status, 404)
    status, _ = request("POST", "/api/vms/999/stop", token=TOKEN)
    check("POST /api/vms/999/stop (auth'd)", status, 404)
    status, _ = request("DELETE", "/api/vms/999", token=TOKEN)
    check("DELETE /api/vms/999 (auth'd)", status, 404)

    # 15. Duplicate create while id=1 is registered conflicts.
    status, _ = request("POST", "/api/vms/create", token=TOKEN, body=create_body)
    check("POST /api/vms/create (duplicate id=1)", status, 409)

    # 16. Start the default VM: accepted synchronously (`async=false`), then
    #     poll the detail into `running`.
    status, body = request("POST", "/api/vms/1/start", token=TOKEN)
    check("POST /api/vms/1/start", status, 200)
    check_action("POST /api/vms/1/start", body, True, False)
    poll_vm_status(1, "running")

    # 17. Re-starting an already-running VM conflicts.
    status, _ = request("POST", "/api/vms/1/start", token=TOKEN)
    check("POST /api/vms/1/start (already running)", status, 409)

    # 18. Stop is a request (`async=true`): the `stopped` state arrives
    #     asynchronously once the vCPU observes it and exits.
    status, body = request("POST", "/api/vms/1/stop", token=TOKEN)
    check("POST /api/vms/1/stop", status, 200)
    check_action("POST /api/vms/1/stop", body, True, True)
    poll_vm_status(1, "stopped")

    # 19. Restart-after-stop is a known scheduling limitation; the contract
    #     rejects it with 409 rather than hanging the VM in `running`.
    status, _ = request("POST", "/api/vms/1/start", token=TOKEN)
    check("POST /api/vms/1/start (restart-after-stop)", status, 409)

    # 20. Delete the stopped VM, then poll until it is gone.
    status, _ = request("DELETE", "/api/vms/1", token=TOKEN)
    check("DELETE /api/vms/1", status, 204)
    poll_vm_gone(1)

    # 21. Recreate after delete: the embedded image is matched by id, so a
    #     fresh create with the same config succeeds and registers id 1 again.
    status, body = request("POST", "/api/vms/create", token=TOKEN, body=create_body)
    check("POST /api/vms/create (recreate)", status, 200)
    if not isinstance(body, dict) or body.get("id") != 1:
        raise AssertionError("recreate did not return id=1")
    poll_vm_status(1, "ready")

    # 22. The re-registered id conflicts with a second create.
    status, _ = request("POST", "/api/vms/create", token=TOKEN, body=create_body)
    check("POST /api/vms/create (recreate duplicate)", status, 409)

    # 23-24. The recreated VM must be fully usable, not merely re-registered:
    #        destroy must have freed guest memory, vCPUs, devices, and the
    #        registry entry so a fresh VM can be rebuilt and run from the same
    #        embedded image. This is the resource re-acquire regression.
    status, _ = request("POST", "/api/vms/1/start", token=TOKEN)
    check("POST /api/vms/1/start (recreated)", status, 200)
    poll_vm_status(1, "running")
    status, _ = request("POST", "/api/vms/1/stop", token=TOKEN)
    check("POST /api/vms/1/stop (recreated)", status, 200)
    poll_vm_status(1, "stopped")

    # 25. Cleanup: leave the hypervisor without a registered VM.
    status, _ = request("DELETE", "/api/vms/1", token=TOKEN)
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
