#!/usr/bin/env python3
"""Boot frozen Linux RT with the diagnostic forward-window PMU benchmark."""

import hashlib
import json
import re
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, "/tmp/issue2308-pydeps")
import websocket

ROOT = Path(__file__).resolve().parent
LINUX = Path("/tmp/pr1775-orangepi/linux-rt/arch/arm64/boot/Image")
DTB = Path("/tmp/pr1775-orangepi/linux-rt/arch/arm64/boot/dts/rockchip/rk3588-orangepi-5-plus.dtb")
INITRAMFS = ROOT / "initramfs-linux.cpio"
API = "http://10.3.10.194:2999"
BOARD = "OrangePi-5-Plus-1"
LINUX_SHA = "aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b"
BENCH_SHA = "7ea6ace6192fb9ffafc2cd0ce13c2c5cc97ceb7eb6808e282c0a417e7ffa82b6"


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def request(path, method="GET", data=None, headers=None):
    if isinstance(data, dict):
        data = json.dumps(data).encode()
        headers = {**(headers or {}), "Content-Type": "application/json"}
    req = urllib.request.Request(API + path, data=data, headers=headers or {}, method=method)
    with urllib.request.urlopen(req, timeout=90) as response:
        return response.read()


def main():
    assert not (ROOT / "linux-status.json").exists(), "diagnostic already started"
    assert sha(LINUX) == LINUX_SHA
    assert sha(ROOT / "bench-window-pmu") == BENCH_SHA
    assert INITRAMFS.stat().st_size > 1_000_000
    lease_start = time.monotonic()
    while True:
        try:
            session = json.loads(request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus",
                "board_id": BOARD,
                "required_tags": [],
                "client_name": "issue2308-resume810-window-linux",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - lease_start > 360:
                raise
            time.sleep(2)

    sid = session["session_id"]
    status = {
        "experiment": "resume810-linux",
        "board_id": session["board_id"],
        "session_id": sid,
        "linux_image_sha256": LINUX_SHA,
        "dtb_sha256": sha(DTB),
        "initramfs_sha256": sha(INITRAMFS),
        "init_source_sha256": sha(ROOT / "init-linux.c"),
        "benchmark_sha256": BENCH_SHA,
        "bootargs": "console=ttyS2,1500000 earlycon=uart8250,mmio32,0xfeb50000 cpuidle.off=1 nokaslr",
        "diagnostic_only": True,
        "board_released": False,
    }
    ws = None
    serial = None
    stop = threading.Event()
    try:
        assert session["board_id"] == BOARD
        (ROOT / "linux-status.json").write_text(json.dumps(status, indent=2) + "\n")
        for name, path in (("linux-rt-Image", LINUX), ("linux-rt.dtb", DTB),
                           ("initramfs.cpio", INITRAMFS)):
            request(f"/api/v1/sessions/{sid}/files", "PUT", path.read_bytes(),
                    {"X-File-Path": name})

        def heartbeat():
            while not stop.wait(3):
                try:
                    request(f"/api/v1/sessions/{sid}/heartbeat", "POST", b"")
                except Exception as error:
                    status["heartbeat_error"] = repr(error)
                    stop.set()

        threading.Thread(target=heartbeat, daemon=True).start()
        ws = websocket.create_connection(API.replace("http:", "ws:") + session["ws_url"], timeout=1)
        serial = (ROOT / "linux-serial.log").open("xb", buffering=0)
        buffer = bytearray()
        interrupted = False

        def wait_for(pattern, seconds):
            nonlocal interrupted
            regex = re.compile(pattern, re.S)
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                match = regex.search(buffer)
                if match:
                    result = bytes(buffer[:match.end()])
                    del buffer[:match.end()]
                    return result
                if stop.is_set():
                    raise RuntimeError("board session heartbeat stopped")
                try:
                    data = ws.recv()
                except websocket.WebSocketTimeoutException:
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
        status["pll_verified"] = True
        initrd_size = INITRAMFS.stat().st_size
        send(f"pci enum; setenv autoload no; dhcp; setenv serverip 192.168.1.2; "
             f"tftpboot 0x02000000 ostool/sessions/{sid}/linux-rt-Image; "
             f"tftpboot 0x12000000 ostool/sessions/{sid}/linux-rt.dtb; "
             f"tftpboot 0x14000000 ostool/sessions/{sid}/initramfs.cpio; "
             f"setenv bootargs '{status['bootargs']}'; "
             f"booti 0x02000000 0x14000000:0x{initrd_size:x} 0x12000000")
        boot = wait_for(rb"RESUME810_LINUX_DONE failures=\d+", 360)
        (ROOT / "linux-boot.log").write_bytes(boot)
        assert b"Linux version" in boot and b"PREEMPT_RT" in boot
        assert b"Starting kernel" in boot
        status["failures"] = int(re.search(rb"RESUME810_LINUX_DONE failures=(\d+)", boot).group(1))
        status["boot_sha256"] = sha(ROOT / "linux-boot.log")
        status["state"] = "collected"
        print("RESUME810_LINUX_COLLECTED", status["failures"], flush=True)
    except Exception as error:
        status["state"] = "stopped_with_error"
        status["error"] = repr(error)
        raise
    finally:
        stop.set()
        if serial is not None:
            serial.close()
        if ws is not None:
            ws.close()
        try:
            request(f"/api/v1/sessions/{sid}", "DELETE")
            status["board_released"] = True
            print("BOARD_RELEASED", sid, flush=True)
        finally:
            (ROOT / "linux-status.json").write_text(json.dumps(status, indent=2) + "\n")


if __name__ == "__main__":
    main()
