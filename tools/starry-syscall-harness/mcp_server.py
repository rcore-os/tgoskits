#!/usr/bin/env python3
import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = "2024-11-05"


def repo_root_from(start: Path) -> Path:
    current = start.resolve()
    for candidate in (current, *current.parents):
        if (candidate / "Cargo.toml").exists() and (candidate / "os/StarryOS").exists():
            return candidate
    raise SystemExit(f"cannot find tgoskits repo root from {start}")


def script_repo_root() -> Path:
    return repo_root_from(Path(__file__).resolve())


def read_message() -> dict[str, Any] | None:
    headers: dict[str, str] = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, _, value = line.decode("ascii").partition(":")
        headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))


def write_message(message: dict[str, Any]) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def tool_schema() -> list[dict[str, Any]]:
    return [
        {
            "name": "starry_syscall_doctor",
            "description": "Check Docker image and toolchain availability for the StarryOS syscall harness.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
        },
        {
            "name": "starry_syscall_discover",
            "description": "Run Linux-vs-StarryOS syscall probes in Docker and return the differential report.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "arch": {
                        "type": "string",
                        "enum": ["riscv64", "aarch64", "loongarch64", "x86_64"],
                        "default": "riscv64",
                    },
                    "timeout": {"type": "integer", "default": 120, "minimum": 1},
                    "fail_on_diff": {"type": "boolean", "default": False},
                },
                "additionalProperties": False,
            },
        },
        {
            "name": "starry_perf_profile",
            "description": "Run StarryOS qperf profiling in Docker and return hotspot/fix-candidate report paths.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "arch": {
                        "type": "string",
                        "enum": ["riscv64", "loongarch64"],
                        "default": "riscv64",
                    },
                    "timeout": {"type": "integer", "default": 20, "minimum": 1},
                    "format": {
                        "type": "string",
                        "enum": ["folded", "svg", "pprof", "all"],
                        "default": "all",
                    },
                    "freq": {"type": "integer", "default": 99, "minimum": 1},
                    "max_depth": {"type": "integer", "default": 64, "minimum": 1},
                    "mode": {"type": "string", "enum": ["tb", "insn"], "default": "tb"},
                    "top": {"type": "integer", "default": 20, "minimum": 1},
                    "min_percent": {"type": "number", "default": 5.0, "minimum": 0.0},
                    "debug": {"type": "boolean", "default": False},
                    "kernel_filter": {"type": "boolean", "default": False},
                    "host_time": {"type": "boolean", "default": False},
                    "host_perf": {"type": "boolean", "default": False},
                    "host_perf_events": {
                        "type": "string",
                        "default": "task-clock,cycles,instructions,cache-references,cache-misses,context-switches,cpu-migrations,page-faults",
                    },
                    "shell_init_cmd": {"type": "string"},
                    "shell_prefix": {"type": "string", "default": "root@starry:"},
                    "qemu_args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "default": [],
                    },
                },
                "additionalProperties": False,
            },
        },
        {
            "name": "starry_perf_diff",
            "description": "Compare two qperf folded stack outputs or profile directories.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "baseline": {"type": "string"},
                    "compare": {"type": "string"},
                    "top": {"type": "integer", "default": 20, "minimum": 1},
                },
                "required": ["baseline", "compare"],
                "additionalProperties": False,
            },
        },
        {
            "name": "starry_harness_ui_command",
            "description": "Return the command for launching the optional local browser UI.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": {"type": "string", "default": "127.0.0.1"},
                    "port": {"type": "integer", "default": 8765, "minimum": 0},
                    "open": {"type": "boolean", "default": False},
                },
                "additionalProperties": False,
            },
        },
    ]


def run_harness(repo: Path, args: list[str]) -> tuple[int, str]:
    cmd = ["python3", str(repo / "tools/starry-syscall-harness/harness.py"), *args]
    result = subprocess.run(cmd, cwd=repo, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    output = result.stdout
    if result.stderr:
        output += "\n[stderr]\n" + result.stderr
    return result.returncode, output[-12000:]


def handle_tool_call(repo: Path, params: dict[str, Any]) -> dict[str, Any]:
    name = params.get("name")
    arguments = params.get("arguments") or {}
    if name == "starry_syscall_doctor":
        code, output = run_harness(repo, ["doctor", "--repo-root", str(repo)])
    elif name == "starry_syscall_discover":
        command = [
            "discover",
            "--repo-root",
            str(repo),
            "--arch",
            arguments.get("arch", "riscv64"),
            "--timeout",
            str(arguments.get("timeout", 120)),
        ]
        if arguments.get("fail_on_diff", False):
            command.append("--fail-on-diff")
        code, output = run_harness(repo, command)
    elif name == "starry_perf_profile":
        command = [
            "perf-profile",
            "--repo-root",
            str(repo),
            "--arch",
            arguments.get("arch", "riscv64"),
            "--timeout",
            str(arguments.get("timeout", 20)),
            "--format",
            arguments.get("format", "all"),
            "--freq",
            str(arguments.get("freq", 99)),
            "--max-depth",
            str(arguments.get("max_depth", 64)),
            "--mode",
            arguments.get("mode", "tb"),
            "--top",
            str(arguments.get("top", 20)),
            "--min-percent",
            str(arguments.get("min_percent", 5.0)),
        ]
        if arguments.get("debug", False):
            command.append("--debug")
        if arguments.get("kernel_filter", False):
            command.append("--kernel-filter")
        if arguments.get("host_time", False):
            command.append("--host-time")
        if arguments.get("host_perf", False):
            command.append("--host-perf")
            command.extend(
                [
                    "--host-perf-events",
                    arguments.get(
                        "host_perf_events",
                        "task-clock,cycles,instructions,cache-references,cache-misses,context-switches,cpu-migrations,page-faults",
                    ),
                ]
            )
        if arguments.get("shell_init_cmd"):
            command.extend(["--shell-init-cmd", arguments["shell_init_cmd"]])
        if arguments.get("shell_prefix"):
            command.extend(["--shell-prefix", arguments["shell_prefix"]])
        for qemu_arg in arguments.get("qemu_args", []):
            command.append(f"--qemu-arg={qemu_arg}")
        code, output = run_harness(repo, command)
    elif name == "starry_perf_diff":
        command = [
            "perf-diff",
            "--repo-root",
            str(repo),
            "--baseline",
            arguments["baseline"],
            "--compare",
            arguments["compare"],
            "--top",
            str(arguments.get("top", 20)),
        ]
        code, output = run_harness(repo, command)
    elif name == "starry_harness_ui_command":
        host = arguments.get("host", "127.0.0.1")
        port = arguments.get("port", 8765)
        command = [
            "python3",
            str(repo / "tools/starry-syscall-harness/harness.py"),
            "ui",
            "--repo-root",
            str(repo),
            "--host",
            str(host),
            "--port",
            str(port),
        ]
        if arguments.get("open", False):
            command.append("--open")
        output = " ".join(command) + f"\nURL: http://{host}:{port}/"
        code = 0
    else:
        return {
            "content": [{"type": "text", "text": f"unknown tool: {name}"}],
            "isError": True,
        }
    return {"content": [{"type": "text", "text": output}], "isError": code != 0}


def serve(repo: Path) -> None:
    while True:
        message = read_message()
        if message is None:
            return
        message_id = message.get("id")
        method = message.get("method")
        if method == "initialize":
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": message_id,
                    "result": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": False}},
                        "serverInfo": {
                            "name": "starry-syscall-harness",
                            "version": "0.1.0",
                        },
                    },
                }
            )
        elif method == "tools/list":
            write_message({"jsonrpc": "2.0", "id": message_id, "result": {"tools": tool_schema()}})
        elif method == "tools/call":
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": message_id,
                    "result": handle_tool_call(repo, message.get("params") or {}),
                }
            )
        elif method == "ping":
            write_message({"jsonrpc": "2.0", "id": message_id, "result": {}})
        elif message_id is not None:
            write_message({"jsonrpc": "2.0", "id": message_id, "result": {}})


def main() -> int:
    parser = argparse.ArgumentParser(description="MCP server for the StarryOS syscall harness")
    parser.add_argument("--repo", default=None)
    args = parser.parse_args()
    repo = repo_root_from(Path(args.repo)) if args.repo else script_repo_root()
    serve(repo)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
