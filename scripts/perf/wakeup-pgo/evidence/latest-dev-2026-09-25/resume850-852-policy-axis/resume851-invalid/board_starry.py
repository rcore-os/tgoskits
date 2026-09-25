#!/usr/bin/env python3
"""Compare FIFO and OTHER receiver wake costs on the unchanged G image."""

import hashlib
import importlib.util
import json
import re
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "starry-run2"
IMAGE = ROOT.parent / "resume817-weighted-four-crate" / "resume817.bin"
BENCH = ROOT / "wake_cost.aarch64"
SOURCE = "69a33650763538692fafea27c869870ed0313642"
IMAGE_SHA = "9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3"
BENCH_SHA = "c9e96efb297c64017c55052368a3470cb717f83375c266213b1bb820163addc5"
BOARD = "OrangePi-5-Plus-2"
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
    assert sha(IMAGE) == IMAGE_SHA and sha(BENCH) == BENCH_SHA
    RUN.mkdir()
    session = json.loads(helper.request("/api/v1/sessions", "POST", {
        "board_type": "OrangePi-5-Plus", "board_id": BOARD,
        "required_tags": [], "client_name": "issue2308-resume851-starry",
    }))
    sid = session["session_id"]
    ws = None
    serial = None
    stop = threading.Event()
    status = {"experiment": "resume851", "source_head": SOURCE,
              "board_id": BOARD, "session_id": sid, "image_sha256": IMAGE_SHA,
              "benchmark_sha256": BENCH_SHA, "board_released": False}
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(helper.request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        for name, path in (("resume851.bin", IMAGE), ("resume851.sh", ROOT / "guest.sh"),
                           ("wake-cost", BENCH)):
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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume851.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"http://192.168.1.2:2999/share/sessions/{sid}/resume851.sh "
             f"-o /tmp/resume851.sh && break; sleep 2; done; "
             f"[ -s /tmp/resume851.sh ] && sh /tmp/resume851.sh {sid}")
        done = wait_for(rb"RESUME851_BOARD_DONE (\d+)", 600)
        overall = int(re.search(rb"RESUME851_BOARD_DONE (\d+)", done).group(1))
        status["guest_exit"] = overall
        for name in ("sha256", *(f"starry-{number}.log" for number in range(1, 7))):
            (RUN / name).write_bytes(helper.request(f"/share/sessions/{sid}/resume851-{name}"))
        assert (RUN / "sha256").read_text().split()[0] == BENCH_SHA
        rounds = []
        modes = ("fifo", "other", "other", "fifo", "fifo", "other")
        for number, mode in enumerate(modes, 1):
            path = RUN / f"starry-{number}.log"
            log = path.read_text()
            rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                    if line.startswith("RESUME851_RESULT ")]
            assert [row["case"] for row in rows] == [
                "empty", "bitset_miss", "bitset_hit"]
            assert all(row["mode"] == mode for row in rows)
            assert all(row["samples"] == 20000 for row in rows)
            expected_policy = 1 if mode == "fifo" else 0
            assert f"RESUME851_POLICY mode={mode} sender=1 receiver={expected_policy} cpu=0" in log
            assert "RESUME851_DONE 0" in log and "DIAGNOSTIC_EXIT 0" in log
            assert "RESUME851_INVALID" not in log
            rounds.append({"round": number, "mode": mode, "rows": rows,
                           "log_sha256": sha(path)})
            print("ROUND", number, mode, rows, flush=True)
        assert overall == 0
        status.update({"state": "collected", "rounds": rounds,
                       "serial_sha256": sha(RUN / "serial.log")})
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
