#!/usr/bin/env python3
"""Compare frozen, rebuilt, and forced-preemption same-CPU futex runs."""

import importlib.util
import json
import re
import threading
import time
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run3"
BENCH = Path("/tmp/pr1775-orangepi/bench")
SOURCE = "69a33650763538692fafea27c869870ed0313642"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
CONTROL_SHA = "6f582832f3e10d3d447d8f06afc706c0684c6f80a0b9346369b4dc59d922e713"
FORCED_SHA = "fabee7b6b3e312b07248d9d797cd3e3b40db406bac9f218d3516c66a1c3abe8c"
IMAGE_SHA = "9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3"
BOARD = "OrangePi-5-Plus-2"
GUEST_API = "http://192.168.1.2:2999"
TEMPLATE = ROOT.parent / "resume720-other-epilogue" / "board.py"
spec = importlib.util.spec_from_file_location("resume720_board", TEMPLATE)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def main():
    assert not RUN.exists()
    assert helper.sha(BENCH) == BENCH_SHA
    control = ROOT / "bench-control-rebuilt"
    forced = ROOT / "bench-forced-rt-preempt"
    assert helper.sha(control) == CONTROL_SHA
    assert helper.sha(forced) == FORCED_SHA
    image = ROOT.parent / "resume817-weighted-four-crate" / "resume817.bin"
    assert image.stat().st_size > 10_000_000
    assert helper.sha(image) == IMAGE_SHA
    config_path = ROOT / "build.toml"
    config = config_path.read_text()
    assert '"starry-kernel/board-profile-export"' in config
    assert '"qperf-metrics"' not in config and '"ax-driver/rk3588-cpufreq"' not in config
    RUN.mkdir()

    start = time.monotonic()
    while True:
        try:
            session = json.loads(helper.request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus", "board_id": BOARD,
                "required_tags": [], "client_name": "issue2308-resume840-forced-rt-preempt",
            }))
            break
        except urllib.error.HTTPError as error:
            if error.code != 409 or time.monotonic() - start > 360:
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
        for name, path in (("resume840.bin", image), ("resume840.sh", ROOT / "guest.sh"),
                           ("frozen", BENCH), ("control", control), ("forced", forced)):
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
        helper.request(f"/api/v1/sessions/{sid}/board/power-off", "POST", b"")
        helper.request(f"/api/v1/sessions/{sid}/board/power-on", "POST", b"")
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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume840.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == image.stat().st_size
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"{GUEST_API}/share/sessions/{sid}/resume840.sh -o /tmp/resume840.sh "
             f"&& break; sleep 2; done; "
             f"[ -s /tmp/resume840.sh ] && sh /tmp/resume840.sh {sid}")
        done = wait_for(rb"RESUME840_DONE (\d+)", 900)
        assert re.search(rb"RESUME840_DONE (\d+)", done).group(1) == b"0"

        cases = [("control", "fifo", 1), ("forced", "fifo", 1),
                 ("frozen", "fifo", 1), ("control", "other", 1),
                 ("control", "other", 2), ("frozen", "fifo", 2),
                 ("forced", "fifo", 2), ("control", "fifo", 2)]
        names = ["sha256"] + [f"{binary}-{policy}-{number}.log"
                               for binary, policy, number in cases]
        for name in names:
            (RUN / name).write_bytes(helper.request(f"/share/sessions/{sid}/resume840-{name}"))
        hashes = [(line.split()[0]) for line in (RUN / "sha256").read_text().splitlines()]
        assert hashes == [BENCH_SHA, CONTROL_SHA, FORCED_SHA]

        rounds = []
        for binary, policy, round_no in cases:
            prefix = f"{binary}-{policy}-{round_no}"
            log = (RUN / f"{prefix}.log").read_text()
            rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                    if line.startswith("WAKEUP_LATENCY_RESULT ")]
            assert len(rows) == 1
            assert (rows[0]["policy"], rows[0]["case"]) == (policy, "thread_futex_same_cpu")
            assert "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log
            marker = "DIAGNOSTIC_FORCED_RT_PREEMPT sender_priority=80 receiver_priority=81"
            assert (log.count(marker) == 1) == (binary == "forced")
            valid = (rows[0]["samples"] == rows[0]["attempted"] == 20000
                     and rows[0]["not_parked"] == rows[0]["missed_deadlines"] == 0)
            assert sum(rows[0]["histogram_counts"]) == rows[0]["samples"]
            rounds.append({"binary": binary, "policy": policy, "round": round_no,
                           "valid": valid, "benchmark": rows[0],
                           "raw_log_sha256": helper.sha(RUN / f"{prefix}.log")})
            print("ROUND", binary, policy, round_no, "VALID" if valid else "INVALID",
                  rows[0]["p50_ns"], flush=True)
        assert all(sum(row["valid"] for row in rounds if row["binary"] == binary) == count
                   for binary, count in (("control", 4), ("forced", 2), ("frozen", 2)))
        (RUN / "results.json").write_text(json.dumps({
            "diagnostic_only": True, "source_head": SOURCE,
            "image_sha256": helper.sha(image), "build_config_sha256": helper.sha(config_path),
            "bench_sha256": {"frozen": BENCH_SHA, "control": CONTROL_SHA,
                             "forced": FORCED_SHA},
            "board_id": BOARD, "session_id": sid,
            "rounds": rounds,
        }, indent=2) + "\n")
        print("RESUME840_VALID_DIAGNOSTIC", len(rounds), flush=True)
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
