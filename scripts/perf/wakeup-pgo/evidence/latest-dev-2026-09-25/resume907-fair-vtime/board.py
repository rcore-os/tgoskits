#!/usr/bin/env python3
"""Compare the same-source Fair virtual-time candidate on Plus-1."""

import hashlib
import importlib.util
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
import toml

ROOT = Path(__file__).resolve().parent / "full20"
WORKTREE = Path("/home/zhourui/.codex/worktrees/03ae/tgoskits-dev")
EXPERIMENTS = ROOT.parent
BUILD_ARTIFACTS = ROOT.parent
BUILD = BUILD_ARTIFACTS / "ordinary.toml"
CONTROL_RECORD = BUILD_ARTIFACTS.parent / "resume898-same-mm-fast/full20/results.json"
IMAGES = {
    "A": ("resume907-A.bin", BUILD_ARTIFACTS.parent / "resume898-same-mm-fast/ordinary-control.bin"),
    "B": ("resume907-B.bin", BUILD_ARTIFACTS / "ordinary-candidate.bin"),
}
BENCH = Path("/tmp/pr1775-orangepi/bench")
BASELINE = Path("/home/zhourui/.codex/artifacts/pr1775-perf/linux-rt-orangepi-5-plus-1-baseline.json")
BOARD = "OrangePi-5-Plus-1"
API = "http://10.3.10.194:2999"
GUEST_API = "http://192.168.1.2:2999"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
SEQUENCE = ("A1", "B1", "B2", "A2")
validator_path = Path("/home/zhourui/.codex/artifacts/issue2308-perf/resume711-board-evidence/resume711-board-bonly.py")
spec = importlib.util.spec_from_file_location("resume711_validator", validator_path)
validator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validator)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def request(path, method="GET", data=None, headers=None):
    if isinstance(data, dict):
        data = json.dumps(data).encode()
        headers = {**(headers or {}), "Content-Type": "application/json"}
    req = urllib.request.Request(API + path, data=data, headers=headers or {}, method=method)
    with urllib.request.urlopen(req, timeout=60) as response:
        return response.read()


def main():
    assert not ROOT.exists(), "experiment already started"
    assert sha(BENCH) == BENCH_SHA
    assert all(path.exists() and path.stat().st_size > 10_000_000
               for _, path in IMAGES.values())
    source_head = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=WORKTREE, text=True).strip()
    assert source_head == "b292a098bb60ef604e7677c37cd95d926ff08200"
    patch_sha = sha(BUILD_ARTIFACTS / "source.patch")
    assert patch_sha == "b824173a37744537d972c168b61178a21031f86ce21a21903e28ea134c729a50"
    assert subprocess.check_output(["git", "diff", "--binary"], cwd=WORKTREE) == (
        BUILD_ARTIFACTS / "source.patch").read_bytes()
    subprocess.run(["git", "apply", "--reverse", "--check", str(BUILD_ARTIFACTS / "source.patch")],
                   cwd=WORKTREE, check=True)
    config = toml.load(BUILD)
    assert len(config["features"]) == 10
    assert "ax-driver/rk3588-cpufreq" not in config["features"]
    assert "qperf" not in " ".join(config["features"])
    assert "env" not in config
    image_sha = {kind: sha(path) for kind, (_, path) in IMAGES.items()}
    assert image_sha == {
        "A": "31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9",
        "B": "448a418b0ef54a3d1b88dbf4af5ee434ba1199a7f83a00363e0f7f7ca7ce940a",
    }
    prior = json.loads(CONTROL_RECORD.read_text())
    assert prior["source_head"] == source_head
    assert prior["build_sha256"] == sha(BUILD)
    assert prior["image_sha256"]["A"] == image_sha["A"]
    assert all(round_["valid"] and len(round_["rows"]) == 20
               for round_ in prior["rounds"] if round_["tag"] in ("A1", "A2"))
    baseline = json.loads(BASELINE.read_text())
    assert ("other", "thread_futex_same_cpu") in {
        (row["policy"], row["case"]) for row in baseline["results"]}
    ROOT.mkdir()

    lease_start = time.monotonic()
    while True:
        try:
            session = json.loads(request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus", "board_id": BOARD,
                "required_tags": [], "client_name": "issue2308-resume907-fair-vtime-full20",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - lease_start > 360:
                raise
            time.sleep(2)
    sid = session["session_id"]
    ws = None
    stop = threading.Event()
    serial = None
    try:
        assert session["board_id"] == BOARD
        (ROOT / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        assert dtb_path == f"ostool/sessions/{sid}/boot/dtb/orangepi-5-plus.dtb"
        for name, path in list(IMAGES.values()) + [("resume756.sh", BUILD_ARTIFACTS.parent / "resume788-pgo-repeat" / "guest.sh"), ("bench", BENCH)]:
            request(f"/api/v1/sessions/{sid}/files", "PUT", path.read_bytes(),
                    {"X-File-Path": name})

        def heartbeat():
            while not stop.wait(3):
                try:
                    request(f"/api/v1/sessions/{sid}/heartbeat", "POST", b"")
                except Exception as error:
                    print("HEARTBEAT_ERROR", repr(error), flush=True)
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
                    raise RuntimeError("heartbeat stopped")
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

        rounds = []
        for index, tag in enumerate(SEQUENCE):
            if index:
                interrupted = False
                send("reboot -f")
            wait_for(rb"=> ", 180)
            send("md.l fd818040 3; md.l fd818280 1; md.l fd818314 3")
            pll = wait_for(rb"fd818314: 0000803f 00000000 00000000.*?=> ", 30)
            assert b"fd818040: 00000110 00000082 00000000" in pll
            assert b"fd818280: 00000001" in pll
            kind = tag[0]
            image_name, image = IMAGES[kind]
            send(f"pci enum; setenv autoload no; dhcp; setenv serverip 192.168.1.2; "
                 f"tftpboot 0x02000000 ostool/sessions/{sid}/{image_name}; "
                 f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
            boot = wait_for(rb"root@starry:~# ", 300)
            if b"DHCP acquired address" not in boot:
                boot += wait_for(rb"DHCP acquired address[^\r\n]*", 180)
            assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
            assert b"Primary CPU 0 started" in boot
            assert b"cpufreq: A55 816->1008" not in boot
            for cpu in range(1, 8):
                assert f"Secondary CPU {cpu} started.".encode() in boot
                assert f"Secondary CPU {cpu} init OK.".encode() in boot
            sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
            assert len(sizes) >= 2 and sizes[0] == image.stat().st_size

            send(f"curl --connect-timeout 10 --max-time 20 -fsS "
                 f"{GUEST_API}/share/sessions/{sid}/resume756.sh -o /tmp/resume756.sh "
                 f"&& sh /tmp/resume756.sh {sid} {tag}")
            terminal = wait_for(f"RESUME756_FULL_DONE {tag} [0-9]+".encode(), 360)
            guest_exit = int(re.search(f"RESUME756_FULL_DONE {tag} ([0-9]+)".encode(),
                                       terminal).group(1))
            log_path = ROOT / f"{tag}-full.log"
            sha_path = ROOT / f"{tag}.sha256"
            with log_path.open("xb") as handle:
                handle.write(request(f"/share/sessions/{sid}/resume756-{tag}-full.log"))
            with sha_path.open("xb") as handle:
                handle.write(request(f"/share/sessions/{sid}/resume756-{tag}.sha256"))
            try:
                rows, _, extra = validator.verify_log(log_path.read_text(), baseline,
                                                       BENCH_SHA, sha_path.read_text(), tag)
                assert extra["samples"] == 380000
                failure = None if guest_exit == 0 else f"guest exit {guest_exit}"
            except AssertionError as error:
                rows = []
                failure = str(error)
            rounds.append({"tag": tag, "image_sha256": image_sha[kind],
                           "raw_log_sha256": sha(log_path), "valid": failure is None,
                           "error": failure, "rows": rows})
            result = {"source_head": source_head, "source_patch_sha256": patch_sha,
                      "build_sha256": sha(BUILD),
                      "image_sha256": image_sha,
                      "bench_sha256": BENCH_SHA, "board_id": BOARD,
                      "session_id": sid, "rounds": rounds}
            (ROOT / "results.json").write_text(json.dumps(result, indent=2) + "\n")
            if failure is None:
                focus = next(row for row in rows if row["policy"] == "other" and
                             row["case"] == "thread_futex_same_cpu")
                print("RUN", tag, "valid full20 p50", focus["p50_ns"], flush=True)
            else:
                print("RUN", tag, "INVALID", failure, flush=True)
        print("RESUME907_FULL20_COMPLETE", len(rounds), flush=True)
    finally:
        stop.set()
        if serial is not None:
            serial.close()
        if ws is not None:
            ws.close()
        request(f"/api/v1/sessions/{sid}", "DELETE")
        print("BOARD_RELEASED", sid, flush=True)


if __name__ == "__main__":
    main()
