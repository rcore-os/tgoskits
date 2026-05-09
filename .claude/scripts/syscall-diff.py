#!/usr/bin/env python3
"""
Compare reference (Linux strace) syscall trace with target OS output.

Usage:
    python3 syscall-diff.py <linux-trace.log> <os-output.log> [--json]

Inputs:
    linux-trace.log: Output of `strace -f -v -o linux-trace.log <test-program>`
    os-output.log:   stdout + stderr + exit code from OS QEMU run

Output:
    Markdown diff report (or JSON with --json)
"""
import os
import re
import sys
import json
import difflib
from dataclasses import dataclass


@dataclass
class SyscallRow:
    pid: int
    name: str
    args: str
    result: str
    line_no: int


def parse_strace_log(path: str) -> list[SyscallRow]:
    """Parse strace -f -v output into structured syscall rows."""
    rows = []
    pattern = re.compile(r'^(\d+)\s+(\w+)\((.+)\)\s*=\s*(.+)$')

    with open(path) as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            if line.startswith(("+++", "---", "strace:")):
                continue

            m = pattern.match(line)
            if not m:
                # Try unfinished syscall lines
                m2 = re.match(r'^(\d+)\s+(\w+)\((.+)\s+<unfinished\s*\.\.\.>$', line)
                if m2:
                    rows.append(SyscallRow(
                        pid=int(m2.group(1)),
                        name=m2.group(2),
                        args=m2.group(3),
                        result="<unfinished>",
                        line_no=i
                    ))
                continue

            rows.append(SyscallRow(
                pid=int(m.group(1)),
                name=m.group(2),
                args=m.group(3),
                result=m.group(4),
                line_no=i
            ))
    return rows


def parse_os_output(path: str) -> dict:
    """Parse OS QEMU output log, extracting stdout and exit code."""
    with open(path) as f:
        content = f.read()

    exit_match = re.search(r'exit\s*code[:\s]*(\d+)', content, re.IGNORECASE)
    exit_code = int(exit_match.group(1)) if exit_match else None

    return {
        "stdout": content,
        "stderr": "",
        "exit_code": exit_code,
    }


def extract_linux_output(raw_strace_text: str) -> str:
    """Extract process output lines from strace raw text.
    Process output lines are those NOT matching the strace syscall line pattern."""
    output_lines = []
    for line in raw_strace_text.split("\n"):
        if re.match(r'^\d+\s+\w+\(', line):
            continue
        if line.startswith(("+++", "---", "strace:")):
            continue
        output_lines.append(line)
    return "\n".join(output_lines)


def compare_syscall_lists(linux_rows: list[SyscallRow], os_output: dict) -> dict:
    """Compare syscall sequences between Linux and OS."""
    linux_names = [r.name for r in linux_rows]

    os_syscall_pattern = re.compile(
        r'(?:syscall|SYSCALL|sys_)?(\w+)\s*[=(]\s*([^,\n]+)',
        re.IGNORECASE
    )
    os_syscalls = []
    for m in os_syscall_pattern.finditer(os_output.get("stdout", "")):
        os_syscalls.append((m.group(1), m.group(2).strip()))

    issues = []
    if not os_syscalls:
        issues.append({
            "type": "warning",
            "msg": "Could not extract syscall trace from OS output. Comparing only final output.",
        })

    linux_set = set(linux_names)
    os_set = set(s[0] for s in os_syscalls) if os_syscalls else set()
    missing = sorted(linux_set - os_set)
    extra = sorted(os_set - linux_set)

    if missing:
        issues.append({
            "type": "missing_syscall",
            "syscalls": missing,
            "msg": f"OS missing {len(missing)} syscall(s) that Linux uses: {', '.join(missing)}",
        })
    if extra:
        issues.append({
            "type": "extra_syscall",
            "syscalls": extra,
            "msg": f"OS uses {len(extra)} syscall(s) not in Linux trace: {', '.join(extra)}",
        })

    return {
        "linux_syscall_count": len(linux_rows),
        "os_syscall_count": len(os_syscalls),
        "issues": issues,
        "linux_syscalls": [(r.name, r.result) for r in linux_rows],
        "os_syscalls": os_syscalls,
    }


def compare_output(linux_output: str, os_output: dict) -> dict:
    """Compare stdout/stderr between Linux and OS output."""
    issues = []
    os_stdout = os_output.get("stdout", "").strip()

    # Normalize whitespace for comparison
    linux_normalized = "\n".join(line.rstrip() for line in linux_output.strip().split("\n"))
    os_normalized = "\n".join(line.rstrip() for line in os_stdout.split("\n"))

    if linux_normalized != os_normalized:
        diff = list(difflib.unified_diff(
            linux_normalized.splitlines(keepends=True),
            os_normalized.splitlines(keepends=True),
            fromfile="linux-output",
            tofile="os-output",
            lineterm="",
        ))
        issues.append({
            "type": "output_mismatch",
            "diff": diff[:100],
            "msg": "stdout/stderr differs between Linux and OS",
        })

    return {"issues": issues, "match": len(issues) == 0}


def generate_report(linux_file: str, os_file: str, syscall_diff: dict, output_diff: dict) -> str:
    """Generate markdown diff report."""
    lines = [
        "# Syscall Behavior Diff Report",
        "",
        f"**Linux trace**: `{linux_file}`",
        f"**OS output**: `{os_file}`",
        "",
        "## Syscall Comparison",
        "",
        f"- Linux syscalls traced: {syscall_diff['linux_syscall_count']}",
        f"- OS syscalls detected: {syscall_diff['os_syscall_count']}",
        "",
    ]

    if syscall_diff["issues"]:
        for issue in syscall_diff["issues"]:
            lines.append(f"### {issue['type']}")
            lines.append(f"**{issue['msg']}**")
            if "syscalls" in issue:
                for s in issue["syscalls"]:
                    lines.append(f"- `{s}`")
            lines.append("")

    lines.append("## Output Comparison")
    lines.append("")
    if output_diff["match"]:
        lines.append(":white_check_mark: Output matches between Linux and OS.")
    else:
        for issue in output_diff["issues"]:
            lines.append(f"### {issue['type']}")
            lines.append(f"**{issue['msg']}**")
            if "diff" in issue:
                lines.append("```diff")
                for d in issue["diff"]:
                    lines.append(d.rstrip("\n"))
                lines.append("```")
            lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: syscall-diff.py <linux-trace.log> <os-output.log> [--json]", file=sys.stderr)
        sys.exit(1)

    linux_file = sys.argv[1]
    os_file = sys.argv[2]
    as_json = "--json" in sys.argv

    if not os.path.exists(linux_file):
        print(f"Error: Linux trace file not found: {linux_file}", file=sys.stderr)
        sys.exit(1)
    if not os.path.exists(os_file):
        print(f"Error: OS output file not found: {os_file}", file=sys.stderr)
        sys.exit(1)

    with open(linux_file) as f:
        linux_raw = f.read()

    linux_rows = parse_strace_log(linux_file)
    linux_output = extract_linux_output(linux_raw)
    os_output = parse_os_output(os_file)

    syscall_diff = compare_syscall_lists(linux_rows, os_output)
    output_diff = compare_output(linux_output, os_output)

    if as_json:
        result = {
            "syscall_diff": {k: v for k, v in syscall_diff.items() if k not in ("linux_syscalls", "os_syscalls")},
            "syscall_diff_detail": {
                "linux_syscalls": syscall_diff["linux_syscalls"],
                "os_syscalls": syscall_diff["os_syscalls"],
            },
            "output_diff": output_diff,
        }
        print(json.dumps(result, indent=2))
    else:
        report = generate_report(linux_file, os_file, syscall_diff, output_diff)
        print(report)
