#!/usr/bin/env python3
"""Collect matched A/F whole-case PMU counts; never use as full20 evidence."""

import hashlib
import json
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, "/tmp/issue2308-pydeps")
import websocket

ROOT = Path(__file__).resolve().parent
EXPERIMENTS = ROOT.parent
WORKTREE = Path("/home/zhourui/.codex/worktrees/03ae/tgoskits-dev")
API = "http://10.3.10.194:2999"
GUEST_API = "http://192.168.1.2:2999"
BOARD = "OrangePi-5-Plus-1"
SOURCE = "69a33650763538692fafea27c869870ed0313642"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
COLLECTOR_SHA = "47b89a6df65277ef7e0b3c26f6dc1c2a49d019219df7a92fbad2b39b135d1394"
IMAGES = {
    "A": (
        EXPERIMENTS / "resume786-matched-ordinary/resume786.bin",
        "750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473",
    ),
    "F": (
        EXPERIMENTS / "resume785-feature-matched-pgouse/resume785.bin",
        "e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d",
    ),
}
BENCH = Path("/tmp/pr1775-orangepi/bench")
COLLECTOR = EXPERIMENTS / "resume149-fixed-count"
ROUNDS = ("A1", "F1", "F2", "A2")


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
    with urllib.request.urlopen(req, timeout=60) as response:
        return response.read()


def save_status(status):
    (ROOT / "status.json").write_text(json.dumps(status, indent=2) + "\n")


def main():
    assert not (ROOT / "status.json").exists(), "diagnostic already started"
    assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=WORKTREE, text=True).strip() == SOURCE
    assert not subprocess.check_output(["git", "diff", "--binary"], cwd=WORKTREE)
    assert sha(BENCH) == BENCH_SHA and sha(COLLECTOR) == COLLECTOR_SHA
    for image, expected in IMAGES.values():
        assert image.stat().st_size > 10_000_000 and sha(image) == expected

    lease_start = time.monotonic()
    while True:
        try:
            session = json.loads(request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus",
                "board_id": BOARD,
                "required_tags": [],
                "client_name": "issue2308-resume808-current-pmu",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - lease_start > 360:
                raise
            time.sleep(2)

    sid = session["session_id"]
    status = {
        "experiment": "resume808",
        "source_commit": SOURCE,
        "source_patch_sha256": "d7c1388dec5a849b1bec41e49b51d1f4cea6e2e853f1b4b2ce58d195a71d2b78",
        "board_id": session["board_id"],
        "session_id": sid,
        "benchmark_sha256": BENCH_SHA,
        "collector_sha256": COLLECTOR_SHA,
        "guest_script_sha256": sha(ROOT / "guest.sh"),
        "image_sha256": {kind: expected for kind, (_, expected) in IMAGES.items()},
        "order": list(ROUNDS),
        "diagnostic_only": True,
        "limitations": ["whole-case CPU0 EL1 counts include startup, reverse handoffs and background", "not full20 or exclusive latency"],
        "rounds": [],
        "board_released": False,
    }
    ws = None
    serial = None
    stop = threading.Event()
    try:
        save_status(status)
        assert session["board_id"] == BOARD
        dtb = json.loads(request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        assert dtb_path == f"ostool/sessions/{sid}/boot/dtb/orangepi-5-plus.dtb"
        files = [
            ("A.bin", IMAGES["A"][0]),
            ("F.bin", IMAGES["F"][0]),
            ("resume808.sh", ROOT / "guest.sh"),
            ("bench", BENCH),
            ("fixed-count", COLLECTOR),
        ]
        for name, path in files:
            request(f"/api/v1/sessions/{sid}/files", "PUT", path.read_bytes(), {"X-File-Path": name})

        def heartbeat():
            while not stop.wait(3):
                try:
                    request(f"/api/v1/sessions/{sid}/heartbeat", "POST", b"")
                except Exception as error:
                    status["heartbeat_error"] = repr(error)
                    stop.set()

        threading.Thread(target=heartbeat, daemon=True).start()
        ws = websocket.create_connection(API.replace("http:", "ws:") + session["ws_url"], timeout=1)
        serial = (ROOT / "serial.log").open("xb", buffering=0)
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

        for index, tag in enumerate(ROUNDS):
            if index:
                interrupted = False
                send("reboot -f")
            wait_for(rb"=> ", 180)
            send("md.l fd818040 3; md.l fd818280 1; md.l fd818314 3")
            pll = wait_for(rb"fd818314: 0000803f 00000000 00000000.*?=> ", 30)
            assert b"fd818040: 00000110 00000082 00000000" in pll
            assert b"fd818280: 00000001" in pll
            kind = tag[0]
            image = IMAGES[kind][0]
            send(f"pci enum; setenv autoload no; dhcp; setenv serverip 192.168.1.2; "
                 f"tftpboot 0x02000000 ostool/sessions/{sid}/{kind}.bin; "
                 f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
            boot = wait_for(rb"root@starry:~# ", 300)
            if b"DHCP acquired address" not in boot:
                boot += wait_for(rb"DHCP acquired address[^\r\n]*", 180)
            assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
            assert b"cpufreq: A55 816->1008" not in boot
            sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
            assert len(sizes) >= 2 and sizes[0] == image.stat().st_size
            send(f"curl --connect-timeout 10 --max-time 20 -fsS "
                 f"{GUEST_API}/share/sessions/{sid}/resume808.sh -o /tmp/resume808.sh "
                 f"&& sh /tmp/resume808.sh {sid} {tag}")
            terminal = wait_for(f"RESUME808_DONE {tag} [01]".encode(), 500)
            rc = int(re.search(f"RESUME808_DONE {tag} ([01])".encode(), terminal).group(1))
            record = {"tag": tag, "kind": kind, "guest_exit": rc, "logs": {}}
            names = ["sha256"] + [f"{policy}-{round}.log" for policy in ("other", "fifo") for round in (1, 2, 3)]
            for name in names:
                content = request(f"/share/sessions/{sid}/resume808-{tag}-{name}")
                path = ROOT / f"{tag}-{name}"
                path.write_bytes(content)
                record["logs"][name] = {"sha256": sha(path), "bytes": len(content)}
            status["rounds"].append(record)
            save_status(status)
            print("ROUND", tag, "guest_exit", rc, flush=True)
        status["state"] = "collected"
        print("RESUME808_COLLECTED", len(status["rounds"]), flush=True)
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
            save_status(status)


if __name__ == "__main__":
    main()
