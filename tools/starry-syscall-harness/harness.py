#!/usr/bin/env python3
import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_IMAGE = "ghcr.io/rcore-os/tgoskits-container:latest"
PROBE_BEGIN = "STARRY_SYSCALL_PROBE_BEGIN"
PROBE_END = "STARRY_SYSCALL_PROBE_END"
CASE_RE = re.compile(r"^CASE\s+(?P<name>\S+)\s*(?P<body>.*)$")


@dataclass(frozen=True)
class ArchConfig:
    target: str
    cc: str
    to_bin: bool
    qemu_args: tuple[str, ...]


ARCHES = {
    "riscv64": ArchConfig(
        target="riscv64gc-unknown-none-elf",
        cc="riscv64-linux-musl-gcc",
        to_bin=True,
        qemu_args=("-nographic", "-cpu", "rv64"),
    ),
    "aarch64": ArchConfig(
        target="aarch64-unknown-none-softfloat",
        cc="aarch64-linux-musl-gcc",
        to_bin=True,
        qemu_args=("-nographic", "-cpu", "cortex-a53"),
    ),
    "loongarch64": ArchConfig(
        target="loongarch64-unknown-none-softfloat",
        cc="loongarch64-linux-musl-gcc",
        to_bin=True,
        qemu_args=("-machine", "virt", "-cpu", "la464", "-nographic", "-m", "128M"),
    ),
    "x86_64": ArchConfig(
        target="x86_64-unknown-none",
        cc="x86_64-linux-musl-gcc",
        to_bin=False,
        qemu_args=("-nographic",),
    ),
}


def repo_root_from(start: Path) -> Path:
    current = start.resolve()
    for candidate in (current, *current.parents):
        if (candidate / "Cargo.toml").exists() and (candidate / "os/StarryOS").exists():
            return candidate
    raise SystemExit(f"cannot find tgoskits repo root from {start}")


def script_repo_root() -> Path:
    return repo_root_from(Path(__file__).resolve())


def is_inside_docker() -> bool:
    return os.environ.get("STARRY_SYSCALL_HARNESS_IN_DOCKER") == "1" or Path("/.dockerenv").exists()


def run(
    cmd: list[str],
    *,
    cwd: Path,
    check: bool = True,
    capture: bool = False,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    print("+ " + " ".join(cmd), file=sys.stderr, flush=True)
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    result = subprocess.run(
        cmd,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        env=merged_env,
    )
    if check and result.returncode != 0:
        if capture:
            sys.stdout.write(result.stdout)
            sys.stderr.write(result.stderr)
        raise subprocess.CalledProcessError(result.returncode, cmd, result.stdout, result.stderr)
    return result


def docker_reexec(repo_root: Path, image: str, argv: list[str]) -> int:
    uid = os.getuid()
    gid = os.getgid()
    chown_paths = docker_chown_paths(argv)
    script = (
        'python3 tools/starry-syscall-harness/harness.py "$@"; '
        "status=$?; "
        f"chown -R {uid}:{gid} {' '.join(chown_paths)} 2>/dev/null || true; "
        "exit $status"
    )
    cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{repo_root}:/work",
        "-w",
        "/work",
        "-e",
        "STARRY_SYSCALL_HARNESS_IN_DOCKER=1",
        image,
        "bash",
        "-lc",
        script,
        "starry-harness",
    ]
    cmd.extend(argv)
    return run(cmd, cwd=repo_root, check=False).returncode


def docker_chown_paths(argv: list[str]) -> list[str]:
    paths = ["target/starry-syscall-harness", "tools/qperf/target"]
    for index, arg in enumerate(argv):
        value = None
        if arg == "--output-dir" and index + 1 < len(argv):
            value = argv[index + 1]
        elif arg.startswith("--output-dir="):
            value = arg.split("=", 1)[1]
        if value and is_safe_relative_path(value):
            paths.append(value)
    return [shlex.quote(path) for path in dict.fromkeys(paths)]


def is_safe_relative_path(value: str) -> bool:
    path = Path(value)
    return not path.is_absolute() and ".." not in path.parts


def docker_tool_output(repo_root: Path, image: str, command: str) -> subprocess.CompletedProcess[str]:
    return run(
        [
            "docker",
            "run",
            "--rm",
            "-v",
            f"{repo_root}:/work",
            "-w",
            "/work",
            image,
            "bash",
            "-lc",
            command,
        ],
        cwd=repo_root,
        check=False,
        capture=True,
    )


def command_exists(name: str) -> bool:
    return shutil.which(name) is not None


def doctor(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    checks: list[dict[str, Any]] = []

    docker_ok = command_exists("docker")
    checks.append({"name": "docker", "ok": docker_ok})
    if docker_ok:
        image_check = run(
            ["docker", "image", "inspect", args.image],
            cwd=repo_root,
            check=False,
            capture=True,
        )
        checks.append({"name": args.image, "ok": image_check.returncode == 0})
        tool_check = docker_tool_output(
            repo_root,
            args.image,
            "command -v debugfs && command -v riscv64-linux-musl-gcc && command -v qemu-system-riscv64 && cargo --version",
        )
        checks.append(
            {
                "name": "container-tools",
                "ok": tool_check.returncode == 0,
                "output": (tool_check.stdout + tool_check.stderr).strip(),
            }
        )

    report = {"repo_root": str(repo_root), "checks": checks}
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if all(item["ok"] for item in checks) else 1


def parse_cases(output: str) -> dict[str, dict[str, str]]:
    cases: dict[str, dict[str, str]] = {}
    for line in output.splitlines():
        match = CASE_RE.match(strip_ansi(line).strip())
        if not match:
            continue
        fields: dict[str, str] = {}
        for part in match.group("body").split():
            if "=" not in part:
                continue
            key, value = part.split("=", 1)
            fields[key] = value
        cases[match.group("name")] = fields
    return cases


def strip_ansi(text: str) -> str:
    return re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", text)


def compare_cases(
    linux_cases: dict[str, dict[str, str]],
    starry_cases: dict[str, dict[str, str]],
) -> list[dict[str, Any]]:
    differences: list[dict[str, Any]] = []
    for name in sorted(set(linux_cases) | set(starry_cases)):
        linux = linux_cases.get(name)
        starry = starry_cases.get(name)
        if linux != starry:
            differences.append({"case": name, "linux": linux, "starry": starry})
    return differences


PERF_RULES = [
    {
        "id": "virtio_vsock_locking",
        "patterns": ["vsock", "Vsock", "VSOCK"],
        "files": [
            "os/arceos/modules/axnet-ng/src/device/vsock.rs",
            "os/arceos/modules/axnet-ng/src/vsock/connection_manager.rs",
        ],
        "strategy": "Inspect global vsock device/connection-manager lock hold time; split TX/RX paths or move long work outside global locks.",
    },
    {
        "id": "virtio_net_shared_state",
        "patterns": ["virtio", "net", "transmit", "receive", "TxQueue", "RxQueue"],
        "files": [
            "platform/axplat-dyn/src/drivers/net/virtio_pci.rs",
            "platform/axplat-dyn/src/drivers/net/mod.rs",
        ],
        "strategy": "Inspect TX/RX shared locking and copy paths; prefer separate queue locks and reduce copies inside locked sections.",
    },
    {
        "id": "virtio_block_sync_queue",
        "patterns": ["virtio", "blk", "Block", "read_blocks", "write_blocks", "CmdQueue"],
        "files": [
            "platform/axplat-dyn/src/drivers/blk/virtio_pci.rs",
            "platform/axplat-dyn/src/drivers/blk/mod.rs",
        ],
        "strategy": "Inspect synchronous queue lock hold time; separate metadata access from blocking I/O and batch adjacent requests when possible.",
    },
    {
        "id": "lock_contention",
        "patterns": ["Mutex", "lock", "spin", "wait"],
        "files": ["components", "os/StarryOS", "os/arceos/modules"],
        "strategy": "Measure lock ownership scope around the hot function; move allocation/copy/syscall work outside the critical section.",
    },
    {
        "id": "copy_overhead",
        "patterns": ["copy", "memcpy", "copy_from_slice", "read_exact", "write_all"],
        "files": ["drivers", "platform", "os/StarryOS/kernel/src/syscall"],
        "strategy": "Inspect repeated buffer copies in the hot path; prefer scatter-gather buffers or one-copy handoff where the driver API allows it.",
    },
]

CATEGORY_RULES = [
    {
        "category": "virtqueue_add_notify_wait_pop",
        "axis": "bottleneck",
        "patterns": ["add_notify_wait_pop", "notify_wait_pop"],
    },
    {
        "category": "virtqueue_add",
        "axis": "bottleneck",
        "patterns": ["virtqueue::add", "add_buf", "receive_begin", "transmit_begin"],
    },
    {
        "category": "virtqueue_pop_complete",
        "axis": "bottleneck",
        "patterns": ["pop_used", "poll_used", "receive_complete", "transmit_complete", "complete"],
    },
    {
        "category": "virtio_notify_kick",
        "axis": "bottleneck",
        "patterns": ["notify", "kick", "should_notify", "queue_notify"],
    },
    {
        "category": "memcpy",
        "axis": "bottleneck",
        "patterns": ["memcpy", "copy_nonoverlapping", "copy_from_slice", "extend_from_slice"],
    },
    {
        "category": "memmove",
        "axis": "bottleneck",
        "patterns": ["memmove", "copy_within"],
    },
    {
        "category": "allocator",
        "axis": "bottleneck",
        "patterns": [
            "ax_alloc",
            "global_allocator",
            "__rust_alloc",
            "__rust_dealloc",
            "alloc_pages",
            "dealloc_pages",
            "raw_vec",
            "with_capacity",
        ],
    },
    {
        "category": "scheduler_wait_preempt",
        "axis": "bottleneck",
        "patterns": [
            "ax_task",
            "axtask",
            "run_queue",
            "yield_current",
            "resched",
            "waitqueue",
            "preempt",
            "schedule",
        ],
    },
    {
        "category": "lock_mutex_wait",
        "axis": "bottleneck",
        "patterns": [
            "mutex",
            "rwlock",
            "spinnoirq",
            "spinraw",
            "kspin",
            "lock_api",
            "try_lock",
            "kernel_guard",
            "irqsave",
        ],
    },
    {
        "category": "pci_probe_transport",
        "axis": "subsystem",
        "patterns": [
            "probe_pci",
            "take_virtio_transport",
            "virtio_pci::init",
            "virtio_pci::probe",
            "pciaddress",
            "read_config",
            "write_config",
            "config_space",
            "bar_info",
            "probe_bar",
        ],
    },
    {
        "category": "net_inflight_btree",
        "axis": "subsystem",
        "patterns": ["tx_inflight", "rx_inflight"],
        "all_patterns": [
            ["btree", "ax_driver::virtio::net"],
            ["btree", "virtionet"],
            ["btree", "virtio_net"],
        ],
    },
    {
        "category": "block_io_path",
        "axis": "subsystem",
        "patterns": ["virtio_blk", "virtioblk", "rd_block", "blockqueue", "read_blocks", "write_blocks", "rsext4", "axfs"],
    },
    {
        "category": "net_rx_tx_path",
        "axis": "subsystem",
        "patterns": ["virtio_net", "virtionet", "axnet", "smoltcp", "rxqueue", "txqueue", "transmit", "receive"],
    },
    {
        "category": "vsock_tx_rx_path",
        "axis": "subsystem",
        "patterns": ["vsock", "virtiosocket", "rdif_vsock"],
    },
]


def parse_kv_summary(path: Path) -> dict[str, str]:
    data: dict[str, str] = {}
    if not path.exists():
        return data
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        data[key.strip()] = value.strip()
    return data


def parse_gnu_time_metrics(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    raw: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        raw[normalize_metric_key(key)] = value.strip()
    metrics: dict[str, Any] = {"raw": raw}
    numeric_keys = {
        "user_time": "user_seconds",
        "system_time": "system_seconds",
        "percent_of_cpu_this_job_got": "cpu_percent",
        "maximum_resident_set_size": "max_rss_kb",
        "major_page_faults": "major_page_faults",
        "minor_page_faults": "minor_page_faults",
        "voluntary_context_switches": "voluntary_context_switches",
        "involuntary_context_switches": "involuntary_context_switches",
        "file_system_inputs": "file_system_inputs",
        "file_system_outputs": "file_system_outputs",
        "socket_messages_sent": "socket_messages_sent",
        "socket_messages_received": "socket_messages_received",
        "signals_delivered": "signals_delivered",
        "page_size": "page_size_bytes",
        "exit_status": "exit_status",
    }
    for raw_key, metric_key in numeric_keys.items():
        if raw_key in raw:
            metrics[metric_key] = parse_metric_number(raw[raw_key])
    elapsed = raw.get("elapsed_time")
    if elapsed:
        metrics["elapsed_seconds"] = parse_elapsed_seconds(elapsed)
    return metrics


def normalize_metric_key(value: str) -> str:
    value = value.strip().lower()
    value = re.sub(r"\([^)]*\)", "", value)
    value = re.sub(r"[^a-z0-9]+", "_", value).strip("_")
    return value


def parse_metric_number(value: str) -> int | float | str:
    text = value.strip().replace(",", "")
    if text.endswith("%"):
        text = text[:-1]
    try:
        if "." in text:
            return float(text)
        return int(text)
    except ValueError:
        return value


def parse_elapsed_seconds(value: str) -> float | None:
    text = value.strip()
    try:
        parts = [float(part) for part in text.split(":")]
    except ValueError:
        return None
    if len(parts) == 3:
        hours, minutes, seconds = parts
        return hours * 3600 + minutes * 60 + seconds
    if len(parts) == 2:
        minutes, seconds = parts
        return minutes * 60 + seconds
    if len(parts) == 1:
        return parts[0]
    return None


def parse_perf_stat_metrics(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    events: dict[str, dict[str, Any]] = {}
    errors: list[str] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("#"):
            errors.append(line.lstrip("# "))
            continue
        parts = line.split(",")
        if len(parts) < 3:
            errors.append(line)
            continue
        value_text, unit, event = parts[0].strip(), parts[1].strip(), parts[2].strip()
        if not event:
            errors.append(line)
            continue
        value = None if value_text in {"", "<not supported>", "<not counted>"} else parse_metric_number(value_text)
        events[event] = {
            "value": value,
            "unit": unit,
            "raw": line,
        }
    return {
        "scope": "host_qemu_process",
        "note": "perf stat measures the host QEMU process and wrapper overhead; it is not a guest PMU counter.",
        "events": events,
        "errors": errors,
    }


def load_json_file(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8", errors="replace"))
    except json.JSONDecodeError as err:
        return {"error": f"failed to parse {path}: {err}"}


def parse_folded(path: Path, limit: int = 20) -> dict[str, Any]:
    function_counts: dict[str, int] = {}
    stack_counts: dict[str, int] = {}
    category_counts: dict[str, int] = {}
    category_stacks: dict[str, int] = {}
    category_functions: dict[str, dict[str, int]] = {}
    total = 0
    if not path.exists():
        return {
            "total_samples": 0,
            "top_functions": [],
            "top_stacks": [],
            "category_totals": [],
        }
    for raw_line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            stack, count_text = line.rsplit(" ", 1)
            count = int(count_text)
        except ValueError:
            stack = line
            count = 1
        total += count
        stack_counts[stack] = stack_counts.get(stack, 0) + count
        functions = [function for function in stack.split(";") if function]
        matched_categories = classify_stack(functions)
        for category in matched_categories:
            category_counts[category] = category_counts.get(category, 0) + count
            category_stacks[category] = category_stacks.get(category, 0) + 1
            per_function = category_functions.setdefault(category, {})
            for function in functions:
                if category_matches_function(category, function):
                    per_function[function] = per_function.get(function, 0) + count
        for function in functions:
            if function:
                function_counts[function] = function_counts.get(function, 0) + count

    def entries(counts: dict[str, int], label: str) -> list[dict[str, Any]]:
        ranked = sorted(counts.items(), key=lambda item: (-item[1], item[0]))[:limit]
        return [
            {
                label: name,
                "samples": count,
                "percent": round((count / total * 100.0) if total else 0.0, 4),
            }
            for name, count in ranked
        ]

    return {
        "total_samples": total,
        "top_functions": entries(function_counts, "function"),
        "top_stacks": entries(stack_counts, "stack"),
        "category_totals": category_entries(
            category_counts,
            category_stacks,
            category_functions,
            total,
            limit,
        ),
    }


def normalize_symbol_for_category(value: str) -> str:
    value = strip_ansi(value).lower()
    value = re.sub(r"\+0x[0-9a-f]+", "", value)
    return value


def category_matches_function(category: str, function: str) -> bool:
    normalized = normalize_symbol_for_category(function)
    for rule in CATEGORY_RULES:
        if rule["category"] == category:
            return rule_matches_text(rule, normalized)
    return False


def classify_stack(functions: list[str]) -> list[str]:
    normalized = "\n".join(normalize_symbol_for_category(function) for function in functions)
    matched = []
    for rule in CATEGORY_RULES:
        if rule_matches_text(rule, normalized):
            matched.append(rule["category"])
    return matched


def rule_matches_text(rule: dict[str, Any], text: str) -> bool:
    if any(pattern in text for pattern in rule.get("patterns", [])):
        return True
    for group in rule.get("all_patterns", []):
        if all(pattern in text for pattern in group):
            return True
    return False


def category_entries(
    counts: dict[str, int],
    stacks: dict[str, int],
    functions: dict[str, dict[str, int]],
    total: int,
    limit: int,
) -> list[dict[str, Any]]:
    axis_by_category = {rule["category"]: rule["axis"] for rule in CATEGORY_RULES}
    items = []
    for rule in CATEGORY_RULES:
        category = rule["category"]
        count = counts.get(category, 0)
        top_functions = sorted(
            functions.get(category, {}).items(),
            key=lambda item: (-item[1], item[0]),
        )[: min(limit, 10)]
        items.append(
            {
                "category": category,
                "axis": axis_by_category.get(category, "unknown"),
                "mode": "inclusive_stack",
                "samples": count,
                "percent": round((count / total * 100.0) if total else 0.0, 4),
                "matched_stacks": stacks.get(category, 0),
                "top_functions": [
                    {
                        "function": name,
                        "samples": samples,
                        "percent": round((samples / total * 100.0) if total else 0.0, 4),
                    }
                    for name, samples in top_functions
                ],
            }
        )
    return sorted(items, key=lambda item: (-item["samples"], item["category"]))


def parse_workload_stdout_metrics(output: str) -> dict[str, Any]:
    text = strip_ansi(output).replace("\r", "\n")
    metrics: dict[str, Any] = {
        "custom": [],
        "values": {},
        "dd": [],
        "wget": [],
        "raw_metric_lines": [],
    }
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    for line in lines:
        if line.startswith("QPERF_METRIC"):
            parsed = parse_qperf_metric_line(line)
            metrics["raw_metric_lines"].append(line)
            if parsed:
                metrics["custom"].append(parsed)
                for key, value in parsed.get("fields", {}).items():
                    metrics["values"][key] = value
            continue
        if line.startswith("QPERF_BEGIN") or line.startswith("QPERF_END"):
            continue
        dd = parse_dd_output_line(line)
        if dd:
            metrics["dd"].append(dd)
        update_wget_metrics(metrics, line)
    return metrics


def parse_qperf_metric_line(line: str) -> dict[str, Any]:
    try:
        parts = shlex.split(line)
    except ValueError:
        parts = line.split()
    if not parts or parts[0] != "QPERF_METRIC":
        return {}
    fields: dict[str, Any] = {}
    labels: list[str] = []
    for item in parts[1:]:
        if "=" not in item:
            labels.append(item)
            continue
        key, value = item.split("=", 1)
        fields[normalize_metric_key(key)] = parse_metric_number(value)
    return {"labels": labels, "fields": fields, "raw": line}


def parse_dd_output_line(line: str) -> dict[str, Any] | None:
    match = re.search(
        r"(?P<bytes>[\d,]+)\s+bytes\b.*?\bcopied,\s*"
        r"(?P<seconds>[0-9.]+)\s*s(?:ec(?:onds?)?)?,\s*"
        r"(?P<rate>[0-9.]+)\s*(?P<unit>[KMGT]?i?B|[KMGT]?B)/s",
        line,
        re.IGNORECASE,
    )
    if not match:
        return None
    byte_count = int(match.group("bytes").replace(",", ""))
    seconds = float(match.group("seconds"))
    return {
        "bytes": byte_count,
        "elapsed_seconds": seconds,
        "throughput_bytes_per_second": byte_count / seconds if seconds > 0 else None,
        "reported_rate": float(match.group("rate")),
        "reported_rate_unit": match.group("unit") + "/s",
        "raw": line,
    }


def update_wget_metrics(metrics: dict[str, Any], line: str) -> None:
    if "wget" not in metrics:
        metrics["wget"] = []
    current = metrics["wget"][-1] if metrics["wget"] else None
    if "Length:" in line:
        match = re.search(r"Length:\s*([\d,]+)", line)
        if match:
            current = {"raw_lines": []}
            current["bytes"] = int(match.group(1).replace(",", ""))
            metrics["wget"].append(current)
    lower_line = line.lower()
    is_wget_line = (
        "connecting to " in lower_line
        or "saving to " in lower_line
        or ("%" in line and "|" in line)
        or " saved " in lower_line
        or lower_line.endswith(" saved")
        or "saved [" in lower_line
    )
    if current is None and is_wget_line:
        current = {"raw_lines": []}
        metrics["wget"].append(current)
    if current is None:
        return
    if current.get("saved") and not is_wget_line:
        return
    current.setdefault("raw_lines", []).append(line)
    saved = re.search(r"saved\s+\[([\d,]+)(?:/[\d,]+)?\]", line, re.IGNORECASE)
    if saved:
        current["saved"] = True
        current["saved_bytes"] = int(saved.group(1).replace(",", ""))
        current.setdefault("bytes", current["saved_bytes"])
    elif " saved" in lower_line:
        current["saved"] = True
    progress = re.search(
        r"(?<!\d)100%.*\|\s*(?P<size>[0-9.]+)\s*(?P<unit>[KMGTkmgt]?)(?:i?B|B)?\b",
        line,
    )
    if progress:
        current["progress_size"] = progress.group("size") + progress.group("unit")
        current.setdefault(
            "bytes",
            byte_size_to_int(float(progress.group("size")), progress.group("unit")),
        )
    elapsed = re.search(r"\bin\s+([0-9.]+)\s*([smh]?)\b", line)
    if elapsed:
        current["elapsed_seconds"] = elapsed_to_seconds(float(elapsed.group(1)), elapsed.group(2))
    rate = re.search(r"\(([0-9.]+)\s*([KMGT]?i?B|[KMGT]?B)/s\)", line, re.IGNORECASE)
    if rate:
        current["reported_rate"] = float(rate.group(1))
        current["reported_rate_unit"] = rate.group(2) + "/s"
    if current.get("bytes") is not None and current.get("elapsed_seconds"):
        current["throughput_bytes_per_second"] = current["bytes"] / current["elapsed_seconds"]


def byte_size_to_int(value: float, unit: str) -> int:
    match unit.lower():
        case "k":
            factor = 1024
        case "m":
            factor = 1024**2
        case "g":
            factor = 1024**3
        case "t":
            factor = 1024**4
        case _:
            factor = 1
    return int(value * factor)


def elapsed_to_seconds(value: float, unit: str) -> float:
    match unit.lower():
        case "h":
            return value * 3600.0
        case "m":
            return value * 60.0
        case _:
            return value


def workload_bytes(metrics: dict[str, Any]) -> int | None:
    for dd in reversed(metrics.get("dd", [])):
        if dd.get("bytes"):
            return int(dd["bytes"])
    for wget in reversed(metrics.get("wget", [])):
        if wget.get("bytes"):
            return int(wget["bytes"])
        if wget.get("saved_bytes"):
            return int(wget["saved_bytes"])
    values = metrics.get("values") or {}
    for key in ("workload_bytes", "bytes", "total_bytes"):
        value = values.get(key)
        if isinstance(value, (int, float)):
            return int(value)
    return None


def workload_elapsed_seconds(metrics: dict[str, Any], window: dict[str, Any]) -> float | None:
    for dd in reversed(metrics.get("dd", [])):
        if dd.get("elapsed_seconds") is not None:
            return float(dd["elapsed_seconds"])
    for wget in reversed(metrics.get("wget", [])):
        if wget.get("elapsed_seconds") is not None:
            return float(wget["elapsed_seconds"])
    values = metrics.get("values") or {}
    for key in ("workload_elapsed_seconds", "elapsed_seconds", "seconds"):
        value = values.get(key)
        if isinstance(value, (int, float)):
            return float(value)
    if isinstance(window.get("duration_sec"), (int, float)):
        return float(window["duration_sec"])
    return None


def complete_workload_metrics(metrics: dict[str, Any], window: dict[str, Any]) -> None:
    window_elapsed = window.get("duration_sec")
    if not isinstance(window_elapsed, (int, float)) or window_elapsed <= 0:
        return
    for wget in metrics.get("wget", []):
        if wget.get("elapsed_seconds") is None and wget.get("saved") and wget.get("bytes"):
            wget["elapsed_seconds"] = float(window_elapsed)
            wget["elapsed_source"] = "marker_window"
        if wget.get("throughput_bytes_per_second") is None and wget.get("bytes") and wget.get("elapsed_seconds"):
            wget["throughput_bytes_per_second"] = wget["bytes"] / wget["elapsed_seconds"]


def perf_fix_candidates(top_functions: list[dict[str, Any]], min_percent: float) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    seen: set[str] = set()
    for item in top_functions:
        function = item["function"]
        percent = float(item["percent"])
        if percent < min_percent:
            continue
        for rule in PERF_RULES:
            if rule["id"] in seen:
                continue
            if any(pattern in function for pattern in rule["patterns"]):
                seen.add(rule["id"])
                candidates.append(
                    {
                        "id": rule["id"],
                        "trigger": function,
                        "samples": item["samples"],
                        "percent": percent,
                        "files": rule["files"],
                        "strategy": rule["strategy"],
                    }
                )
                break
    return candidates


def numeric_metric(data: dict[str, Any], key: str) -> int | float | None:
    value = data.get(key)
    if isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        parsed = parse_metric_number(value)
        if isinstance(parsed, (int, float)):
            return parsed
    return None


def compute_normalized_metrics(
    hotspots: dict[str, Any],
    plugin_summary: dict[str, str],
    host_time_metrics: dict[str, Any],
    host_perf_metrics: dict[str, Any],
    workload_metrics: dict[str, Any],
    window: dict[str, Any],
) -> dict[str, Any]:
    byte_count = workload_bytes(workload_metrics)
    mb = (byte_count / 1_000_000.0) if byte_count else None
    elapsed = workload_elapsed_seconds(workload_metrics, window)
    host_elapsed = numeric_metric(host_time_metrics, "elapsed_seconds")
    executed_instructions = numeric_metric(plugin_summary, "executed_instructions")
    executed_blocks = numeric_metric(plugin_summary, "executed_blocks")
    total_samples = hotspots.get("total_samples") or 0
    normalized: dict[str, Any] = {
        "workload_bytes": byte_count,
        "workload_elapsed_seconds": elapsed,
        "guest_instructions_per_MB": divide_or_none(executed_instructions, mb),
        "guest_blocks_per_MB": divide_or_none(executed_blocks, mb),
        "host_elapsed_sec_per_MB": divide_or_none(host_elapsed, mb),
        "samples_per_MB": divide_or_none(total_samples, mb),
        "category_samples_per_MB": {
            item["category"]: divide_or_none(item.get("samples"), mb)
            for item in hotspots.get("category_totals", [])
        },
    }
    if elapsed:
        normalized["samples_per_second"] = divide_or_none(total_samples, elapsed)
    host_events = (host_perf_metrics.get("events") or {}) if isinstance(host_perf_metrics, dict) else {}
    cycles = host_event_value(host_events, "cycles")
    instructions = host_event_value(host_events, "instructions")
    cache_misses = host_event_value(host_events, "cache-misses")
    cache_refs = host_event_value(host_events, "cache-references")
    normalized["host_ipc"] = divide_or_none(instructions, cycles)
    normalized["host_cycles_per_sample"] = divide_or_none(cycles, total_samples)
    normalized["host_instructions_per_sample"] = divide_or_none(instructions, total_samples)
    normalized["host_cache_miss_percent"] = percent_or_none(cache_misses, cache_refs)
    return normalized


def divide_or_none(numerator: Any, denominator: Any) -> float | None:
    if not isinstance(numerator, (int, float)) or not isinstance(denominator, (int, float)):
        return None
    if denominator == 0:
        return None
    return round(numerator / denominator, 6)


def percent_or_none(numerator: Any, denominator: Any) -> float | None:
    value = divide_or_none(numerator, denominator)
    return round(value * 100.0, 6) if value is not None else None


def host_event_value(events: dict[str, Any], name: str) -> int | float | None:
    event = events.get(name) or {}
    value = event.get("value")
    return value if isinstance(value, (int, float)) else None


def write_hotspots_csv(path: Path, hotspots: dict[str, Any]) -> None:
    lines = ["kind,name,samples,percent"]
    for item in hotspots["top_functions"]:
        lines.append(f"function,{json.dumps(item['function'])},{item['samples']},{item['percent']}")
    for item in hotspots["top_stacks"]:
        lines.append(f"stack,{json.dumps(item['stack'])},{item['samples']},{item['percent']}")
    write_text(path, "\n".join(lines) + "\n")


def write_hotspot_categories_csv(path: Path, hotspots: dict[str, Any]) -> None:
    lines = ["category,axis,mode,samples,percent,matched_stacks,top_function"]
    for item in hotspots.get("category_totals", []):
        top_function = ""
        if item.get("top_functions"):
            top_function = item["top_functions"][0]["function"]
        lines.append(
            ",".join(
                [
                    json.dumps(item["category"]),
                    json.dumps(item["axis"]),
                    json.dumps(item["mode"]),
                    str(item["samples"]),
                    str(item["percent"]),
                    str(item["matched_stacks"]),
                    json.dumps(top_function),
                ]
            )
        )
    write_text(path, "\n".join(lines) + "\n")


def write_perf_markdown(path: Path, report: dict[str, Any]) -> None:
    host_time = report.get("host_time_metrics") or {}
    host_perf = report.get("host_perf_metrics") or {}
    host_perf_events = (host_perf.get("events") or {}) if isinstance(host_perf, dict) else {}
    workload = report.get("workload_metrics") or {}
    normalized = report.get("normalized_metrics") or {}
    window = report.get("window") or {}
    lines = [
        "# StarryOS qperf Performance Report",
        "",
        f"- arch: `{report['arch']}`",
        f"- result: `{report['result']}`",
        f"- samples: `{report['hotspots']['total_samples']}`",
        f"- artifacts: `{report['artifacts']['work_dir']}`",
    ]
    if host_time.get("elapsed_seconds") is not None:
        lines.append(f"- host elapsed seconds: `{host_time['elapsed_seconds']}`")
    if host_time.get("user_seconds") is not None:
        lines.append(f"- host user seconds: `{host_time['user_seconds']}`")
    if host_time.get("system_seconds") is not None:
        lines.append(f"- host system seconds: `{host_time['system_seconds']}`")
    if window.get("enabled"):
        lines.append(f"- workload window: `{window.get('start_time')}` -> `{window.get('stop_time')}`")
        if window.get("truncated_by_timeout"):
            lines.append("- workload window truncated by timeout: `true`")
    if host_perf_events:
        lines.extend(
            [
                "- host perf scope: `host_qemu_process`",
                "- host perf note: `host perf stat measures the host QEMU process, not guest PMU counters`",
            ]
        )
        for event_name in ("task-clock", "cycles", "instructions", "cache-references", "cache-misses"):
            event = host_perf_events.get(event_name)
            if event and event.get("value") is not None:
                unit = f" {event['unit']}" if event.get("unit") else ""
                lines.append(f"- host {event_name}: `{event['value']}{unit}`")
    elif not report.get("parameters", {}).get("host_perf"):
        lines.append("- host perf: `未启用 host perf`")
    if normalized.get("workload_bytes") is not None:
        lines.append(f"- workload bytes: `{normalized['workload_bytes']}`")
    if normalized.get("workload_elapsed_seconds") is not None:
        lines.append(f"- workload elapsed seconds: `{normalized['workload_elapsed_seconds']}`")
    if normalized.get("samples_per_MB") is not None:
        lines.append(f"- samples per MB: `{normalized['samples_per_MB']}`")
    lines.extend(
        [
            "",
            "## Workload Metrics",
            "",
        ]
    )
    if workload.get("dd") or workload.get("wget") or workload.get("custom"):
        if workload.get("dd"):
            for item in workload["dd"]:
                lines.append(
                    f"- dd: `{item.get('bytes')}` bytes, `{item.get('elapsed_seconds')}` sec, "
                    f"`{item.get('throughput_bytes_per_second')}` B/s"
                )
        if workload.get("wget"):
            for item in workload["wget"]:
                lines.append(
                    f"- wget: `{item.get('bytes') or item.get('saved_bytes')}` bytes, "
                    f"saved=`{item.get('saved')}`, elapsed=`{item.get('elapsed_seconds')}` sec"
                )
        if workload.get("values"):
            preview = ", ".join(f"{key}={value}" for key, value in sorted(workload["values"].items())[:12])
            lines.append(f"- QPERF_METRIC: `{preview}`")
    else:
        lines.append("No workload stdout metrics were parsed.")
    lines.extend(
        [
            "",
            "## Hotspot Categories",
            "",
            "| Category | Axis | Samples | Percent |",
            "|---|---|---:|---:|",
        ]
    )
    for item in report["hotspots"].get("category_totals", []):
        lines.append(
            f"| `{item['category']}` | `{item['axis']}` | {item['samples']} | {item['percent']:.2f}% |"
        )
    lines.extend(
        [
            "",
            "## Top Functions",
            "",
            "| Function | Samples | Percent |",
            "|---|---:|---:|",
        ]
    )
    for item in report["hotspots"]["top_functions"][:20]:
        lines.append(f"| `{item['function']}` | {item['samples']} | {item['percent']:.2f}% |")
    lines.extend(["", "## Fix Candidates", ""])
    candidates = report.get("fix_candidates", [])
    if candidates:
        for candidate in candidates:
            files = ", ".join(f"`{item}`" for item in candidate["files"])
            lines.extend(
                [
                    f"### {candidate['id']}",
                    "",
                    f"- trigger: `{candidate['trigger']}`",
                    f"- percent: `{candidate['percent']:.2f}%`",
                    f"- files: {files}",
                    f"- strategy: {candidate['strategy']}",
                    "",
                ]
            )
    else:
        lines.append("No rule-based bottleneck fix candidates crossed the configured threshold.")
    write_text(path, "\n".join(lines) + "\n")


def perf_work_dir(repo_root: Path, output_dir: str, arch: str) -> Path:
    return repo_root / output_dir / "perf" / arch / "latest"


def perf_profile_inside(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    work_dir = perf_work_dir(repo_root, args.output_dir, args.arch)
    if work_dir.exists():
        shutil.rmtree(work_dir)
    qperf_dir = work_dir / "qperf"
    qperf_dir.mkdir(parents=True, exist_ok=True)

    ensure_starry_qemu_defconfig(repo_root, args.arch)
    command = [
        "cargo",
        "xtask",
        "starry",
        "perf",
        "--arch",
        args.arch,
        "--timeout",
        str(args.timeout),
        "--format",
        args.format,
        "--freq",
        str(args.freq),
        "--max-depth",
        str(args.max_depth),
        "--mode",
        args.mode,
        "--top",
        str(args.top),
        "--out",
        str(qperf_dir),
    ]
    if args.debug:
        command.append("--debug")
    if args.kernel_filter:
        command.append("--kernel-filter")
    if args.host_time:
        command.append("--host-time")
    if args.host_perf:
        command.append("--host-perf")
        command.extend(["--host-perf-events", args.host_perf_events])
    if args.shell_init_cmd:
        command.extend(["--shell-init-cmd", args.shell_init_cmd])
    if args.shell_prefix:
        command.extend(["--shell-prefix", args.shell_prefix])
    if args.start_marker:
        command.extend(["--start-marker", args.start_marker])
    if args.stop_marker:
        command.extend(["--stop-marker", args.stop_marker])
    if args.workload_timeout is not None:
        command.extend(["--workload-timeout", str(args.workload_timeout)])
    if args.qperf_metrics:
        command.append("--qperf-metrics")
    for qemu_arg in args.qemu_arg:
        command.append(f"--qemu-arg={qemu_arg}")
    run_result = run(command, cwd=repo_root, check=False, capture=True)
    write_text(work_dir / "profile.stdout", run_result.stdout)
    write_text(work_dir / "profile.stderr", run_result.stderr)

    folded = qperf_dir / "stack.folded"
    hotspots = parse_folded(folded, args.top)
    summary = parse_kv_summary(qperf_dir / "summary.txt")
    plugin_summary = parse_kv_summary(qperf_dir / "qperf.summary.txt")
    host_time_metrics = parse_gnu_time_metrics(qperf_dir / "qemu.time.txt")
    host_perf_metrics = parse_perf_stat_metrics(qperf_dir / "qemu.perf.csv") if args.host_perf else {
        "enabled": False,
        "note": "未启用 host perf",
        "events": {},
        "errors": [],
    }
    if args.host_perf:
        host_perf_metrics["enabled"] = True
    workload_metrics = parse_workload_stdout_metrics(run_result.stdout)
    window = load_json_file(qperf_dir / "window.json")
    if not window:
        window = {
            "enabled": bool(args.start_marker or args.stop_marker or args.workload_timeout),
            "start_marker": args.start_marker,
            "stop_marker": args.stop_marker,
            "start_time": None,
            "stop_time": None,
            "duration_sec": None,
            "workload_timeout": args.workload_timeout,
            "truncated_by_timeout": False,
            "boot_samples_excluded": None,
            "warnings": [],
            "method": "qperf_raw_elapsed_timestamp_filter" if (args.start_marker or args.stop_marker) else "disabled",
        }
    resolve_stats = load_json_file(qperf_dir / "resolve.stats.json")
    if resolve_stats:
        window["boot_samples_excluded"] = resolve_stats.get("samples_excluded_before")
        window["post_window_samples_excluded"] = resolve_stats.get("samples_excluded_after")
        if resolve_stats.get("warning"):
            window.setdefault("warnings", []).append(resolve_stats["warning"])
    if args.start_marker and not window.get("start_time"):
        window.setdefault("warnings", []).append("start marker missing; report may include boot samples")
    if args.stop_marker and not window.get("stop_time"):
        window.setdefault("warnings", []).append("stop marker missing; report may include post-workload samples")
    complete_workload_metrics(workload_metrics, window)
    normalized_metrics = compute_normalized_metrics(
        hotspots,
        plugin_summary,
        host_time_metrics,
        host_perf_metrics,
        workload_metrics,
        window,
    )
    candidates = perf_fix_candidates(hotspots["top_functions"], args.min_percent)
    result = "ok" if run_result.returncode == 0 and hotspots["total_samples"] > 0 else "incomplete"
    report = {
        "arch": args.arch,
        "result": result,
        "returncode": run_result.returncode,
        "parameters": {
            "timeout": args.timeout,
            "format": args.format,
            "freq": args.freq,
            "max_depth": args.max_depth,
            "mode": args.mode,
            "top": args.top,
            "min_percent": args.min_percent,
            "debug": args.debug,
            "kernel_filter": args.kernel_filter,
            "host_time": args.host_time,
            "host_perf": args.host_perf,
            "host_perf_events": args.host_perf_events,
            "shell_init_cmd": args.shell_init_cmd,
            "shell_prefix": args.shell_prefix,
            "start_marker": args.start_marker,
            "stop_marker": args.stop_marker,
            "workload_timeout": args.workload_timeout,
            "qperf_metrics": args.qperf_metrics,
            "qemu_args": args.qemu_arg,
        },
        "window": window,
        "resolve_stats": resolve_stats,
        "hotspots": hotspots,
        "summary": summary,
        "plugin_summary": plugin_summary,
        "host_time_metrics": host_time_metrics,
        "host_perf_metrics": host_perf_metrics,
        "workload_metrics": workload_metrics,
        "normalized_metrics": normalized_metrics,
        "fix_candidates": candidates,
        "linux_alignment": {
            "status": "baseline_required",
            "note": "Provide a Linux workload baseline or a previous optimized folded stack to perf-diff for quantitative alignment.",
        },
        "artifacts": {
            "work_dir": str(work_dir),
            "qperf_dir": str(qperf_dir),
            "report": str(work_dir / "report.json"),
            "markdown": str(work_dir / "report.md"),
            "hotspots_csv": str(work_dir / "hotspots.csv"),
            "hotspot_categories_csv": str(work_dir / "hotspot_categories.csv"),
            "profile_stdout": str(work_dir / "profile.stdout"),
            "profile_stderr": str(work_dir / "profile.stderr"),
            "raw_samples": str(qperf_dir / "qperf.bin"),
            "folded": str(folded),
            "flamegraph": str(qperf_dir / "flamegraph.svg"),
            "summary_txt": str(qperf_dir / "summary.txt"),
            "plugin_summary": str(qperf_dir / "qperf.summary.txt"),
            "qemu_config": str(qperf_dir / "qemu.toml"),
            "host_time": str(qperf_dir / "qemu.time.txt"),
            "host_perf": str(qperf_dir / "qemu.perf.csv"),
            "window": str(qperf_dir / "window.json"),
            "resolve_stats": str(qperf_dir / "resolve.stats.json"),
        },
    }
    write_text(work_dir / "report.json", json.dumps(report, indent=2, sort_keys=True))
    write_perf_markdown(work_dir / "report.md", report)
    write_hotspots_csv(work_dir / "hotspots.csv", hotspots)
    write_hotspot_categories_csv(work_dir / "hotspot_categories.csv", hotspots)
    print_perf_summary(report)
    return run_result.returncode


def print_perf_summary(report: dict[str, Any]) -> None:
    print(f"arch: {report['arch']}")
    print(f"artifacts: {report['artifacts']['work_dir']}")
    print(f"result: {report['result']}")
    print(f"samples: {report['hotspots']['total_samples']}")
    host_time = report.get("host_time_metrics") or {}
    if host_time.get("elapsed_seconds") is not None:
        print(f"host elapsed: {host_time['elapsed_seconds']}s")
    host_perf = report.get("host_perf_metrics") or {}
    perf_events = host_perf.get("events") or {}
    if perf_events:
        preview = []
        for name in ("task-clock", "cycles", "instructions", "cache-misses"):
            event = perf_events.get(name)
            if event and event.get("value") is not None:
                preview.append(f"{name}={event['value']}")
        if preview:
            print("host perf: " + ", ".join(preview))
    for item in report["hotspots"]["top_functions"][:5]:
        print(f"- {item['percent']:.2f}% {item['function']}")
    if report["fix_candidates"]:
        print("fix candidates:")
        for candidate in report["fix_candidates"]:
            print(f"- {candidate['id']}: {candidate['trigger']} ({candidate['percent']:.2f}%)")


def perf_profile(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    if not args.no_docker and not is_inside_docker():
        forwarded = [
            "perf-profile",
            "--repo-root",
            "/work",
            "--arch",
            args.arch,
            "--timeout",
            str(args.timeout),
            "--format",
            args.format,
            "--freq",
            str(args.freq),
            "--max-depth",
            str(args.max_depth),
            "--mode",
            args.mode,
            "--top",
            str(args.top),
            "--min-percent",
            str(args.min_percent),
            "--output-dir",
            args.output_dir,
            "--no-docker",
        ]
        if args.debug:
            forwarded.append("--debug")
        if args.kernel_filter:
            forwarded.append("--kernel-filter")
        if args.host_time:
            forwarded.append("--host-time")
        if args.host_perf:
            forwarded.append("--host-perf")
            forwarded.extend(["--host-perf-events", args.host_perf_events])
        if args.shell_init_cmd:
            forwarded.extend(["--shell-init-cmd", args.shell_init_cmd])
        if args.shell_prefix:
            forwarded.extend(["--shell-prefix", args.shell_prefix])
        if args.start_marker:
            forwarded.extend(["--start-marker", args.start_marker])
        if args.stop_marker:
            forwarded.extend(["--stop-marker", args.stop_marker])
        if args.workload_timeout is not None:
            forwarded.extend(["--workload-timeout", str(args.workload_timeout)])
        if args.qperf_metrics:
            forwarded.append("--qperf-metrics")
        for qemu_arg in args.qemu_arg:
            forwarded.append(f"--qemu-arg={qemu_arg}")
        return docker_reexec(repo_root, args.image, forwarded)
    return perf_profile_inside(args)


def resolve_folded_path(path: Path) -> Path:
    if path.is_file():
        return path
    candidates = [
        path / "stack.folded",
        path / "qperf" / "stack.folded",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    raise FileNotFoundError(f"cannot find stack.folded under {path}")


def perf_diff(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    baseline = resolve_folded_path(Path(args.baseline))
    compare = resolve_folded_path(Path(args.compare))
    baseline_hotspots = parse_folded(baseline, args.top)
    compare_hotspots = parse_folded(compare, args.top)

    baseline_counts = {item["function"]: item for item in baseline_hotspots["top_functions"]}
    compare_counts = {item["function"]: item for item in compare_hotspots["top_functions"]}
    changes: list[dict[str, Any]] = []
    for name in sorted(set(baseline_counts) | set(compare_counts)):
        base = baseline_counts.get(name, {"samples": 0, "percent": 0.0})
        comp = compare_counts.get(name, {"samples": 0, "percent": 0.0})
        changes.append(
            {
                "function": name,
                "baseline_percent": base["percent"],
                "compare_percent": comp["percent"],
                "delta_percent": round(comp["percent"] - base["percent"], 4),
                "baseline_samples": base["samples"],
                "compare_samples": comp["samples"],
            }
        )
    changes.sort(key=lambda item: abs(item["delta_percent"]), reverse=True)

    out_dir = repo_root / args.output_dir / "perf-diff"
    out_dir.mkdir(parents=True, exist_ok=True)
    report = {
        "baseline": str(baseline),
        "compare": str(compare),
        "top_changes": changes[: args.top],
        "artifacts": {
            "work_dir": str(out_dir),
            "report": str(out_dir / "report.json"),
        },
    }
    write_text(out_dir / "report.json", json.dumps(report, indent=2, sort_keys=True))
    print(f"artifacts: {out_dir}")
    for item in report["top_changes"][:10]:
        print(
            f"- {item['delta_percent']:+.2f}% {item['function']} "
            f"({item['baseline_percent']:.2f}% -> {item['compare_percent']:.2f}%)"
        )
    return 0


def resolve_perf_report_input(path: Path) -> dict[str, Any]:
    source = path
    report_path: Path | None = None
    folded_path: Path | None = None
    source_type = "unknown"
    if path.is_file() and path.name == "report.json":
        report_path = path
        source_type = "report_json"
    elif path.is_file():
        folded_path = path
        source_type = "folded"
    else:
        for candidate in [path / "report.json", path.parent / "report.json"]:
            if candidate.exists():
                report_path = candidate
                source_type = "profile_dir"
                break
        for candidate in [path / "qperf" / "stack.folded", path / "stack.folded"]:
            if candidate.exists():
                folded_path = candidate
                break
    report = load_json_file(report_path) if report_path else {}
    if folded_path is None and report:
        artifact = (report.get("artifacts") or {}).get("folded")
        if artifact and Path(artifact).exists():
            folded_path = Path(artifact)
        elif report_path:
            for candidate in [
                report_path.parent / "qperf" / "stack.folded",
                report_path.parent / "stack.folded",
            ]:
                if candidate.exists():
                    folded_path = candidate
                    break
    hotspots = None
    if folded_path:
        hotspots = parse_folded(folded_path, limit=10_000)
    elif isinstance(report.get("hotspots"), dict):
        hotspots = report.get("hotspots")
    return {
        "input": str(source),
        "source_type": source_type,
        "report_path": str(report_path) if report_path else None,
        "folded_path": str(folded_path) if folded_path else None,
        "report": report,
        "hotspots": hotspots or {"total_samples": 0, "top_functions": [], "top_stacks": [], "category_totals": []},
    }


def perf_compare(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    baseline = resolve_perf_report_input(Path(args.baseline))
    candidate = resolve_perf_report_input(Path(args.candidate or args.compare))
    out_dir = repo_root / args.output_dir / "perf-compare"
    if args.name:
        out_dir = out_dir / args.name
    out_dir.mkdir(parents=True, exist_ok=True)

    metrics = compare_metric_entries(baseline, candidate)
    top_changes = compare_hotspot_entries(
        baseline["hotspots"].get("top_functions", []),
        candidate["hotspots"].get("top_functions", []),
        "function",
        args.top,
    )
    category_changes = compare_category_entries(
        baseline["hotspots"].get("category_totals", []),
        candidate["hotspots"].get("category_totals", []),
    )
    compatibility = compare_compatibility(baseline, candidate)
    conclusion = compare_conclusion(metrics)
    report = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "baseline": compare_input_summary(baseline),
        "candidate": compare_input_summary(candidate),
        "compatibility": compatibility,
        "conclusion": conclusion,
        "metrics": metrics,
        "top_changes": top_changes,
        "category_changes": category_changes,
        "artifacts": {
            "work_dir": str(out_dir),
            "json": str(out_dir / "compare.json"),
            "markdown": str(out_dir / "compare.md"),
            "csv": str(out_dir / "compare.csv"),
        },
    }
    write_text(out_dir / "compare.json", json.dumps(report, indent=2, sort_keys=True))
    write_compare_markdown(out_dir / "compare.md", report)
    write_compare_csv(out_dir / "compare.csv", report)
    print(f"artifacts: {out_dir}")
    print(f"conclusion: {conclusion['label']}")
    for item in metrics[:10]:
        print(
            f"- {item['group']}.{item['name']}: {format_na(item['baseline'])} -> "
            f"{format_na(item['candidate'])} ({format_na(item['relative_change_percent'])}%)"
        )
    return 0


def compare_input_summary(data: dict[str, Any]) -> dict[str, Any]:
    report = data.get("report") or {}
    return {
        "input": data.get("input"),
        "source_type": data.get("source_type"),
        "report": data.get("report_path"),
        "folded": data.get("folded_path"),
        "arch": report.get("arch"),
        "result": report.get("result"),
        "parameters": report.get("parameters") or {},
        "total_samples": data.get("hotspots", {}).get("total_samples", 0),
    }


def compare_compatibility(baseline: dict[str, Any], candidate: dict[str, Any]) -> dict[str, Any]:
    base_report = baseline.get("report") or {}
    cand_report = candidate.get("report") or {}
    base_params = base_report.get("parameters") or {}
    cand_params = cand_report.get("parameters") or {}
    keys = sorted(set(base_params) | set(cand_params))
    parameter_diffs = {
        key: {"baseline": base_params.get(key), "candidate": cand_params.get(key)}
        for key in keys
        if base_params.get(key) != cand_params.get(key)
    }
    warnings = []
    if base_report.get("arch") != cand_report.get("arch"):
        warnings.append("baseline and candidate arch differ")
    if not baseline.get("report_path") or not candidate.get("report_path"):
        warnings.append("one side is missing report.json; metadata comparison is incomplete")
    return {
        "same_arch": base_report.get("arch") == cand_report.get("arch"),
        "parameter_diffs": parameter_diffs,
        "warnings": warnings,
    }


def compare_metric_entries(baseline: dict[str, Any], candidate: dict[str, Any]) -> list[dict[str, Any]]:
    base_report = baseline.get("report") or {}
    cand_report = candidate.get("report") or {}
    specs = [
        ("workload", "throughput_bytes_per_second", "B/s", first_throughput),
        ("workload", "elapsed_seconds", "s", lambda r: workload_elapsed_seconds(r.get("workload_metrics") or {}, r.get("window") or {})),
        ("guest", "executed_instructions", "count", lambda r: numeric_metric(r.get("plugin_summary") or {}, "executed_instructions")),
        ("guest", "executed_blocks", "count", lambda r: numeric_metric(r.get("plugin_summary") or {}, "executed_blocks")),
        ("host", "elapsed_seconds", "s", lambda r: numeric_metric(r.get("host_time_metrics") or {}, "elapsed_seconds")),
        ("host", "user_seconds", "s", lambda r: numeric_metric(r.get("host_time_metrics") or {}, "user_seconds")),
        ("host", "system_seconds", "s", lambda r: numeric_metric(r.get("host_time_metrics") or {}, "system_seconds")),
        ("samples", "total_samples", "count", lambda r: (r.get("hotspots") or {}).get("total_samples")),
    ]
    entries = [
        compare_metric(group, name, unit, getter(base_report), getter(cand_report))
        for group, name, unit, getter in specs
    ]
    for key in sorted(set(metric_values(base_report)) | set(metric_values(cand_report))):
        entries.append(
            compare_metric(
                "virtio_counters",
                key,
                "count",
                metric_values(base_report).get(key),
                metric_values(cand_report).get(key),
            )
        )
    for event in sorted(set(host_perf_values(base_report)) | set(host_perf_values(cand_report))):
        entries.append(
            compare_metric(
                "host_perf",
                event,
                "",
                host_perf_values(base_report).get(event),
                host_perf_values(cand_report).get(event),
            )
        )
    return entries


def first_throughput(report: dict[str, Any]) -> int | float | None:
    workload = report.get("workload_metrics") or {}
    for key in ("dd", "wget"):
        for item in reversed(workload.get(key, [])):
            value = item.get("throughput_bytes_per_second")
            if isinstance(value, (int, float)):
                return value
    return None


def metric_values(report: dict[str, Any]) -> dict[str, Any]:
    values = dict((report.get("workload_metrics") or {}).get("values") or {})
    return {
        key: value
        for key, value in values.items()
        if isinstance(value, (int, float))
        and (
            key.startswith("virt")
            or "copy" in key
            or "inflight" in key
            or "depth" in key
            or key.endswith("_bytes")
        )
    }


def host_perf_values(report: dict[str, Any]) -> dict[str, Any]:
    events = ((report.get("host_perf_metrics") or {}).get("events") or {})
    return {
        name: event.get("value")
        for name, event in events.items()
        if isinstance(event, dict) and isinstance(event.get("value"), (int, float))
    }


def compare_metric(group: str, name: str, unit: str, baseline: Any, candidate: Any) -> dict[str, Any]:
    delta = None
    relative = None
    availability = "ok"
    if not isinstance(baseline, (int, float)) or not isinstance(candidate, (int, float)):
        availability = "missing"
    else:
        delta = candidate - baseline
        if baseline != 0:
            relative = delta / baseline * 100.0
    return {
        "group": group,
        "name": name,
        "unit": unit,
        "baseline": baseline if isinstance(baseline, (int, float)) else None,
        "candidate": candidate if isinstance(candidate, (int, float)) else None,
        "delta": round(delta, 6) if isinstance(delta, (int, float)) else None,
        "relative_change_percent": round(relative, 6) if isinstance(relative, (int, float)) else None,
        "availability": availability,
    }


def compare_hotspot_entries(
    baseline_items: list[dict[str, Any]],
    candidate_items: list[dict[str, Any]],
    key: str,
    limit: int,
) -> list[dict[str, Any]]:
    baseline_by_name = {item[key]: item for item in baseline_items}
    candidate_by_name = {item[key]: item for item in candidate_items}
    changes = []
    for name in sorted(set(baseline_by_name) | set(candidate_by_name)):
        base = baseline_by_name.get(name, {"samples": 0, "percent": 0.0})
        cand = candidate_by_name.get(name, {"samples": 0, "percent": 0.0})
        presence = "common" if name in baseline_by_name and name in candidate_by_name else (
            "added" if name in candidate_by_name else "removed"
        )
        changes.append(
            {
                "kind": key,
                "name": name,
                "baseline_samples": base["samples"],
                "candidate_samples": cand["samples"],
                "baseline_percent": base["percent"],
                "candidate_percent": cand["percent"],
                "delta_samples": cand["samples"] - base["samples"],
                "delta_percent_points": round(cand["percent"] - base["percent"], 6),
                "relative_change_percent": percent_delta(base["samples"], cand["samples"]),
                "presence": presence,
            }
        )
    changes.sort(key=lambda item: abs(item["delta_percent_points"]), reverse=True)
    return changes[:limit]


def compare_category_entries(
    baseline_items: list[dict[str, Any]],
    candidate_items: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    return compare_hotspot_entries(baseline_items, candidate_items, "category", 100)


def percent_delta(baseline: int | float, candidate: int | float) -> float | None:
    if baseline == 0:
        return None
    return round((candidate - baseline) / baseline * 100.0, 6)


def compare_conclusion(metrics: list[dict[str, Any]]) -> dict[str, str]:
    by_name = {(item["group"], item["name"]): item for item in metrics}
    throughput = by_name.get(("workload", "throughput_bytes_per_second"), {})
    elapsed = by_name.get(("workload", "elapsed_seconds"), {})
    evidence = []
    if isinstance(throughput.get("relative_change_percent"), (int, float)):
        change = throughput["relative_change_percent"]
        evidence.append(f"throughput {change:+.2f}%")
        if change <= -5.0:
            return {"label": "退化", "evidence": "; ".join(evidence)}
        if change >= 5.0:
            return {"label": "明显改善", "evidence": "; ".join(evidence)}
    if isinstance(elapsed.get("relative_change_percent"), (int, float)):
        change = elapsed["relative_change_percent"]
        evidence.append(f"elapsed {change:+.2f}%")
        if change >= 5.0:
            return {"label": "退化", "evidence": "; ".join(evidence)}
        if change <= -5.0:
            return {"label": "明显改善", "evidence": "; ".join(evidence)}
    if evidence:
        return {"label": "基本无变化", "evidence": "; ".join(evidence)}
    return {"label": "数据不足", "evidence": "missing comparable workload throughput or elapsed metrics"}


def write_compare_markdown(path: Path, report: dict[str, Any]) -> None:
    lines = [
        "# qperf A/B Compare",
        "",
        f"- conclusion: `{report['conclusion']['label']}`",
        f"- evidence: `{report['conclusion']['evidence']}`",
        f"- baseline: `{report['baseline']['input']}`",
        f"- candidate: `{report['candidate']['input']}`",
        "",
        "## Metrics",
        "",
        "| Group | Metric | Baseline | Candidate | Delta | Change |",
        "|---|---|---:|---:|---:|---:|",
    ]
    for item in report["metrics"]:
        lines.append(
            f"| `{item['group']}` | `{item['name']}` | {format_na(item['baseline'])} | "
            f"{format_na(item['candidate'])} | {format_na(item['delta'])} | "
            f"{format_na(item['relative_change_percent'])}% |"
        )
    lines.extend(["", "## Hotspot Categories", "", "| Category | Base% | Candidate% | Delta pp |", "|---|---:|---:|---:|"])
    for item in report["category_changes"][:20]:
        lines.append(
            f"| `{item['name']}` | {item['baseline_percent']:.2f}% | "
            f"{item['candidate_percent']:.2f}% | {item['delta_percent_points']:+.2f} |"
        )
    lines.extend(["", "## Top Function Changes", "", "| Function | Base% | Candidate% | Delta pp |", "|---|---:|---:|---:|"])
    for item in report["top_changes"][:20]:
        lines.append(
            f"| `{item['name']}` | {item['baseline_percent']:.2f}% | "
            f"{item['candidate_percent']:.2f}% | {item['delta_percent_points']:+.2f} |"
        )
    if report["compatibility"]["warnings"]:
        lines.extend(["", "## Warnings", ""])
        lines.extend(f"- {warning}" for warning in report["compatibility"]["warnings"])
    write_text(path, "\n".join(lines) + "\n")


def write_compare_csv(path: Path, report: dict[str, Any]) -> None:
    lines = [
        "row_type,group,name,unit,baseline,candidate,delta,relative_change_percent,baseline_samples,candidate_samples,baseline_percent,candidate_percent,delta_percent_points,presence,note"
    ]
    for item in report["metrics"]:
        lines.append(
            ",".join(
                [
                    "metric",
                    json.dumps(item["group"]),
                    json.dumps(item["name"]),
                    json.dumps(item["unit"]),
                    csv_value(item["baseline"]),
                    csv_value(item["candidate"]),
                    csv_value(item["delta"]),
                    csv_value(item["relative_change_percent"]),
                    "",
                    "",
                    "",
                    "",
                    "",
                    "",
                    json.dumps(item["availability"]),
                ]
            )
        )
    for row_type, rows in (("category", report["category_changes"]), ("function", report["top_changes"])):
        for item in rows:
            lines.append(
                ",".join(
                    [
                        row_type,
                        "",
                        json.dumps(item["name"]),
                        "",
                        "",
                        "",
                        "",
                        csv_value(item["relative_change_percent"]),
                        str(item["baseline_samples"]),
                        str(item["candidate_samples"]),
                        str(item["baseline_percent"]),
                        str(item["candidate_percent"]),
                        str(item["delta_percent_points"]),
                        json.dumps(item["presence"]),
                        "",
                    ]
                )
            )
    write_text(path, "\n".join(lines) + "\n")


def csv_value(value: Any) -> str:
    return "" if value is None else json.dumps(value)


def format_na(value: Any) -> str:
    if value is None:
        return "N/A"
    if isinstance(value, float):
        return f"{value:.6g}"
    return str(value)


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def compile_probe(repo_root: Path, work_dir: Path, arch: str) -> tuple[Path, Path]:
    config = ARCHES[arch]
    source = repo_root / "tools/starry-syscall-harness/probes/syscall_probe.c"
    linux_bin = work_dir / "probe-linux"
    starry_bin = work_dir / f"probe-{arch}"
    run(["gcc", "-O2", "-Wall", "-Wextra", "-o", str(linux_bin), str(source)], cwd=repo_root)
    run([config.cc, "-static", "-O2", "-Wall", "-Wextra", "-o", str(starry_bin), str(source)], cwd=repo_root)
    return linux_bin, starry_bin


def ensure_starry_qemu_defconfig(repo_root: Path, arch: str) -> None:
    run(["cargo", "xtask", "starry", "defconfig", f"qemu-{arch}"], cwd=repo_root)


def ensure_rootfs(repo_root: Path, arch: str) -> Path:
    config = ARCHES[arch]
    ensure_starry_qemu_defconfig(repo_root, arch)
    run(["cargo", "xtask", "starry", "rootfs", "--arch", arch], cwd=repo_root)
    candidates = [
        repo_root / "tmp" / "axbuild" / "rootfs" / f"rootfs-{arch}-alpine.img",
        repo_root / "target" / config.target / f"rootfs-{arch}.img",
    ]
    for rootfs in candidates:
        if rootfs.exists():
            return rootfs
    raise FileNotFoundError(candidates[0])


def inject_probe(rootfs: Path, probe: Path, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    run(["cp", "--reflink=auto", str(rootfs), str(output)], cwd=rootfs.parent)
    command_file = output.parent / "debugfs.commands"
    write_text(
        command_file,
        "\n".join(
            [
                "rm /root/syscall-probe",
                f"write {probe} /root/syscall-probe",
                "sif /root/syscall-probe mode 0100755",
                "",
            ]
        ),
    )
    run(["debugfs", "-w", "-f", str(command_file), str(output)], cwd=output.parent)


def write_qemu_config(path: Path, arch: str, disk_img: Path, timeout: int) -> None:
    config = ARCHES[arch]
    qemu_args = list(config.qemu_args)
    qemu_args.extend(
        [
            "-device",
            "virtio-blk-pci,drive=disk0",
            "-drive",
            f"id=disk0,if=none,format=raw,file={disk_img}",
            "-device",
            "virtio-net-pci,netdev=net0",
            "-netdev",
            "user,id=net0",
        ]
    )
    rendered_args = ",\n".join(json.dumps(item) for item in qemu_args)
    content = f"""args = [
{rendered_args}
]
uefi = false
to_bin = {str(config.to_bin).lower()}
shell_prefix = "root@starry:"
shell_init_cmd = "/root/syscall-probe"
success_regex = ["(?m)^{PROBE_END}\\\\s*$"]
fail_regex = ['(?i)\\bpanic(?:ked)?\\b']
timeout = {timeout}
"""
    write_text(path, content)


def run_starry_probe(
    repo_root: Path,
    arch: str,
    qemu_config: Path,
    rootfs: Path,
) -> subprocess.CompletedProcess[str]:
    return run(
        [
            "cargo",
            "xtask",
            "starry",
            "qemu",
            "--arch",
            arch,
            "--qemu-config",
            str(qemu_config),
            "--rootfs",
            str(rootfs),
        ],
        cwd=repo_root,
        check=False,
        capture=True,
    )


def print_summary(report: dict[str, Any]) -> None:
    print(f"arch: {report['arch']}")
    print(f"artifacts: {report['artifacts']['work_dir']}")
    differences = report["differences"]
    if not differences:
        print("result: no syscall semantic differences detected")
        return
    print(f"result: {len(differences)} syscall semantic difference(s) detected")
    for diff in differences:
        print(f"- {diff['case']}")
        print(f"  linux: {diff['linux']}")
        print(f"  starry: {diff['starry']}")


def discover_inside(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    arch = args.arch
    if arch not in ARCHES:
        raise SystemExit(f"unsupported arch {arch}; expected one of {', '.join(ARCHES)}")

    out_root = repo_root / args.output_dir
    work_dir = out_root / arch / "latest"
    if work_dir.exists():
        shutil.rmtree(work_dir)
    work_dir.mkdir(parents=True, exist_ok=True)

    linux_bin, starry_bin = compile_probe(repo_root, work_dir, arch)
    linux_run = run([str(linux_bin)], cwd=repo_root, check=True, capture=True)
    write_text(work_dir / "linux.stdout", linux_run.stdout)
    write_text(work_dir / "linux.stderr", linux_run.stderr)

    base_rootfs = ensure_rootfs(repo_root, arch)
    probe_rootfs = work_dir / f"rootfs-{arch}-probe.img"
    inject_probe(base_rootfs, starry_bin, probe_rootfs)
    qemu_config = work_dir / "qemu.toml"
    write_qemu_config(qemu_config, arch, probe_rootfs, args.timeout)

    starry_run = run_starry_probe(repo_root, arch, qemu_config, probe_rootfs)
    starry_output = starry_run.stdout + starry_run.stderr
    write_text(work_dir / "starry.stdout", starry_run.stdout)
    write_text(work_dir / "starry.stderr", starry_run.stderr)

    linux_cases = parse_cases(linux_run.stdout)
    starry_cases = parse_cases(starry_output)
    differences = compare_cases(linux_cases, starry_cases)
    report = {
        "arch": arch,
        "returncode": {"starry_qemu": starry_run.returncode},
        "linux": linux_cases,
        "starry": starry_cases,
        "differences": differences,
        "markers": {
            "starry_begin": PROBE_BEGIN in strip_ansi(starry_output),
            "starry_end": PROBE_END in strip_ansi(starry_output),
        },
        "artifacts": {
            "work_dir": str(work_dir),
            "report": str(work_dir / "report.json"),
            "qemu_config": str(qemu_config),
            "rootfs": str(probe_rootfs),
        },
    }
    write_text(work_dir / "report.json", json.dumps(report, indent=2, sort_keys=True))
    print_summary(report)

    if starry_run.returncode != 0:
        return starry_run.returncode
    if differences and args.fail_on_diff:
        return 2
    return 0


def discover(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    if not args.no_docker and not is_inside_docker():
        forwarded = [
            "discover",
            "--repo-root",
            "/work",
            "--arch",
            args.arch,
            "--timeout",
            str(args.timeout),
            "--output-dir",
            args.output_dir,
        ]
        if args.fail_on_diff:
            forwarded.append("--fail-on-diff")
        forwarded.append("--no-docker")
        return docker_reexec(repo_root, args.image, forwarded)
    return discover_inside(args)


def ui(args: argparse.Namespace) -> int:
    repo_root = repo_root_from(Path(args.repo_root)) if args.repo_root else script_repo_root()
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from ui_server import serve_ui

    return serve_ui(repo_root, args.host, args.port, args.image, args.open)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="StarryOS syscall and qperf differential harness")
    sub = parser.add_subparsers(dest="command", required=True)

    doctor_parser = sub.add_parser("doctor", help="check Docker and harness prerequisites")
    doctor_parser.add_argument("--repo-root")
    doctor_parser.add_argument("--image", default=DEFAULT_IMAGE)
    doctor_parser.set_defaults(func=doctor)

    discover_parser = sub.add_parser("discover", help="run syscall probes against Linux and StarryOS")
    discover_parser.add_argument("--repo-root")
    discover_parser.add_argument("--arch", default="riscv64", choices=sorted(ARCHES))
    discover_parser.add_argument("--image", default=DEFAULT_IMAGE)
    discover_parser.add_argument("--timeout", type=int, default=120)
    discover_parser.add_argument("--output-dir", default="target/starry-syscall-harness")
    discover_parser.add_argument("--no-docker", action="store_true")
    discover_parser.add_argument("--fail-on-diff", action="store_true")
    discover_parser.set_defaults(func=discover)

    perf_parser = sub.add_parser("perf-profile", help="run StarryOS qperf profiling in Docker")
    perf_parser.add_argument("--repo-root")
    perf_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64"])
    perf_parser.add_argument("--image", default=DEFAULT_IMAGE)
    perf_parser.add_argument("--timeout", type=int, default=20)
    perf_parser.add_argument("--format", default="all", choices=["folded", "svg", "pprof", "all"])
    perf_parser.add_argument("--freq", type=int, default=99)
    perf_parser.add_argument("--max-depth", type=int, default=64)
    perf_parser.add_argument("--mode", default="tb", choices=["tb", "insn"])
    perf_parser.add_argument("--top", type=int, default=20)
    perf_parser.add_argument("--min-percent", type=float, default=5.0)
    perf_parser.add_argument("--output-dir", default="target/starry-syscall-harness")
    perf_parser.add_argument("--no-docker", action="store_true")
    perf_parser.add_argument("--debug", action="store_true")
    perf_parser.add_argument("--kernel-filter", action="store_true")
    perf_parser.add_argument("--host-time", action="store_true")
    perf_parser.add_argument("--host-perf", action="store_true")
    perf_parser.add_argument(
        "--host-perf-events",
        default="task-clock,cycles,instructions,cache-references,cache-misses,context-switches,cpu-migrations,page-faults",
    )
    perf_parser.add_argument("--shell-init-cmd")
    perf_parser.add_argument("--shell-prefix", default="root@starry:")
    perf_parser.add_argument("--start-marker")
    perf_parser.add_argument("--stop-marker")
    perf_parser.add_argument("--workload-timeout", type=int)
    perf_parser.add_argument("--qperf-metrics", action="store_true")
    perf_parser.add_argument("--qemu-arg", action="append", default=[])
    perf_parser.set_defaults(func=perf_profile)

    perf_diff_parser = sub.add_parser("perf-diff", help="compare two qperf folded stack outputs")
    perf_diff_parser.add_argument("--repo-root")
    perf_diff_parser.add_argument("--baseline", required=True)
    perf_diff_parser.add_argument("--compare", required=True)
    perf_diff_parser.add_argument("--top", type=int, default=20)
    perf_diff_parser.add_argument("--output-dir", default="target/starry-syscall-harness")
    perf_diff_parser.set_defaults(func=perf_diff)

    perf_compare_parser = sub.add_parser("perf-compare", help="compare two qperf report.json/profile outputs")
    perf_compare_parser.add_argument("--repo-root")
    perf_compare_parser.add_argument("--baseline", required=True)
    candidate_group = perf_compare_parser.add_mutually_exclusive_group(required=True)
    candidate_group.add_argument("--candidate")
    candidate_group.add_argument("--compare")
    perf_compare_parser.add_argument("--top", type=int, default=20)
    perf_compare_parser.add_argument("--name")
    perf_compare_parser.add_argument("--output-dir", default="target/starry-syscall-harness")
    perf_compare_parser.set_defaults(func=perf_compare)

    ui_parser = sub.add_parser("ui", help="serve the optional local browser UI")
    ui_parser.add_argument("--repo-root")
    ui_parser.add_argument("--image", default=DEFAULT_IMAGE)
    ui_parser.add_argument("--host", default="127.0.0.1")
    ui_parser.add_argument("--port", type=int, default=8765)
    ui_parser.add_argument("--open", action="store_true")
    ui_parser.set_defaults(func=ui)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
