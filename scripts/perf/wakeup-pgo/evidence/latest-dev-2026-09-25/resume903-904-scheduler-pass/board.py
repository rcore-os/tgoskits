#!/usr/bin/env python3
"""Collect qperf scheduler-pass classes on OrangePi-5-Plus-1."""

import hashlib
import importlib.util
import json
import os
import re
import subprocess
import threading
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent
RUN_NAME = os.environ.get("RESUME904_RUN", "run1")
assert RUN_NAME in ("run1", "run2")
RUN = ROOT / RUN_NAME
WORKTREE = Path("/home/zhourui/.codex/worktrees/03ae/tgoskits-dev")
IMAGE = ROOT / "image.bin"
BENCH = Path("/tmp/pr1775-orangepi/bench")
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
BOARD = "OrangePi-5-Plus-1"
MODES = ("fifo", "other", "other", "fifo", "fifo", "other")
HELPER = Path("/home/zhourui/.codex/artifacts/issue2308-perf/resume720-other-epilogue/board.py")
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
    assert IMAGE.is_file() and IMAGE.stat().st_size > 10_000_000
    assert sha(BENCH) == BENCH_SHA
    assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=WORKTREE,
                                   text=True).strip() == SOURCE
    assert subprocess.check_output(["git", "diff", "--binary"], cwd=WORKTREE) == (
        ROOT / "source.patch").read_bytes()
    assert '"qperf-metrics"' in (ROOT / "build.toml").read_text()
    session = json.loads(helper.request("/api/v1/sessions", "POST", {
        "board_type": "OrangePi-5-Plus", "board_id": BOARD,
        "required_tags": [], "client_name": f"issue2308-resume904-{RUN_NAME}",
    }))
    RUN.mkdir()
    sid = session["session_id"]
    ws = None
    serial = None
    stop = threading.Event()
    status = {
        "experiment": "resume904", "source_head": SOURCE,
        "board_id": BOARD, "session_id": sid,
        "image_sha256": sha(IMAGE), "benchmark_sha256": BENCH_SHA,
        "source_patch_sha256": sha(ROOT / "source.patch"),
        "build_config_sha256": sha(ROOT / "build.toml"),
        "board_released": False,
    }
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(helper.request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        for name, path in (("resume904.bin", IMAGE),
                           ("resume904.sh", ROOT / "guest.sh"),
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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume904.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == IMAGE.stat().st_size
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"http://192.168.1.2:2999/share/sessions/{sid}/resume904.sh "
             f"-o /tmp/resume904.sh && break; sleep 2; done; "
             f"[ -s /tmp/resume904.sh ] && sh /tmp/resume904.sh {sid}")
        done = wait_for(rb"RESUME904_BOARD_DONE (\d+)", 900)
        status["guest_exit"] = int(re.search(rb"RESUME904_BOARD_DONE (\d+)", done).group(1))
        names = ["sha256"]
        for number in range(1, 7):
            names.extend((f"round-{number}.log", f"round-{number}-before",
                          f"round-{number}-after"))
        for name in names:
            (RUN / name).write_bytes(helper.request(
                f"/share/sessions/{sid}/resume904-{name}"))
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
                     and rows[0]["case"] == "thread_futex_same_cpu"
                     and rows[0]["samples"] == rows[0]["attempted"] == 20000
                     and rows[0]["not_parked"] == rows[0]["missed_deadlines"] == 0
                     and "WAKEUP_LATENCY_PASSED" in log
                     and "DIAGNOSTIC_EXIT 0" in log)
            rounds.append({"round": number, "mode": mode, "valid": valid,
                           "rows": rows, "delta": delta, "log_sha256": sha(path),
                           "before_sha256": sha(RUN / f"{prefix}-before"),
                           "after_sha256": sha(RUN / f"{prefix}-after")})
            print("ROUND", number, mode, "VALID" if valid else "INVALID",
                  delta["preempt_schedule_rq_no_switch"],
                  delta["preempt_schedule_early_no_switch"],
                  delta["preempt_schedule_repeats"], flush=True)
        status.update({"state": "collected" if status["guest_exit"] == 0
                       and all(row["valid"] for row in rounds) else "invalid",
                       "rounds": rounds, "serial_sha256": sha(RUN / "serial.log")})
    except Exception as error:
        status.update({"state": "stopped_with_error", "error": repr(error)})
        raise
    finally:
        stop.set()
        if serial is not None:
            serial.close()
        if ws is not None:
            ws.close()
        try:
            helper.request(f"/api/v1/sessions/{sid}", "DELETE")
            status["board_released"] = True
        except Exception as error:
            status["release_error"] = repr(error)
        (RUN / "results.json").write_text(json.dumps(status, indent=2) + "\n")
        print("BOARD_RELEASED", sid, flush=True)


if __name__ == "__main__":
    main()
