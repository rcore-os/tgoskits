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
        "python3",
        "tools/starry-syscall-harness/harness.py",
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
    parser = argparse.ArgumentParser(description="Linux-vs-StarryOS syscall differential harness")
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

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
