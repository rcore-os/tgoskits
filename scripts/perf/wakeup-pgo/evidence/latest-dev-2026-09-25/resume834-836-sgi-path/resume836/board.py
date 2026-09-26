#!/usr/bin/env python3
"""Collect paired CPU1 IPI issue-to-entry diagnostics for cross-CPU futex."""

import importlib.util
import json
import re
import threading
import time
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUN = ROOT / "run1"
BENCH = Path("/tmp/pr1775-orangepi/bench")
SOURCE = "69a33650763538692fafea27c869870ed0313642"
BENCH_SHA = "94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773"
BOARD = "OrangePi-5-Plus-2"
GUEST_API = "http://192.168.1.2:2999"
TEMPLATE = ROOT.parent / "resume720-other-epilogue" / "board.py"
spec = importlib.util.spec_from_file_location("resume720_board", TEMPLATE)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def main():
    assert not RUN.exists()
    assert helper.sha(BENCH) == BENCH_SHA
    image = ROOT / "image.bin"
    assert image.stat().st_size > 10_000_000
    assert helper.sha(image) == "e803441a0d600b9d02d11899d1f028fb9c1c620e516901a6ed3b05877bf903ff"
    config_path = ROOT / "build.toml"
    config = config_path.read_text()
    assert '"qperf-metrics"' in config and '"ax-driver/rk3588-cpufreq"' not in config
    RUN.mkdir()

    start = time.monotonic()
    while True:
        try:
            session = json.loads(helper.request("/api/v1/sessions", "POST", {
                "board_type": "OrangePi-5-Plus", "board_id": BOARD,
                "required_tags": [], "client_name": "issue2308-resume836-sgi-flight",
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
        for name, path in (("resume836.bin", image), ("resume836.sh", ROOT / "guest.sh"),
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
             f"tftpboot 0x02000000 ostool/sessions/{sid}/resume836.bin; "
             f"tftpboot 0x12000000 {dtb_path}; booti 0x02000000 - 0x12000000")
        boot = wait_for(rb"root@starry:~# ", 300)
        assert b"Starting kernel" in boot and re.search(rb"smp\s*=\s*8", boot)
        sizes = [int(value) for value in re.findall(rb"Bytes transferred = (\d+)", boot)]
        assert len(sizes) >= 2 and sizes[0] == image.stat().st_size
        send(f"for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do "
             f"curl --connect-timeout 10 --max-time 20 -fsS "
             f"{GUEST_API}/share/sessions/{sid}/resume836.sh -o /tmp/resume836.sh "
             f"&& break; sleep 2; done; "
             f"[ -s /tmp/resume836.sh ] && sh /tmp/resume836.sh {sid}")
        done = wait_for(rb"RESUME836_DONE (\d+)", 900)
        assert re.search(rb"RESUME836_DONE (\d+)", done).group(1) == b"0"

        names = ["sha256"]
        cases = [(policy, case, round_no)
                 for policy in ("fifo", "other")
                 for case in ("thread_futex_cross_cpu",)
                 for round_no in (1, 2)]
        for policy, case, round_no in cases:
            prefix = f"{policy}-{case}-{round_no}"
            names.extend((f"{prefix}.log", f"{prefix}-before", f"{prefix}-after"))
        for name in names:
            (RUN / name).write_bytes(helper.request(f"/share/sessions/{sid}/resume836-{name}"))
        assert (RUN / "sha256").read_text().split()[0] == BENCH_SHA

        rounds = []
        for policy, case, round_no in cases:
            prefix = f"{policy}-{case}-{round_no}"
            log = (RUN / f"{prefix}.log").read_text()
            rows = [json.loads(line.split(" ", 1)[1]) for line in log.splitlines()
                    if line.startswith("WAKEUP_LATENCY_RESULT ")]
            assert len(rows) == 1 and (rows[0]["policy"], rows[0]["case"]) == (policy, case)
            assert "WAKEUP_LATENCY_PASSED" in log and "DIAGNOSTIC_EXIT 0" in log
            valid = (rows[0]["samples"] == rows[0]["attempted"] == 20000
                     and rows[0]["not_parked"] == rows[0]["missed_deadlines"] == 0)
            before = helper.parse_metrics(RUN / f"{prefix}-before")
            after = helper.parse_metrics(RUN / f"{prefix}-after")
            assert before.keys() == after.keys()
            for key in ("ipi_dispatch_count", "ipi_dispatch_total_ns",
                        "ipi_handler_count", "ipi_handler_total_ns",
                        "ipi_issue_count", "ipi_entry_count", "ipi_pair_count",
                        "ipi_overwrite_count", "ipi_unmatched_count", "ipi_backward_count",
                        "ipi_cpu0_pair_count", "ipi_cpu0_pair_total_ns"):
                assert key in before
            delta = {key: after[key] - before[key] for key in before}
            assert all(value >= 0 for value in delta.values())
            assert delta["ipi_dispatch_count"] == delta["ipi_handler_count"]
            assert delta["ipi_dispatch_total_ns"] >= delta["ipi_handler_total_ns"]
            assert abs(delta["ipi_entry_count"] - delta["ipi_dispatch_count"]) <= 16
            paired_entries = (delta["ipi_pair_count"] + delta["ipi_unmatched_count"]
                              + delta["ipi_backward_count"])
            assert abs(paired_entries - delta["ipi_entry_count"]) <= 16
            histogram_count = sum(delta[f"ipi_cpu0_pair_bucket_{index}"]
                                  for index in range(64))
            assert abs(histogram_count - delta["ipi_cpu0_pair_count"]) <= 16
            assert delta["ipi_cpu0_pair_count"] > 1000
            rounds.append({"policy": policy, "case": case, "round": round_no,
                           "valid": valid,
                           "benchmark": rows[0], "delta": delta,
                           "raw_log_sha256": helper.sha(RUN / f"{prefix}.log")})
            print("ROUND", policy, case, round_no, "VALID" if valid else "INVALID",
                  rows[0]["p50_ns"], flush=True)
        assert all(sum(row["valid"] for row in rounds if row["policy"] == policy) >= 1
                   for policy in ("fifo", "other"))
        (RUN / "results.json").write_text(json.dumps({
            "diagnostic_only": True, "source_head": SOURCE,
            "source_patch_sha256": helper.sha(ROOT / "probe.patch"),
            "image_sha256": helper.sha(image), "build_config_sha256": helper.sha(config_path),
            "bench_sha256": BENCH_SHA, "board_id": BOARD, "session_id": sid,
            "rounds": rounds,
        }, indent=2) + "\n")
        print("RESUME836_VALID_DIAGNOSTIC", len(rounds), flush=True)
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
