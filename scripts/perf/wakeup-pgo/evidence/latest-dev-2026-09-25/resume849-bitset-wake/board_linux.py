#!/usr/bin/env python3
"""Run the same three-arm bitset binary on frozen Linux RT."""

import hashlib
import importlib.util
import json
import re
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "linux-run1"
LINUX = Path("/tmp/pr1775-orangepi/linux-rt/arch/arm64/boot/Image")
DTB = Path("/tmp/pr1775-orangepi/linux-rt/arch/arm64/boot/dts/rockchip/rk3588-orangepi-5-plus.dtb")
INITRAMFS = ROOT / "initramfs.cpio"
BENCH = ROOT / "wake_cost.aarch64"
LINUX_SHA = "aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b"
DTB_SHA = "316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994"
INITRAMFS_SHA = "44cdf310cb527e5d78d91fa468d5b4ad74ac2828f21b13524efe55370f1d3345"
BENCH_SHA = "cb129fc2b48a8af7a7d9c845161bab1f7858750aed70ae9fee6dc83bcab15326"
BOARD = "OrangePi-5-Plus-2"
BOOTARGS = "console=ttyS2,1500000 earlycon=uart8250,mmio32,0xfeb50000 cpuidle.off=1 nokaslr"
HELPER = ROOT.parent / "resume720-other-epilogue" / "board.py"
spec = importlib.util.spec_from_file_location("board_helper", HELPER)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    assert not RUN.exists()
    assert sha(LINUX) == LINUX_SHA and sha(DTB) == DTB_SHA
    assert sha(INITRAMFS) == INITRAMFS_SHA and sha(BENCH) == BENCH_SHA
    RUN.mkdir()
    session = json.loads(helper.request("/api/v1/sessions", "POST", {
        "board_type": "OrangePi-5-Plus", "board_id": BOARD,
        "required_tags": [], "client_name": "issue2308-resume849-linux-rt",
    }))
    sid = session["session_id"]
    status = {"experiment": "resume849", "diagnostic_only": True,
              "board_id": BOARD, "session_id": sid, "linux_image_sha256": LINUX_SHA,
              "dtb_sha256": DTB_SHA, "initramfs_sha256": INITRAMFS_SHA,
              "benchmark_sha256": BENCH_SHA, "bootargs": BOOTARGS,
              "board_released": False}
    ws = None
    serial = None
    stop = threading.Event()
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        for name, path in (("linux-rt-Image", LINUX), ("linux-rt.dtb", DTB),
                           ("initramfs.cpio", INITRAMFS)):
            helper.request(f"/api/v1/sessions/{sid}/files", "PUT", path.read_bytes(),
                           {"X-File-Path": name})

        def heartbeat():
            while not stop.wait(3):
                try:
                    helper.request(f"/api/v1/sessions/{sid}/heartbeat", "POST", b"")
                except Exception as error:
                    status["heartbeat_error"] = repr(error)
                    stop.set()

        threading.Thread(target=heartbeat, daemon=True).start()
        ws = helper.websocket.create_connection(
            helper.API.replace("http:", "ws:") + session["ws_url"], timeout=1)
        serial = (RUN / "serial.log").open("xb", buffering=0)
        helper.request(f"/api/v1/sessions/{sid}/board/power-off", "POST", b"")
        helper.request(f"/api/v1/sessions/{sid}/board/power-on", "POST", b"")
        buffer = bytearray()
        interrupted = False

        def wait_for(pattern, seconds):
            nonlocal interrupted
            regex = re.compile(pattern, re.S)
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                match = regex.search(buffer)
                if match:
                    matched = bytes(buffer[:match.end()])
                    del buffer[:match.end()]
                    return matched
                if stop.is_set():
                    raise RuntimeError("board heartbeat stopped")
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
        status["pll_verified"] = True
        send(f"pci enum; setenv autoload no; dhcp; setenv serverip 192.168.1.2; "
             f"tftpboot 0x02000000 ostool/sessions/{sid}/linux-rt-Image; "
             f"tftpboot 0x12000000 ostool/sessions/{sid}/linux-rt.dtb; "
             f"tftpboot 0x14000000 ostool/sessions/{sid}/initramfs.cpio; "
             f"setenv bootargs '{BOOTARGS}'; "
             f"booti 0x02000000 0x14000000:0x{INITRAMFS.stat().st_size:x} 0x12000000")
        boot = wait_for(rb"RESUME849_LINUX_INIT_DONE failures=\d+", 420)
        (RUN / "boot.log").write_bytes(boot)
        assert b"Linux version" in boot and b"PREEMPT_RT" in boot
        failures = int(re.search(rb"RESUME849_LINUX_INIT_DONE failures=(\d+)", boot).group(1))
        assert failures == 0
        rows = [json.loads(line.split(" ", 1)[1])
                for line in boot.decode(errors="replace").splitlines()
                if line.startswith("RESUME849_RESULT ")]
        assert len(rows) == 9 and all(row["samples"] == 20000 for row in rows)
        assert [row["case"] for row in rows] == [
            "empty", "bitset_miss", "bitset_hit"] * 3
        assert boot.count(b"RESUME849_DONE 0") == 3
        assert b"RESUME849_INVALID" not in boot
        status.update({"state": "collected", "failures": failures, "rows": rows,
                       "boot_sha256": sha(RUN / "boot.log")})
        print("LINUX_ROWS", rows, flush=True)
    except Exception as error:
        status.update({"state": "stopped_with_error", "error": repr(error)})
        raise
    finally:
        stop.set()
        if serial is not None:
            serial.close()
        if ws is not None:
            ws.close()
        helper.request(f"/api/v1/sessions/{sid}", "DELETE")
        status["board_released"] = True
        (RUN / "results.json").write_text(json.dumps(status, indent=2) + "\n")
        print("BOARD_RELEASED", sid, flush=True)


if __name__ == "__main__":
    main()
