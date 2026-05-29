#!/usr/bin/env python3
import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


# The QEMU capture hook dumps low16m from K230_GNNE_RUNTIME_RDATA_BASE
# (0x10000020), not from the page-aligned reserved-memory base.
LOW_CAPTURE_BASE = 0x10000020
LOW_SIZE = 0x01000000
L2_BASE = 0x80000000
L2_SIZE = 0x00200000
DDR_BASE = 0x3C000000
DDR_SIZE = 0x04000000
DIFF_BLOCK_SIZE = 0x1000

LOW_WINDOWS = [
    ("rdata", 0x10000000, 0x00090000),
    ("fake_output", 0x10090000, 0x00100000),
    ("command", 0x10190000, 0x00370000),
    ("direct_io", 0x10500000, 0x00B00000),
]

WINDOWS = [
    *LOW_WINDOWS,
    ("ddr", DDR_BASE, DDR_SIZE),
]


START_RE = re.compile(r"k230_kpu_start .* start 0x([0-9a-f]+) end 0x([0-9a-f]+) hi 0x([0-9a-f]+)")
STORE_HASH_RE = re.compile(r"k230_kpu_l2_store_hash .* dest_hash 0x([0-9a-f]+)")
STORE_RE = re.compile(r"k230_kpu_l2_store .* logical 0x([0-9a-f]+) physical 0x([0-9a-f]+) bytes ([0-9]+)")
SUMMARY_RE = re.compile(r"k230_kpu_gnne_summary .* instructions ([0-9]+) .* unknown ([0-9]+)")


def parse_commands(trace_path):
    commands = []
    current = None
    pending_dest_hash = None

    with open(trace_path, "r", encoding="utf-8", errors="replace") as file:
        for line in file:
            start = START_RE.search(line)
            if start:
                if current is not None:
                    commands.append(current)
                current = {
                    "start": int(start.group(1), 16),
                    "end": int(start.group(2), 16),
                    "hi": int(start.group(3), 16),
                    "stores": [],
                }
                pending_dest_hash = None
                continue

            if current is None:
                continue

            store_hash = STORE_HASH_RE.search(line)
            if store_hash:
                pending_dest_hash = int(store_hash.group(1), 16)
                continue

            store = STORE_RE.search(line)
            if store:
                current["stores"].append(
                    {
                        "logical": int(store.group(1), 16),
                        "physical": int(store.group(2), 16),
                        "bytes": int(store.group(3), 10),
                        "dest_hash": pending_dest_hash,
                    }
                )
                pending_dest_hash = None
                continue

            summary = SUMMARY_RE.search(line)
            if summary:
                current["instructions"] = int(summary.group(1), 10)
                current["unknown"] = int(summary.group(2), 10)

    if current is not None:
        commands.append(current)

    complete = [cmd for cmd in commands if cmd.get("stores") and "unknown" in cmd]
    if not complete:
        raise ValueError("trace did not contain a complete KPU command with l2_store and summary")
    return complete


def parse_last_command(trace_path):
    return parse_commands(trace_path)[-1]


def window_for_paddr(paddr, length):
    for name, base, size in LOW_WINDOWS:
        if base <= paddr and length <= size and paddr - base <= size - length:
            return name, paddr - base
    raise ValueError(f"physical range 0x{paddr:x}+0x{length:x} is outside replay windows")


def low_file_offset_for_paddr(paddr):
    if paddr < LOW_CAPTURE_BASE or paddr >= LOW_CAPTURE_BASE + LOW_SIZE:
        raise ValueError(f"physical address 0x{paddr:x} is outside low16m snapshot")
    return paddr - LOW_CAPTURE_BASE


def low_window_segments():
    capture_start = LOW_CAPTURE_BASE
    capture_end = LOW_CAPTURE_BASE + LOW_SIZE
    for name, base, size in LOW_WINDOWS:
        start = max(base, capture_start)
        end = min(base + size, capture_end)
        if start >= end:
            continue
        yield {
            "window": name,
            "window_offset": start - base,
            "file_offset": start - capture_start,
            "len": end - start,
        }


def make_sections(guest_low, guest_l2, guest_ddr=None):
    sections = [
        {
            "kind": "copy_file",
            "window": "l2",
            "offset": 0,
            "path": guest_l2,
            "file_offset": 0,
            "len": L2_SIZE,
        }
    ]
    for segment in low_window_segments():
        sections.append(
            {
                "kind": "copy_file",
                "window": segment["window"],
                "offset": segment["window_offset"],
                "path": guest_low,
                "file_offset": segment["file_offset"],
                "len": segment["len"],
            }
        )
    if guest_ddr is not None:
        sections.append(
            {
                "kind": "copy_file",
                "window": "ddr",
                "offset": 0,
                "path": guest_ddr,
                "file_offset": 0,
                "len": DDR_SIZE,
            }
        )
    return sections


def make_capture(command, guest_low, guest_l2, guest_ddr=None, expected_output_hash=None):
    command_len = command["end"] - command["start"]
    if command_len <= 0:
        raise ValueError("last command range is empty")
    if command["start"] < LOW_CAPTURE_BASE or command["end"] > LOW_CAPTURE_BASE + LOW_SIZE:
        raise ValueError("last command range is outside low16m snapshot")
    if command.get("unknown") != 0:
        raise ValueError("last command summary reported unknown instructions")

    store = command["stores"][-1]
    if store["dest_hash"] is None:
        raise ValueError("last command store did not have a dest_hash")
    check_hash = store["dest_hash"]
    if expected_output_hash is not None:
        check_hash = expected_output_hash
    check_window, check_offset = window_for_paddr(store["physical"], store["bytes"])

    sections = make_sections(guest_low, guest_l2, guest_ddr=guest_ddr)
    sections.append(
        {
            "kind": "fill",
            "window": check_window,
            "offset": check_offset,
            "len": store["bytes"],
            "byte": "0xa5",
        }
    )

    return {
        "name": "kunos_yolov8n_last_command",
        "command_paddr": f"0x{command['start']:x}",
        "command_file": {
            "path": guest_low,
            "file_offset": low_file_offset_for_paddr(command["start"]),
            "len": command_len,
        },
        "sections": sections,
        "checks": [
            {
                "window": check_window,
                "offset": check_offset,
                "total_len": store["bytes"],
                "fnv1a64": f"0x{check_hash:016x}",
            }
        ],
        "metadata": {
            "command_end": f"0x{command['end']:x}",
            "instructions": command.get("instructions"),
            "output_paddr": f"0x{store['physical']:x}",
            "output_len": store["bytes"],
            "official_store_hash": f"0x{store['dest_hash']:016x}",
        },
    }


def make_sequence_capture(commands, guest_low, guest_l2, guest_ddr=None, include_run_checks=True):
    runs = []
    total_command_bytes = 0
    total_stores = 0
    for index, command in enumerate(commands, start=1):
        command_len = command["end"] - command["start"]
        if command_len <= 0:
            raise ValueError(f"command {index} range is empty")
        if command["start"] < LOW_CAPTURE_BASE or command["end"] > LOW_CAPTURE_BASE + LOW_SIZE:
            raise ValueError(f"command {index} range is outside low16m snapshot")
        if command.get("unknown") != 0:
            raise ValueError(f"command {index} summary reported unknown instructions")

        checks = []
        for store in command["stores"]:
            if store["dest_hash"] is None:
                raise ValueError(f"command {index} store did not have a dest_hash")
            check_window, check_offset = window_for_paddr(store["physical"], store["bytes"])
            if include_run_checks:
                checks.append(
                    {
                        "window": check_window,
                        "offset": check_offset,
                        "total_len": store["bytes"],
                        "fnv1a64": f"0x{store['dest_hash']:016x}",
                    }
                )

        runs.append(
            {
                "index": index,
                "command_paddr": f"0x{command['start']:x}",
                "command_file": {
                    "path": guest_low,
                    "file_offset": low_file_offset_for_paddr(command["start"]),
                    "len": command_len,
                },
                "checks": checks,
                "metadata": {
                    "command_end": f"0x{command['end']:x}",
                    "instructions": command.get("instructions"),
                    "store_count": len(command["stores"]),
                },
            }
        )
        total_command_bytes += command_len
        total_stores += len(command["stores"])

    checks = []
    if not include_run_checks:
        final_store = commands[-1]["stores"][-1]
        check_window, check_offset = window_for_paddr(final_store["physical"], final_store["bytes"])
        checks.append(
            {
                "window": check_window,
                "offset": check_offset,
                "total_len": final_store["bytes"],
                "fnv1a64": f"0x{final_store['dest_hash']:016x}",
            }
        )

    return {
        "name": "kunos_yolov8n_full_sequence",
        "sections": make_sections(guest_low, guest_l2, guest_ddr=guest_ddr),
        "runs": runs,
        "checks": checks,
        "metadata": {
            "command_count": len(commands),
            "total_command_bytes": total_command_bytes,
            "store_count": total_stores,
            "run_checks": include_run_checks,
        },
    }


def snapshot_name(index, kind):
    return f"run-{index:04d}-{kind}.bin"


def snapshot_host_path(snapshot_dir, index, kind):
    return snapshot_dir / snapshot_name(index, kind)


def snapshot_guest_path(guest_dir, index, kind):
    return f"{guest_dir.rstrip('/')}/{snapshot_name(index, kind)}"


def compact_command_name(index):
    return f"yolov8n-full-sequence-delta-run-{index:04d}-command.bin"


def compact_delta_name(index, window, offset):
    return f"yolov8n-full-sequence-delta-run-{index:04d}-{window}-{offset:08x}.bin"


def copy_file_range(src_path, src_offset, length, dst_path, chunk_size=1024 * 1024):
    with src_path.open("rb") as src_file, dst_path.open("wb") as dst_file:
        src_file.seek(src_offset)
        remaining = length
        while remaining > 0:
            data = src_file.read(min(chunk_size, remaining))
            if not data:
                raise ValueError(f"short read from {src_path} at offset 0x{src_offset:x}")
            dst_file.write(data)
            remaining -= len(data)


def require_snapshot(path, size):
    if not path.is_file():
        raise FileNotFoundError(path)
    actual = path.stat().st_size
    if actual != size:
        raise ValueError(f"{path} must be exactly 0x{size:x} bytes, got 0x{actual:x}")


def diff_file_sections(
    prev_path,
    curr_path,
    guest_curr_path,
    window,
    window_offset,
    file_offset,
    length,
    block_size,
    *,
    run_index=None,
    compact_out_dir=None,
    compact_guest_dir=None,
):
    sections = []
    pending_start = None
    pending_len = 0

    def flush():
        nonlocal pending_start, pending_len
        if pending_start is None:
            return
        section_file_offset = file_offset + pending_start
        guest_path = guest_curr_path
        guest_file_offset = section_file_offset
        if compact_out_dir is not None:
            if run_index is None or compact_guest_dir is None:
                raise ValueError("compact delta output requires run_index and compact_guest_dir")
            compact_name = compact_delta_name(run_index, window, window_offset + pending_start)
            copy_file_range(curr_path, section_file_offset, pending_len, compact_out_dir / compact_name)
            guest_path = f"{compact_guest_dir.rstrip('/')}/{compact_name}"
            guest_file_offset = 0
        sections.append(
            {
                "kind": "copy_file",
                "window": window,
                "offset": window_offset + pending_start,
                "path": guest_path,
                "file_offset": guest_file_offset,
                "len": pending_len,
            }
        )
        pending_start = None
        pending_len = 0

    with prev_path.open("rb") as prev_file, curr_path.open("rb") as curr_file:
        for offset in range(0, length, block_size):
            size = min(block_size, length - offset)
            prev_file.seek(file_offset + offset)
            curr_file.seek(file_offset + offset)
            if prev_file.read(size) == curr_file.read(size):
                flush()
                continue
            if pending_start is None:
                pending_start = offset
                pending_len = size
            else:
                pending_len += size
    flush()
    return sections


def make_delta_sequence_capture(
    commands,
    snapshot_dir,
    guest_dir,
    include_ddr,
    include_run_checks=True,
    block_size=DIFF_BLOCK_SIZE,
    compact_out_dir=None,
):
    snapshot_dir = Path(snapshot_dir)
    if compact_out_dir is not None:
        compact_out_dir = Path(compact_out_dir)
        compact_out_dir.mkdir(parents=True, exist_ok=True)
    run_count = len(commands)
    for index in range(1, run_count + 1):
        require_snapshot(snapshot_host_path(snapshot_dir, index, "low16m"), LOW_SIZE)
        require_snapshot(snapshot_host_path(snapshot_dir, index, "l2"), L2_SIZE)
        if include_ddr:
            require_snapshot(snapshot_host_path(snapshot_dir, index, "ddr64m"), DDR_SIZE)

    sections = make_sections(
        snapshot_guest_path(guest_dir, 1, "low16m"),
        snapshot_guest_path(guest_dir, 1, "l2"),
        guest_ddr=snapshot_guest_path(guest_dir, 1, "ddr64m") if include_ddr else None,
    )

    runs = []
    total_command_bytes = 0
    total_stores = 0
    total_delta_sections = 0
    for index, command in enumerate(commands, start=1):
        command_len = command["end"] - command["start"]
        if command_len <= 0:
            raise ValueError(f"command {index} range is empty")
        if command["start"] < LOW_CAPTURE_BASE or command["end"] > LOW_CAPTURE_BASE + LOW_SIZE:
            raise ValueError(f"command {index} range is outside low16m snapshot")
        if command.get("unknown") != 0:
            raise ValueError(f"command {index} summary reported unknown instructions")

        checks = []
        for store in command["stores"]:
            if store["dest_hash"] is None:
                raise ValueError(f"command {index} store did not have a dest_hash")
            check_window, check_offset = window_for_paddr(store["physical"], store["bytes"])
            if include_run_checks:
                checks.append(
                    {
                        "window": check_window,
                        "offset": check_offset,
                        "total_len": store["bytes"],
                        "fnv1a64": f"0x{store['dest_hash']:016x}",
                    }
                )

        run_sections = []
        if index > 1:
            prev_low = snapshot_host_path(snapshot_dir, index - 1, "low16m")
            curr_low = snapshot_host_path(snapshot_dir, index, "low16m")
            curr_low_guest = snapshot_guest_path(guest_dir, index, "low16m")
            for segment in low_window_segments():
                run_sections.extend(
                    diff_file_sections(
                        prev_low,
                        curr_low,
                        curr_low_guest,
                        segment["window"],
                        segment["window_offset"],
                        segment["file_offset"],
                        segment["len"],
                        block_size,
                        run_index=index,
                        compact_out_dir=compact_out_dir,
                        compact_guest_dir=guest_dir,
                    )
                )

            run_sections.extend(
                diff_file_sections(
                    snapshot_host_path(snapshot_dir, index - 1, "l2"),
                    snapshot_host_path(snapshot_dir, index, "l2"),
                    snapshot_guest_path(guest_dir, index, "l2"),
                    "l2",
                    0,
                    0,
                    L2_SIZE,
                    block_size,
                    run_index=index,
                    compact_out_dir=compact_out_dir,
                    compact_guest_dir=guest_dir,
                )
            )
            if include_ddr:
                run_sections.extend(
                    diff_file_sections(
                        snapshot_host_path(snapshot_dir, index - 1, "ddr64m"),
                        snapshot_host_path(snapshot_dir, index, "ddr64m"),
                        snapshot_guest_path(guest_dir, index, "ddr64m"),
                        "ddr",
                        0,
                        0,
                        DDR_SIZE,
                        block_size,
                        run_index=index,
                        compact_out_dir=compact_out_dir,
                        compact_guest_dir=guest_dir,
                    )
                )
        total_delta_sections += len(run_sections)

        command_path = snapshot_guest_path(guest_dir, index, "low16m")
        command_file_offset = low_file_offset_for_paddr(command["start"])
        if compact_out_dir is not None:
            command_name = compact_command_name(index)
            copy_file_range(
                snapshot_host_path(snapshot_dir, index, "low16m"),
                command_file_offset,
                command_len,
                compact_out_dir / command_name,
            )
            command_path = f"{guest_dir.rstrip('/')}/{command_name}"
            command_file_offset = 0

        runs.append(
            {
                "index": index,
                "command_paddr": f"0x{command['start']:x}",
                "command_file": {
                    "path": command_path,
                    "file_offset": command_file_offset,
                    "len": command_len,
                },
                "sections": run_sections,
                "checks": checks,
                "metadata": {
                    "command_end": f"0x{command['end']:x}",
                    "instructions": command.get("instructions"),
                    "store_count": len(command["stores"]),
                    "delta_section_count": len(run_sections),
                },
            }
        )
        total_command_bytes += command_len
        total_stores += len(command["stores"])

    checks = []
    if not include_run_checks:
        final_store = commands[-1]["stores"][-1]
        check_window, check_offset = window_for_paddr(final_store["physical"], final_store["bytes"])
        checks.append(
            {
                "window": check_window,
                "offset": check_offset,
                "total_len": final_store["bytes"],
                "fnv1a64": f"0x{final_store['dest_hash']:016x}",
            }
        )

    return {
        "name": "kunos_yolov8n_full_sequence_delta",
        "sections": sections,
        "runs": runs,
        "checks": checks,
        "metadata": {
            "command_count": len(commands),
            "total_command_bytes": total_command_bytes,
            "store_count": total_stores,
            "run_checks": include_run_checks,
            "delta_block_size": block_size,
            "delta_section_count": total_delta_sections,
            "ddr_delta": include_ddr,
        },
    }


def copy_delta_snapshots(snapshot_dir, out_dir, run_count, include_ddr):
    for kind in ["low16m", "l2", *([] if not include_ddr else ["ddr64m"])]:
        shutil.copyfile(snapshot_host_path(snapshot_dir, 1, kind), out_dir / snapshot_name(1, kind))


def main():
    parser = argparse.ArgumentParser(
        description="Build StarryOS .krun replays from a kunOS YOLOv8n KPU trace."
    )
    parser.add_argument("--trace", required=True, help="kunOS KPU trace log")
    parser.add_argument(
        "--low16m",
        help="QEMU KPU low16m snapshot whose byte 0 maps to guest physical 0x10000020",
    )
    parser.add_argument("--l2", help="QMP pmemsave snapshot for 0x80000000+0x200000")
    parser.add_argument("--ddr", help="optional QMP pmemsave snapshot for 0x3c000000+0x4000000")
    parser.add_argument(
        "--snapshot-dir",
        help="directory containing run-0001-low16m.bin/run-0001-l2.bin/... pre-start snapshots",
    )
    parser.add_argument("--out-dir", required=True, help="output capture directory")
    parser.add_argument(
        "--mode",
        choices=["last-command", "full-sequence", "full-sequence-delta"],
        default="last-command",
        help="capture shape to emit",
    )
    parser.add_argument(
        "--no-run-checks",
        action="store_true",
        help="for full-sequence mode, submit all commands but omit per-command store hash checks",
    )
    parser.add_argument(
        "--expected-output-hash",
        help=(
            "override the generated check_hash, for example when a deterministic single-command "
            "replay intentionally differs from the official full-runtime output"
        ),
    )
    parser.add_argument(
        "--guest-dir",
        default="/usr/share/k230-kpu-smoke/captures",
        help="directory where the capture files will be installed in the StarryOS rootfs",
    )
    parser.add_argument(
        "--snapshot-prefix",
        default="yolov8n-post",
        help="basename prefix for copied snapshots inside out-dir and guest-dir",
    )
    parser.add_argument("--copy-snapshots", action="store_true", help="copy snapshots into out-dir")
    parser.add_argument(
        "--delta-block-size",
        default=DIFF_BLOCK_SIZE,
        type=lambda value: int(value, 0),
        help="block granularity for full-sequence-delta snapshot diffs",
    )
    args = parser.parse_args()

    if args.mode != "full-sequence-delta" and (args.low16m is None or args.l2 is None):
        raise ValueError("--low16m and --l2 are required unless --mode full-sequence-delta")
    low16m = Path(args.low16m) if args.low16m is not None else None
    l2 = Path(args.l2) if args.l2 is not None else None
    if low16m is not None and low16m.stat().st_size != LOW_SIZE:
        raise ValueError(f"{low16m} must be exactly 0x{LOW_SIZE:x} bytes")
    if l2 is not None and l2.stat().st_size != L2_SIZE:
        raise ValueError(f"{l2} must be exactly 0x{L2_SIZE:x} bytes")
    ddr = Path(args.ddr) if args.ddr is not None else None
    if ddr is not None and ddr.stat().st_size != DDR_SIZE:
        raise ValueError(f"{ddr} must be exactly 0x{DDR_SIZE:x} bytes")
    expected_output_hash = None
    if args.expected_output_hash is not None:
        expected_output_hash = int(args.expected_output_hash, 0)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    commands = parse_commands(args.trace)
    if args.mode == "full-sequence-delta":
        if expected_output_hash is not None:
            raise ValueError("--expected-output-hash is only supported with --mode last-command")
        if args.snapshot_dir is None:
            raise ValueError("--snapshot-dir is required with --mode full-sequence-delta")
        snapshot_dir = Path(args.snapshot_dir)
        include_ddr = snapshot_host_path(snapshot_dir, 1, "ddr64m").exists()
        if args.copy_snapshots:
            copy_delta_snapshots(snapshot_dir, out_dir, len(commands), include_ddr)
        capture = make_delta_sequence_capture(
            commands,
            snapshot_dir,
            args.guest_dir,
            include_ddr,
            include_run_checks=not args.no_run_checks,
            block_size=args.delta_block_size,
            compact_out_dir=out_dir if args.copy_snapshots else None,
        )
        stem = "yolov8n-full-sequence-delta"
    elif args.mode == "last-command":
        low_name = f"{args.snapshot_prefix}-low16m.bin"
        l2_name = f"{args.snapshot_prefix}-l2.bin"
        ddr_name = f"{args.snapshot_prefix}-ddr64m.bin"
        if args.copy_snapshots:
            shutil.copyfile(low16m, out_dir / low_name)
            shutil.copyfile(l2, out_dir / l2_name)
            if ddr is not None:
                shutil.copyfile(ddr, out_dir / ddr_name)

        guest_low = f"{args.guest_dir.rstrip('/')}/{low_name}"
        guest_l2 = f"{args.guest_dir.rstrip('/')}/{l2_name}"
        guest_ddr = f"{args.guest_dir.rstrip('/')}/{ddr_name}" if ddr is not None else None
        capture = make_capture(
            commands[-1],
            guest_low,
            guest_l2,
            guest_ddr=guest_ddr,
            expected_output_hash=expected_output_hash,
        )
        stem = "yolov8n-last-command"
    else:
        if expected_output_hash is not None:
            raise ValueError("--expected-output-hash is only supported with --mode last-command")
        low_name = f"{args.snapshot_prefix}-low16m.bin"
        l2_name = f"{args.snapshot_prefix}-l2.bin"
        ddr_name = f"{args.snapshot_prefix}-ddr64m.bin"
        if args.copy_snapshots:
            shutil.copyfile(low16m, out_dir / low_name)
            shutil.copyfile(l2, out_dir / l2_name)
            if ddr is not None:
                shutil.copyfile(ddr, out_dir / ddr_name)

        guest_low = f"{args.guest_dir.rstrip('/')}/{low_name}"
        guest_l2 = f"{args.guest_dir.rstrip('/')}/{l2_name}"
        guest_ddr = f"{args.guest_dir.rstrip('/')}/{ddr_name}" if ddr is not None else None
        capture = make_sequence_capture(
            commands,
            guest_low,
            guest_l2,
            guest_ddr=guest_ddr,
            include_run_checks=not args.no_run_checks,
        )
        stem = "yolov8n-full-sequence"

    capture_json = out_dir / f"{stem}.capture.json"
    with capture_json.open("w", encoding="utf-8") as file:
        json.dump(capture, file, indent=2)
        file.write("\n")

    krun = out_dir / f"{stem}.krun"
    converter = Path(__file__).with_name("capture-to-krun.py")
    subprocess.run([sys.executable, str(converter), str(capture_json), "-o", str(krun)], check=True)

    print(f"commands {len(commands)}")
    print(f"capture {capture_json}")
    print(f"krun {krun}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
