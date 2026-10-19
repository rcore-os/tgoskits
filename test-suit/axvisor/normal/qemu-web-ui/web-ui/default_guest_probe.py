#!/usr/bin/env python3
"""Exercise a guest created from the dashboard schema without uploaded assets.

This checks the HTTP/terminal contract on real AxVisor. Rendered browser
interaction is a separate acceptance check; these probes do not emulate it.
"""

import json
import time

import http_probe as probe


def create_defaults(omit_id=False):
    status, defaults = probe.get("/api/vms/defaults", "creation defaults")
    probe.expect_status("creation defaults", status, 200)
    fields = defaults["fields"]
    if omit_id:
        fields.pop("id", None)
    status, created = probe.request("POST", "/api/vms/create", json.dumps({"fields": fields}))
    if status != 200:
        raise AssertionError("creating schema defaults returned %s: %r" % (status, created))
    vm_id = created.get("id")
    if not isinstance(vm_id, int):
        raise AssertionError("auto-assignment returned no guest id: %r" % created)
    print("  web-ui probe: assigned guest id -> %d" % vm_id)
    return vm_id, fields


def run_guest(vm_id):
    route = "vm-%d" % vm_id
    guest = probe.expect_upgrade("/ws/" + route, 101)
    try:
        probe.receive_until(guest, ("browser console attached to VM %d" % vm_id).encode())
        deadline = time.monotonic() + probe.GUEST_BOOT_DEADLINE
        status, body = probe.request("POST", "/api/vms/%d/start" % vm_id)
        probe.expect_status("start default guest", status, 200)
        probe.check("start completed", body["ok"], True)
        # A result absent from the command itself proves actual shell execution.
        # Send after the getty starts; its prompt may include an Alpine hostname.
        probe.receive_output_until(guest, b"# ", deadline, "default guest shell")
        guest.send_binary(probe.GUEST_INPUT_COMMAND)
        probe.receive_output_until(guest, probe.GUEST_INPUT_RESULT, deadline, "default guest arithmetic")
        guest.send_binary(b"cat /etc/alpine-release\r")
        probe.receive_output_until(guest, b"3.23.6", deadline, "existing Alpine root filesystem")
        detail = probe.vm_detail(vm_id, "running default guest")
        probe.check("guest state", detail["status"], "running")
        if detail.get("guest_entry_count", 0) < 1:
            raise AssertionError("the default guest never entered")
        duplicate, response = probe.open_websocket("/ws/" + route)
        duplicate.close()
        if not response.startswith(b"HTTP/1.1 409"):
            raise AssertionError("an occupied guest lane accepted a second owner: %r" % response)
    finally:
        guest.close()
    probe.poll_lane_attached("released guest lane", route, False, time.monotonic() + 10)
    # Reconnection must support input without creating a new VM or losing its state.
    guest = probe.expect_upgrade("/ws/" + route, 101)
    try:
        probe.receive_until(guest, ("browser console attached to VM %d" % vm_id).encode())
        guest.send_binary(probe.GUEST_ISOLATION_COMMAND)
        probe.receive_output_until(
            guest, probe.GUEST_ISOLATION_RESULT, time.monotonic() + 10, "reconnected guest arithmetic"
        )
    finally:
        guest.close()


def remove_guest(vm_id):
    status, _ = probe.request("POST", "/api/vms/%d/stop" % vm_id)
    probe.expect_status("stop default guest", status, 200)
    probe.check("stopped guest state", probe.vm_detail(vm_id, "stopped guest")["status"], "stopped")
    status, _ = probe.request("DELETE", "/api/vms/%d" % vm_id)
    probe.expect_status("delete default guest", status, 204)
    probe.poll_consoles_for("deleted guest lane", ["axvisor"], time.monotonic() + 10)


def main():
    probe.poll_ready()
    probe.check_dashboard()
    status, guests = probe.get("/api/vms", "initial guest registry")
    probe.expect_status("initial guest registry", status, 200)
    probe.check("initial guest registry", guests, [])
    vm_id, fields = create_defaults()
    second_id, _ = create_defaults(omit_id=True)
    if second_id == vm_id:
        raise AssertionError("auto-assignment reused an occupied id")
    status, _ = probe.request("DELETE", "/api/vms/%d" % second_id)
    probe.expect_status("delete unused second guest", status, 204)
    # A stale page or double click must not create a second owner of the same VM.
    status, _ = probe.request("POST", "/api/vms/create", json.dumps({"fields": dict(fields, id=vm_id)}))
    probe.expect_status("duplicate guest rejected", status, 409)
    run_guest(vm_id)
    remove_guest(vm_id)
    # Reusing the released ID must produce a working guest and a new console lane.
    recreated, _ = create_defaults()
    probe.check("released id reused", recreated, vm_id)
    run_guest(recreated)
    remove_guest(recreated)
    print("AXVISOR_DEFAULT_GUEST_WEB_UI_PASSED", flush=True)


if __name__ == "__main__":
    main()
