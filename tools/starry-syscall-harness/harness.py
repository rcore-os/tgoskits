#!/usr/bin/env python3
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
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
    script = (
        'python3 tools/starry-syscall-harness/harness.py "$@"; '
        "status=$?; "
        f"chown -R {uid}:{gid} target/starry-syscall-harness tools/qperf/target 2>/dev/null || true; "
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


def parse_folded(path: Path, limit: int = 20) -> dict[str, Any]:
    function_counts: dict[str, int] = {}
    stack_counts: dict[str, int] = {}
    total = 0
    if not path.exists():
        return {
            "total_samples": 0,
            "top_functions": [],
            "top_stacks": [],
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
        for function in stack.split(";"):
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
    }


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


def write_hotspots_csv(path: Path, hotspots: dict[str, Any]) -> None:
    lines = ["kind,name,samples,percent"]
    for item in hotspots["top_functions"]:
        lines.append(f"function,{json.dumps(item['function'])},{item['samples']},{item['percent']}")
    for item in hotspots["top_stacks"]:
        lines.append(f"stack,{json.dumps(item['stack'])},{item['samples']},{item['percent']}")
    write_text(path, "\n".join(lines) + "\n")


def write_perf_markdown(path: Path, report: dict[str, Any]) -> None:
    lines = [
        "# StarryOS qperf Performance Report",
        "",
        f"- arch: `{report['arch']}`",
        f"- result: `{report['result']}`",
        f"- samples: `{report['hotspots']['total_samples']}`",
        f"- artifacts: `{report['artifacts']['work_dir']}`",
        "",
        "## Top Functions",
        "",
        "| Function | Samples | Percent |",
        "|---|---:|---:|",
    ]
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
    run_result = run(command, cwd=repo_root, check=False, capture=True)
    write_text(work_dir / "profile.stdout", run_result.stdout)
    write_text(work_dir / "profile.stderr", run_result.stderr)

    folded = qperf_dir / "stack.folded"
    hotspots = parse_folded(folded, args.top)
    summary = parse_kv_summary(qperf_dir / "summary.txt")
    plugin_summary = parse_kv_summary(qperf_dir / "qperf.summary.txt")
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
        },
        "hotspots": hotspots,
        "summary": summary,
        "plugin_summary": plugin_summary,
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
            "folded": str(folded),
            "flamegraph": str(qperf_dir / "flamegraph.svg"),
        },
    }
    write_text(work_dir / "report.json", json.dumps(report, indent=2, sort_keys=True))
    write_perf_markdown(work_dir / "report.md", report)
    write_hotspots_csv(work_dir / "hotspots.csv", hotspots)
    print_perf_summary(report)
    return run_result.returncode


def print_perf_summary(report: dict[str, Any]) -> None:
    print(f"arch: {report['arch']}")
    print(f"artifacts: {report['artifacts']['work_dir']}")
    print(f"result: {report['result']}")
    print(f"samples: {report['hotspots']['total_samples']}")
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


def ensure_rootfs(repo_root: Path, arch: str) -> Path:
    config = ARCHES[arch]
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
    perf_parser.set_defaults(func=perf_profile)

    perf_diff_parser = sub.add_parser("perf-diff", help="compare two qperf folded stack outputs")
    perf_diff_parser.add_argument("--repo-root")
    perf_diff_parser.add_argument("--baseline", required=True)
    perf_diff_parser.add_argument("--compare", required=True)
    perf_diff_parser.add_argument("--top", type=int, default=20)
    perf_diff_parser.add_argument("--output-dir", default="target/starry-syscall-harness")
    perf_diff_parser.set_defaults(func=perf_diff)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
