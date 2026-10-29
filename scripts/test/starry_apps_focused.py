#!/usr/bin/env python3
"""Rerun the failed application/architecture pairs from Starry Apps 40800597490."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
CASES = {
    "video-aarch64": ("aarch64", "cpu-video-test"),
    "video-riscv64": ("riscv64", "cpu-video-test"),
    "ahci-x86_64": ("x86_64", "qemu/ahci/ahci-dual-ncq-rw-20m"),
    "go-aarch64": ("aarch64", "go-lang"),
    "top-aarch64": ("aarch64", "top"),
    "findutils-riscv64": ("riscv64", "qemu/findutils"),
}


def ci_matrix(results, base):
    """Reuse the normal Apps planner's per-architecture execution environment."""
    templates = {}
    for row in base["include"]:
        command = shlex.split(row["command"])
        if command[:5] != ["cargo", "xtask", "starry", "app", "qemu"] or "--all" not in command:
            continue
        arch = command[command.index("--arch") + 1]
        if arch in templates:
            raise ValueError(f"Multiple Apps environment templates for {arch}")
        templates[arch] = row
    rows = []
    for result in results:
        row = dict(templates[result["arch"]])
        row.update(
            id=f"starry-focused-{result['case'].replace('_', '-')}",
            name=f"{result['case']} · {result['app']}",
            command=shlex.join([
                "python3", "scripts/test/starry_apps_focused.py", "--case", result["case"],
            ]),
        )
        rows.append(row)
    return {"include": rows}


def stop(process):
    """Reap the command and its process group on an interrupted local run."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def run_case(command, log_path):
    with log_path.open("wb") as log:
        try:
            process = subprocess.Popen(
                command, cwd=ROOT, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, start_new_session=True,
            )
        except OSError as error:
            log.write(str(error).encode() + b"\n")
            print(f"Cannot start command: {error}", file=sys.stderr, flush=True)
            return None
        try:
            with process.stdout:
                for line in process.stdout:
                    log.write(line)
                    log.flush()
                    sys.stdout.buffer.write(line)
                    sys.stdout.buffer.flush()
            return process.wait()
        finally:
            # Also clean up QEMU if its parent command exits unexpectedly.
            stop(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", action="append", choices=CASES, help="Select a pair; repeat to select several")
    preview = parser.add_mutually_exclusive_group()
    preview.add_argument("--list", action="store_true", help="List the selected pairs without running them")
    preview.add_argument("--dry-run", action="store_true", help="Print the exact commands without running them")
    preview.add_argument("--ci-matrix", action="store_true", help="Print a GitHub matrix using the normal Apps environment")
    parser.add_argument("--base-matrix", help="Normal Starry Apps planner matrix JSON; required with --ci-matrix")
    parser.add_argument("--output-dir", type=Path, help="Create this new directory for logs and summary.json")
    args = parser.parse_args()
    names = list(dict.fromkeys(args.case or CASES))
    results = []
    for name in names:
        arch, app = CASES[name]
        results.append({
            "case": name, "arch": arch, "app": app, "status": "NOT_RUN",
            "command": ["cargo", "xtask", "starry", "app", "qemu", "-t", app, "--arch", arch],
        })
    if args.ci_matrix:
        if not args.base_matrix:
            parser.error("--ci-matrix requires --base-matrix")
        try:
            matrix = ci_matrix(results, json.loads(args.base_matrix))
        except (KeyError, IndexError, TypeError, ValueError) as error:
            parser.error(f"Cannot plan focused matrix: {error}")
        print(json.dumps(matrix))
        return 0
    if args.base_matrix:
        parser.error("--base-matrix requires --ci-matrix")
    if args.list or args.dry_run:
        for result in results:
            detail = shlex.join(result["command"]) if args.dry_run else f"{result['arch']}\t{result['app']}"
            print(f"{result['case']}\t{detail}")
        return 0

    try:
        if args.output_dir:
            output = args.output_dir.resolve()
            output.mkdir(parents=True)
        else:
            parent = ROOT / "target/axbuild/starry-apps-focused"
            parent.mkdir(parents=True, exist_ok=True)
            output = Path(tempfile.mkdtemp(prefix="run-", dir=parent))
    except OSError as error:
        parser.error(f"Cannot create a new output directory: {error}")
    summary = {"started_at": datetime.now(timezone.utc).isoformat(), "status": "RUNNING", "results": results}
    summary_path = output / "summary.json"

    def save():
        summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    print(f"Logs: {output}", flush=True)
    save()
    interrupted = False
    for index, result in enumerate(results, 1):
        result.update(status="RUNNING", log=str(output / f"{result['case']}.log"))
        save()
        print(f"RUN {index}/{len(results)} {result['case']}: {shlex.join(result['command'])}", flush=True)
        started = time.monotonic()
        try:
            code = run_case(result["command"], Path(result["log"]))
            result.update(exit_code=code, status="PASS" if code == 0 else "FAIL")
        except KeyboardInterrupt:
            result.update(exit_code=130, status="INTERRUPTED")
            interrupted = True
        result["elapsed_s"] = round(time.monotonic() - started, 3)
        save()
        print(f"{result['status']} {result['case']} elapsed_s={result['elapsed_s']} log={result['log']}", flush=True)
        if interrupted:
            break
    passed = sum(result["status"] == "PASS" for result in results)
    failed = sum(result["status"] == "FAIL" for result in results)
    summary.update(
        status="INTERRUPTED" if interrupted else ("FAIL" if failed else "PASS"),
        passed=passed, failed=failed, finished_at=datetime.now(timezone.utc).isoformat(),
    )
    save()
    print(f"Focused Apps: {passed}/{len(results)} passed, {failed} failed; {summary_path}", flush=True)
    return 130 if interrupted else int(failed != 0)


if __name__ == "__main__":
    sys.exit(main())
