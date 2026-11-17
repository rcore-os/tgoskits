#!/usr/bin/env python3
"""Restore an empty runtime registry without changing builtin candidates."""
import json
import os
import time
import urllib.error
import urllib.request
BASE = os.environ["AXVISOR_HTTP_BASE"].rstrip("/")
TIMEOUT = float(os.environ.get("AXVISOR_HTTP_REQUEST_TIMEOUT", "60"))
def request(method, path):
    with urllib.request.urlopen(urllib.request.Request(BASE + path, method=method), timeout=TIMEOUT) as response:
        body = response.read()
        return response.status, json.loads(body) if body else None
deadline = time.monotonic() + float(os.environ.get("AXVISOR_HTTP_CONNECT_TIMEOUT", "120"))
while True:
    try:
        status, guests = request("GET", "/api/vms")
        assert status == 200 and isinstance(guests, list), (status, guests)
        break
    except (OSError, urllib.error.URLError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.1)
for guest in guests:
    status, _ = request("DELETE", "/api/vms/%d" % guest["id"])
    assert status == 204, (guest, status)
status, remaining = request("GET", "/api/vms")
assert status == 200 and remaining == [], remaining
print("Runtime guest registry restored to empty", flush=True)
