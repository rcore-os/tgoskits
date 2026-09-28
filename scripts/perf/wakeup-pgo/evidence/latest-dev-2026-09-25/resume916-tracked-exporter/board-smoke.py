#!/usr/bin/env python3
"""Verify the tracked counter exporter on the fixed Plus-1 board."""

import importlib.util
import json
import re
import threading
import time
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "board-smoke-3"
IMAGE = ROOT / "generate.bin"
BOARD = "OrangePi-5-Plus-1"
IMAGE_SHA = "979a60e788523e930500bbc3a93c30cf360b9aa7197c315f9c601b6292aaa975"
COUNTER_BYTES = 790272
TEMPLATE = ROOT.parent / "resume720-other-epilogue" / "board.py"
spec = importlib.util.spec_from_file_location("issue2308_board", TEMPLATE)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def main():
    assert not RUN.exists(), "preserve prior evidence"
    assert helper.sha(IMAGE) == IMAGE_SHA
    RUN.mkdir()
    lease_start = time.monotonic()
    while True:
        try:
            session = json.loads(helper.request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus", "board_id": BOARD,
                "required_tags": [], "client_name": "issue2308-resume916-exporter-smoke",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - lease_start > 180:
                raise
            time.sleep(2)

    sid = session["session_id"]
    ws = None
    serial = None
    stop = threading.Event()
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(helper.request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        assert dtb_path == f"ostool/sessions/{sid}/boot/dtb/orangepi-5-plus.dtb"
        helper.request(f"/api/v1/sessions/{sid}/files", "PUT", IMAGE.read_bytes(),
                       {"X-File-Path": "generate.bin"})

        def heartbeat():
            while not stop.wait(3):
                try:
                    helper.request(f"/api/v1/sessions/{sid}/heartbeat", "POST", b"")
                except Exception as error:
                    print("HEARTBEAT_ERROR", repr(error), flush=True)
                    stop.set()

        threading.Thread(target=heartbeat, daemon=True).start()
        ws = helper.websocket.create_connection(
            helper.API.replace("http:", "ws:") + session["ws_url"], timeout=1)
        serial = (RUN / "serial.log").open("xb", buffering=0)
        buffer = bytearray()
        interrupted = False

        def wait_for(pattern, seconds):
            nonlocal interrupted
            deadline = time.monotonic() + seconds
            regex = re.compile(pattern, re.S)
            while time.monotonic() < deadline:
                match = regex.search(buffer)
                if match:
                    matched = bytes(buffer[:match.end()])
                    del buffer[:match.end()]
                    return matched
                if stop.is_set():
                    raise RuntimeError("heartbeat stopped")
                try:
                    data = ws.recv()
                except helper.websocket.WebSocketTimeoutException:
                    continue
                if isinstance(data, str):
                    data = data.encode()
                if not data:
                    raise RuntimeError("serial closed")
                serial.write(data)
                buffer.extend(data)
                if not interrupted and b"Hit any key to stop autoboot" in buffer:
                    ws.send_binary(b" ")
                    interrupted = True
            raise TimeoutError(pattern)

        def send(command):
            ws.send_binary((command + "\n").encode())

        wait_for(rb"=> ", 180)
        send("md.l fd818040 3; md.l fd818280 1; md.l fd818314 3")
        pll = wait_for(rb"fd818314: 0000803f 00000000 00000000.*?=> ", 30)
        assert b"fd818040: 00000110 00000082 00000000" in pll
        assert b"fd818280: 00000001" in pll
        send(f"pci enum; setenv autoload no; dhcp; setenv serverip 192.168.1.2; "
             f"tftpboot 0x02000000 ostool/sessions/{sid}/generate.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == IMAGE.stat().st_size
        send("mkdir -p /sys/kernel/debug; mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true; "
             "printf 'RESUME916_COUNTER_BYTES '; wc -c < /sys/kernel/debug/profile_counters; "
             "echo RESUME916_SMOKE_DONE")
        result = wait_for(rb"(?m)^RESUME916_SMOKE_DONE\r?$", 120)
        assert b"RESUME916_COUNTER_BYTES" in result
        match = re.search(rb"(?m)^(\d+)\r?$", result)
        assert match and int(match.group(1)) == COUNTER_BYTES, result[-1000:]
        (RUN / "result.json").write_text(json.dumps({
            "diagnostic_only": True, "board_id": BOARD, "session_id": sid,
            "image_sha256": IMAGE_SHA, "counter_bytes": int(match.group(1)),
        }, indent=2) + "\n")
        print("RESUME916_SMOKE_PASSED", match.group(1).decode(), flush=True)
    finally:
        stop.set()
        if serial is not None:
            serial.close()
        if ws is not None:
            ws.close()
        helper.request(f"/api/v1/sessions/{sid}", "DELETE")
        print("BOARD_RELEASED", sid, flush=True)


if __name__ == "__main__":
    main()
