#!/usr/bin/env python3
"""Collect pre-registered futex wake-stage diagnostics on OrangePi-5-Plus-1."""

import importlib.util
import json
import re
import subprocess
import threading
import time
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
WORKTREE = Path("/home/zhourui/.codex/worktrees/03ae/tgoskits-dev")
IMAGE = ROOT / "image.bin"
BENCH = Path("/tmp/pr1775-orangepi/bench")
BOARD = "OrangePi-5-Plus-1"
SOURCE = "b292a098bb60ef604e7677c37cd95d926ff08200"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
API = "http://192.168.1.2:2999"
ROUNDS = ("other-1", "fifo-1", "other-2", "fifo-2", "other-3")
TEMPLATE = ROOT.parent / "resume720-other-epilogue" / "board.py"

spec = importlib.util.spec_from_file_location("resume720_board", TEMPLATE)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def check_source():
    head = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=WORKTREE, text=True
    ).strip()
    assert head == SOURCE, head
    subprocess.run(["sha256sum", "-c", str(ROOT / "source.sha256")],
                   cwd=WORKTREE, check=True)
    assert helper.sha(BENCH) == BENCH_SHA
    assert IMAGE.exists() and IMAGE.stat().st_size > 10_000_000


def collect_rounds(sid):
    names = ["bench.sha256"]
    for sequence in ROUNDS:
        prefix = f"{sequence}-thread_futex_same_cpu"
        names.append(f"{prefix}.log")
        for kind in ("sched", "futex"):
            names.extend((f"{prefix}-{kind}-before", f"{prefix}-{kind}-after"))
    for name in names:
        (RUN / name).write_bytes(helper.request(f"/share/sessions/{sid}/resume862-{name}"))
    assert (RUN / "bench.sha256").read_text().split()[0] == BENCH_SHA

    rounds = []
    for sequence in ROUNDS:
        policy = sequence.split("-", 1)[0]
        prefix = f"{sequence}-thread_futex_same_cpu"
        log = (RUN / f"{prefix}.log").read_text()
        rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                if line.startswith("WAKEUP_LATENCY_RESULT ")]
        assert len(rows) == 1
        row = rows[0]
        assert (row["policy"], row["case"]) == (policy, "thread_futex_same_cpu")
        exit_zero = "DIAGNOSTIC_EXIT 0" in log and "WAKEUP_LATENCY_PASSED" in log
        complete = (row["samples"] == row["attempted"] == 20000
                    and row["not_parked"] == row["missed_deadlines"] == 0)
        deltas = {}
        for kind in ("sched", "futex"):
            before = helper.parse_metrics(RUN / f"{prefix}-{kind}-before")
            after = helper.parse_metrics(RUN / f"{prefix}-{kind}-after")
            assert before.keys() == after.keys()
            delta = {key: after[key] - before[key] for key in before}
            assert all(value >= 0 for value in delta.values())
            deltas[kind] = delta
        futex = deltas["futex"]
        expected = {
            "futex_wake_skipped", "futex_wake_selected_zero",
            "futex_wake_selected_many", "futex_wake_selected_one_coalesced",
            "futex_wake_selected_one_enqueued",
            "futex_wake_key_and_hint_total_ns", "futex_wake_bucket_lock_total_ns",
            "futex_wake_collect_total_ns", "futex_wake_unlock_total_ns",
            "futex_wake_wake_batch_total_ns",
        }
        assert set(futex) == expected, set(futex) ^ expected
        valid = exit_zero and complete
        rounds.append({
            "sequence": sequence, "policy": policy, "valid": valid,
            "benchmark": row, "delta": deltas,
            "raw_log_sha256": helper.sha(RUN / f"{prefix}.log"),
        })
        print("ROUND", sequence, "VALID" if valid else "INVALID", row["p50_ns"],
              flush=True)
    return rounds


def main():
    assert not RUN.exists(), "run1 already exists"
    check_source()
    RUN.mkdir()
    started = time.monotonic()
    while True:
        try:
            session = json.loads(helper.request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus", "board_id": BOARD,
                "required_tags": [], "client_name": "issue2308-resume862-stage-probe",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - started > 360:
                raise
            time.sleep(2)
    sid = session["session_id"]
    stop = threading.Event()
    ws = None
    serial = None
    try:
        assert session["board_id"] == BOARD
        (RUN / "session.json").write_text(json.dumps(session, indent=2) + "\n")
        dtb = json.loads(helper.request(f"/api/v1/sessions/{sid}/dtb"))
        dtb_path = dtb["relative_path"]
        assert dtb_path == f"ostool/sessions/{sid}/boot/dtb/orangepi-5-plus.dtb"
        for name, path in (("resume862.bin", IMAGE), ("resume862.sh", ROOT / "guest.sh"),
                           ("bench", BENCH)):
            helper.request(f"/api/v1/sessions/{sid}/files", "PUT", path.read_bytes(),
                           {"X-File-Path": name})

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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume862.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == IMAGE.stat().st_size
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"{API}/share/sessions/{sid}/resume862.sh -o /tmp/resume862.sh "
             f"&& break; sleep 2; done; "
             f"[ -s /tmp/resume862.sh ] && sh /tmp/resume862.sh {sid}")
        done = wait_for(rb"RESUME862_DONE (\d+)", 900)
        assert re.search(rb"RESUME862_DONE (\d+)", done).group(1) == b"0"
        rounds = collect_rounds(sid)
        (RUN / "results.json").write_text(json.dumps({
            "diagnostic_only": True, "source_head": SOURCE,
            "image_sha256": helper.sha(IMAGE), "bench_sha256": BENCH_SHA,
            "board_id": BOARD, "session_id": sid, "rounds": rounds,
        }, indent=2) + "\n")
        print("RESUME862_ROUNDS", len(rounds), flush=True)
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
