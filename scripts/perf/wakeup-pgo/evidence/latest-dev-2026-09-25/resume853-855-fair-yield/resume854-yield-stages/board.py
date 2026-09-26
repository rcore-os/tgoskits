#!/usr/bin/env python3
"""Collect exact-head FIFO/OTHER yield-stage qperf deltas on one board boot."""

import hashlib
import importlib.util
import json
import re
import subprocess
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
WORKTREE = Path("/home/zhourui/.codex/worktrees/03ae/tgoskits-dev")
IMAGE = ROOT / "image.bin"
BENCH = Path("/tmp/pr1775-orangepi/bench")
SOURCE = "69a33650763538692fafea27c869870ed0313642"
IMAGE_SHA = "255c03696d0de9b808c6c06a9d3cf61303557212c602253016ae500f2e0877fc"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
BOARD = "OrangePi-5-Plus-2"
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
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
    assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=WORKTREE,
                                   text=True).strip() == SOURCE
    assert not subprocess.check_output(["git", "diff", "--binary"], cwd=WORKTREE)
    config = (ROOT / "build.toml").read_text()
    assert '"qperf-metrics"' in config and '"ax-driver/rk3588-cpufreq"' not in config
    RUN.mkdir()
    session = json.loads(helper.request("/api/v1/sessions", "POST", {
        "board_type": "OrangePi-5-Plus", "board_id": BOARD,
        "required_tags": [], "client_name": "issue2308-resume854-yield-stages",
    }))
    sid = session["session_id"]
    ws = None
    serial = None
    stop = threading.Event()
    status = {"experiment": "resume854", "source_head": SOURCE,
              "board_id": BOARD, "session_id": sid, "image_sha256": IMAGE_SHA,
              "benchmark_sha256": BENCH_SHA,
              "build_config_sha256": sha(ROOT / "build.toml"),
              "board_released": False}
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(helper.request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        for name, path in (("resume854.bin", IMAGE), ("resume854.sh", ROOT / "guest.sh"),
                           ("bench", BENCH)):
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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume854.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == IMAGE.stat().st_size
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"http://192.168.1.2:2999/share/sessions/{sid}/resume854.sh "
             f"-o /tmp/resume854.sh && break; sleep 2; done; "
             f"[ -s /tmp/resume854.sh ] && sh /tmp/resume854.sh {sid}")
        done = wait_for(rb"RESUME854_BOARD_DONE (\d+)", 900)
        status["guest_exit"] = int(re.search(rb"RESUME854_BOARD_DONE (\d+)", done).group(1))
        names = ["sha256"]
        for number in range(1, 7):
            names.extend((f"round-{number}.log", f"round-{number}-before",
                          f"round-{number}-after"))
        for name in names:
            (RUN / name).write_bytes(helper.request(
                f"/share/sessions/{sid}/resume854-{name}"))
        assert (RUN / "sha256").read_text().split()[0] == BENCH_SHA
        rounds = []
        for number, mode in enumerate(MODES, 1):
            prefix = f"round-{number}"
            path = RUN / f"{prefix}.log"
            log = path.read_text()
            rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                    if line.startswith("WAKEUP_LATENCY_RESULT ")]
            before = helper.parse_metrics(RUN / f"{prefix}-before")
            after = helper.parse_metrics(RUN / f"{prefix}-after")
            assert before.keys() == after.keys()
            delta = {key: after[key] - before[key] for key in before}
            assert all(value >= 0 for value in delta.values())
            valid = (len(rows) == 1 and rows[0]["policy"] == mode
                     and rows[0]["case"] == "sched_yield_handoff"
                     and rows[0]["samples"] == rows[0]["attempted"] == 20000
                     and rows[0]["not_parked"] == rows[0]["missed_deadlines"] == 0
                     and "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log)
            names = ("account", "put_prev", "pick", "rq_commit", "selection_tail")
            counts = [delta.get(f"switch_scheduler_detail_{name}_count", 0)
                      for name in names]
            valid = valid and all(
                count >= 20000 and abs(count - counts[0]) <= counts[0] * 0.03
                for count in counts)
            rounds.append({"round": number, "mode": mode, "valid": valid,
                           "rows": rows, "counts": counts, "delta": delta,
                           "log_sha256": sha(path),
                           "before_sha256": sha(RUN / f"{prefix}-before"),
                           "after_sha256": sha(RUN / f"{prefix}-after")})
            print("ROUND", number, mode, "VALID" if valid else "INVALID",
                  rows[0]["p50_ns"] if rows else "NO_ROW", counts, flush=True)
        status.update({"state": "collected" if status["guest_exit"] == 0
                       and all(row["valid"] for row in rounds) else "invalid",
                       "rounds": rounds,
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
