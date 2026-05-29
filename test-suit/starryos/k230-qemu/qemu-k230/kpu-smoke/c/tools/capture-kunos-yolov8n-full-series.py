#!/usr/bin/env python3
import argparse
import json
import os
import select
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path


TRACE_EVENTS = [
    "k230_kpu_start",
    "k230_kpu_l2_store",
    "k230_kpu_l2_store_hash",
    "k230_kpu_gnne_summary",
]


def default_base_root(repo_root: Path) -> Path:
    parts = repo_root.parts
    if "target" in parts and "worktrees" in parts:
        target_index = parts.index("target")
        if target_index + 2 < len(parts) and parts[target_index + 1] == "worktrees":
            return Path(*parts[:target_index])
    return repo_root


def find_repo_root(start: Path) -> Path:
    for path in [start, *start.parents]:
        if (path / "Cargo.toml").exists() and (path / "test-suit").is_dir():
            return path
    raise RuntimeError(f"could not find repository root from {start}")


def default_qemu(base_root: Path) -> Path:
    candidates = [
        base_root / "target/qemu-k230-docker-build/qemu-system-riscv64",
        base_root / "target/qemu-k230/bin/qemu-system-riscv64",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return candidates[0]


def qmp_recv(qmp):
    line = qmp.readline()
    if not line:
        raise RuntimeError("QMP socket closed")
    return json.loads(line.decode("utf-8"))


def qmp_cmd(qmp, obj):
    qmp.write((json.dumps(obj) + "\r\n").encode("utf-8"))
    qmp.flush()
    while True:
        msg = qmp_recv(qmp)
        if "error" in msg:
            raise RuntimeError(msg)
        if "return" in msg:
            return msg["return"]


def wait_for(path: Path, timeout_s: float):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if path.exists():
            return
        time.sleep(0.1)
    raise TimeoutError(path)


def count_trace_summaries(trace_path: Path) -> int:
    if not trace_path.exists():
        return 0
    return trace_path.read_text(encoding="utf-8", errors="replace").count("k230_kpu_gnne_summary")


def wait_for_trace_summaries(proc, trace_path: Path, uart_log_path: Path, expected: int, timeout_s: float):
    deadline = time.time() + timeout_s
    last_count = -1
    last_size = -1
    with uart_log_path.open("w", encoding="utf-8", errors="replace") as uart_log:
        while time.time() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f"QEMU exited before {expected} KPU summaries")
            ready, _, _ = select.select([proc.stdout], [], [], 0.2)
            if ready:
                data = os.read(proc.stdout.fileno(), 4096)
                if data:
                    uart_log.write(data.decode("utf-8", errors="replace"))
                    uart_log.flush()
            count = count_trace_summaries(trace_path)
            if count >= expected:
                return count
            size = trace_path.stat().st_size if trace_path.exists() else 0
            if count != last_count or size != last_size:
                print(f"KPU summaries {count}/{expected}, trace bytes {size}", flush=True)
                last_count = count
                last_size = size
    raise TimeoutError(f"did not see {expected} KPU summaries in {trace_path}")


def clean_capture_dir(capture_dir: Path):
    capture_dir.mkdir(parents=True, exist_ok=True)
    for pattern in ["run-*.bin", "starts.jsonl"]:
        for path in capture_dir.glob(pattern):
            path.unlink()


def write_trace_events(path: Path):
    path.write_text("\n".join(TRACE_EVENTS) + "\n", encoding="utf-8")


def parse_args():
    script_dir = Path(__file__).resolve().parent
    repo_root = Path(os.environ.get("K230_REPO_ROOT", find_repo_root(script_dir)))
    base_root = Path(os.environ.get("K230_BASE_ROOT", default_base_root(repo_root)))
    out_dir = Path(os.environ.get("K230_OFFICIAL_DIR", base_root / "target/official-k230"))
    parser = argparse.ArgumentParser(
        description="Capture the kunOS/K230 SDK RT-Smart YOLOv8n full KPU submit series."
    )
    parser.add_argument("--repo-root", type=Path, default=repo_root, help="StarryOS worktree root")
    parser.add_argument("--base-root", type=Path, default=base_root, help="root that owns target/official-k230")
    parser.add_argument("--out-dir", type=Path, default=out_dir, help="official capture output directory")
    parser.add_argument("--qemu", type=Path, default=Path(os.environ.get("K230_QEMU", default_qemu(base_root))))
    parser.add_argument(
        "--uboot",
        type=Path,
        default=Path(
            os.environ.get(
                "K230_KUNOS_UBOOT",
                base_root / "target/upstreams/kunos/prebuilt/k230-sdk/riscv-nomtee/u-boot",
            )
        ),
    )
    parser.add_argument(
        "--image",
        type=Path,
        default=Path(
            os.environ.get(
                "K230_KUNOS_YOLOV8N_IMAGE",
                out_dir / "CanMV-K230_sdcard_v1.7_kunos-yolov8n-debug1.img",
            )
        ),
    )
    parser.add_argument(
        "--capture-dir",
        type=Path,
        default=Path(os.environ.get("K230_KPU_CAPTURE_DIR_OUT", out_dir / "yolov8n-prestart-snapshots")),
    )
    parser.add_argument(
        "--trace",
        type=Path,
        default=Path(os.environ.get("K230_KPU_FULL_TRACE", out_dir / "kunos-yolov8n-full-series-kpu-trace.log")),
    )
    parser.add_argument("--expected-runs", type=int, default=54)
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--keep-existing", action="store_true", help="do not remove old run-*.bin snapshots first")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    for path in [args.qemu, args.uboot, args.image]:
        if not path.exists():
            print(f"missing required file: {path}", file=sys.stderr)
            return 2

    if not args.keep_existing:
        clean_capture_dir(args.capture_dir)
    else:
        args.capture_dir.mkdir(parents=True, exist_ok=True)

    qmp_path = args.out_dir / "kunos-yolov8n-full-series.qmp"
    uart0 = args.out_dir / "kunos-yolov8n-full-series-uart0.log"
    trace_events = args.out_dir / "k230-kpu-capture-trace-events"
    write_trace_events(trace_events)

    for path in [qmp_path, uart0, args.trace]:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    for index in range(1, 5):
        try:
            (args.out_dir / f"kunos-yolov8n-full-series-uart{index}.log").unlink()
        except FileNotFoundError:
            pass

    qemu_args = [
        str(args.qemu),
        "-machine",
        "k230",
        "-smp",
        "2",
        "-m",
        "2G",
        "-bios",
        str(args.uboot),
        "-drive",
        f"if=sd,file={args.image},format=raw,snapshot=on",
        "-nic",
        "none",
        "-display",
        "none",
        "-qmp",
        f"unix:{qmp_path},server=on,wait=off",
        "-trace",
        f"events={trace_events},file={args.trace}",
        "-serial",
        "mon:stdio",
        "-serial",
        f"file:{args.out_dir / 'kunos-yolov8n-full-series-uart1.log'}",
        "-serial",
        f"file:{args.out_dir / 'kunos-yolov8n-full-series-uart2.log'}",
        "-serial",
        f"file:{args.out_dir / 'kunos-yolov8n-full-series-uart3.log'}",
        "-serial",
        f"file:{args.out_dir / 'kunos-yolov8n-full-series-uart4.log'}",
    ]

    sock = None
    env = os.environ.copy()
    env["K230_KPU_CAPTURE_DIR"] = str(args.capture_dir)
    proc = subprocess.Popen(
        qemu_args,
        cwd=args.base_root,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
    )
    try:
        wait_for(qmp_path, 20)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(str(qmp_path))
        qmp = sock.makefile("rwb")
        qmp_recv(qmp)
        qmp_cmd(qmp, {"execute": "qmp_capabilities"})

        summaries = wait_for_trace_summaries(proc, args.trace, uart0, args.expected_runs, args.timeout)
        print(f"KPU summaries reached: {summaries}", flush=True)
        qmp_cmd(qmp, {"execute": "quit"})
        proc.wait(timeout=20)
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if sock is not None:
            sock.close()

    starts = args.capture_dir / "starts.jsonl"
    start_count = 0
    if starts.exists():
        start_count = sum(1 for _ in starts.open("r", encoding="utf-8", errors="replace"))
    missing = []
    for index in range(1, args.expected_runs + 1):
        for kind in ["low16m", "l2", "ddr64m"]:
            path = args.capture_dir / f"run-{index:04d}-{kind}.bin"
            if not path.exists():
                missing.append(path.name)
    if start_count != args.expected_runs or missing:
        print(f"capture incomplete: starts={start_count}, missing={missing[:8]}", file=sys.stderr)
        return 1

    print(f"trace={args.trace} bytes={args.trace.stat().st_size}")
    print(f"snapshots={args.capture_dir} runs={start_count}")
    print(f"uart0={uart0} bytes={uart0.stat().st_size if uart0.exists() else 0}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
