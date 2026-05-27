#!/usr/bin/env python3
import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = "2024-11-05"


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
    parser.add_argument("--repo", default="/home/cg24/tgoskits")
    args = parser.parse_args()
    serve(Path(args.repo).resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
