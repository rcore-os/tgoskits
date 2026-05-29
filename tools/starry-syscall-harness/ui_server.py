#!/usr/bin/env python3
from __future__ import annotations

import json
import mimetypes
import shlex
import subprocess
import sys
import threading
import time
import urllib.parse
import uuid
import webbrowser
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


DEFAULT_OUTPUT_DIR = "target/starry-syscall-harness"
SYSCALL_ARCHES = ("aarch64", "loongarch64", "riscv64", "x86_64")
PERF_ARCHES = ("loongarch64", "riscv64")
PERF_FORMATS = ("all", "folded", "pprof", "svg")
PERF_MODES = ("insn", "tb")
MAX_LOG_LINES = 500
MAX_REQUEST_BYTES = 1_000_000
MAX_TEXT_FIELD_CHARS = 8192
MAX_QEMU_ARGS = 256
MAX_QEMU_ARG_CHARS = 4096


class ApiError(Exception):
    def __init__(self, status: HTTPStatus, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


@dataclass
class Job:
    id: str
    kind: str
    command: list[str]
    log_path: Path
    report_path: Path | None
    created_at: float = field(default_factory=time.time)
    started_at: float | None = None
    finished_at: float | None = None
    status: str = "queued"
    returncode: int | None = None
    output_tail: list[str] = field(default_factory=list)

    def append_output(self, line: str) -> None:
        self.output_tail.append(line.rstrip("\n"))
        if len(self.output_tail) > MAX_LOG_LINES:
            del self.output_tail[: len(self.output_tail) - MAX_LOG_LINES]

    def to_json(self) -> dict[str, Any]:
        duration = None
        if self.started_at is not None:
            end = self.finished_at if self.finished_at is not None else time.time()
            duration = round(end - self.started_at, 2)
        return {
            "id": self.id,
            "kind": self.kind,
            "status": self.status,
            "returncode": self.returncode,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "duration_sec": duration,
            "command": self.command,
            "log_path": str(self.log_path),
            "report_path": str(self.report_path) if self.report_path else None,
            "report_exists": bool(self.report_path and self.report_path.exists()),
            "output": "\n".join(self.output_tail),
        }


class HarnessUiState:
    def __init__(self, repo_root: Path, image: str) -> None:
        self.repo_root = repo_root.resolve()
        self.image = image
        self.script = self.repo_root / "tools/starry-syscall-harness/harness.py"
        self.web_root = self.repo_root / "tools/starry-syscall-harness/web"
        self.artifact_root = self.repo_root / DEFAULT_OUTPUT_DIR
        self.log_root = self.artifact_root / "ui/jobs"
        self.log_root.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.jobs: dict[str, Job] = {}

    def active_job(self) -> Job | None:
        for job in self.jobs.values():
            if job.status in {"queued", "running"}:
                return job
        return None

    def status(self) -> dict[str, Any]:
        with self.lock:
            jobs = sorted(self.jobs.values(), key=lambda item: item.created_at, reverse=True)
            active = self.active_job()
        return {
            "repo_root": str(self.repo_root),
            "image": self.image,
            "server_time": time.time(),
            "active_job": active.to_json() if active else None,
            "reports": {
                "syscall": {arch: self.report_summary("syscall", arch) for arch in SYSCALL_ARCHES},
                "perf": {arch: self.report_summary("perf", arch) for arch in PERF_ARCHES},
                "perf_diff": self.report_summary("perf-diff", None),
            },
            "jobs": [job.to_json() for job in jobs[:20]],
        }

    def start_job(self, payload: dict[str, Any]) -> Job:
        kind = require_choice(payload.get("kind"), ("doctor", "discover", "perf-profile", "perf-diff"), "kind")
        with self.lock:
            active = self.active_job()
            if active:
                raise ApiError(HTTPStatus.CONFLICT, f"job {active.id} is still {active.status}")
            command, report_path = self.build_command(kind, payload)
            job_id = uuid.uuid4().hex[:12]
            job = Job(
                id=job_id,
                kind=kind,
                command=command,
                log_path=self.log_root / f"{job_id}.log",
                report_path=report_path,
            )
            self.jobs[job_id] = job
        thread = threading.Thread(target=self.run_job, args=(job,), daemon=True)
        thread.start()
        return job

    def build_command(self, kind: str, payload: dict[str, Any]) -> tuple[list[str], Path | None]:
        base = [sys.executable, str(self.script), kind, "--repo-root", str(self.repo_root)]
        if kind == "doctor":
            return base + ["--image", self.image], None

        if kind == "discover":
            arch = require_choice(payload.get("arch", "riscv64"), SYSCALL_ARCHES, "arch")
            timeout = require_int(payload.get("timeout", 120), "timeout", minimum=1, maximum=3600)
            command = [
                *base,
                "--arch",
                arch,
                "--image",
                self.image,
                "--timeout",
                str(timeout),
                "--output-dir",
                DEFAULT_OUTPUT_DIR,
            ]
            if bool(payload.get("fail_on_diff", False)):
                command.append("--fail-on-diff")
            return command, self.syscall_report_path(arch)

        if kind == "perf-profile":
            arch = require_choice(payload.get("arch", "riscv64"), PERF_ARCHES, "arch")
            timeout = require_int(payload.get("timeout", 20), "timeout", minimum=1, maximum=3600)
            perf_format = require_choice(payload.get("format", "all"), PERF_FORMATS, "format")
            freq = require_int(payload.get("freq", 99), "freq", minimum=1, maximum=100000)
            max_depth = require_int(payload.get("max_depth", 64), "max_depth", minimum=1, maximum=1024)
            mode = require_choice(payload.get("mode", "tb"), PERF_MODES, "mode")
            top = require_int(payload.get("top", 20), "top", minimum=1, maximum=200)
            min_percent = require_float(payload.get("min_percent", 5.0), "min_percent", minimum=0.0, maximum=100.0)
            host_perf_events = optional_text(payload.get("host_perf_events"), "host_perf_events")
            shell_init_cmd = optional_text(payload.get("shell_init_cmd"), "shell_init_cmd")
            shell_prefix = optional_text(payload.get("shell_prefix"), "shell_prefix")
            qemu_args = parse_qemu_args(payload.get("qemu_args"))
            command = [
                *base,
                "--arch",
                arch,
                "--image",
                self.image,
                "--timeout",
                str(timeout),
                "--format",
                perf_format,
                "--freq",
                str(freq),
                "--max-depth",
                str(max_depth),
                "--mode",
                mode,
                "--top",
                str(top),
                "--min-percent",
                str(min_percent),
                "--output-dir",
                DEFAULT_OUTPUT_DIR,
            ]
            if bool(payload.get("debug", False)):
                command.append("--debug")
            if bool(payload.get("kernel_filter", False)):
                command.append("--kernel-filter")
            if bool(payload.get("host_time", False)):
                command.append("--host-time")
            if bool(payload.get("host_perf", False)):
                command.append("--host-perf")
                if host_perf_events is not None:
                    command.extend(["--host-perf-events", host_perf_events])
            if shell_init_cmd is not None:
                command.extend(["--shell-init-cmd", shell_init_cmd])
            if shell_prefix is not None:
                command.extend(["--shell-prefix", shell_prefix])
            for qemu_arg in qemu_args:
                command.append(f"--qemu-arg={qemu_arg}")
            return command, self.perf_report_path(arch)

        baseline = self.resolve_repo_path(str(payload.get("baseline", "")), required=True)
        compare = self.resolve_repo_path(str(payload.get("compare", "")), required=True)
        top = require_int(payload.get("top", 20), "top", minimum=1, maximum=200)
        command = [
            *base,
            "--baseline",
            str(baseline),
            "--compare",
            str(compare),
            "--top",
            str(top),
            "--output-dir",
            DEFAULT_OUTPUT_DIR,
        ]
        return command, self.perf_diff_report_path()

    def run_job(self, job: Job) -> None:
        with self.lock:
            job.status = "running"
            job.started_at = time.time()
        job.log_path.parent.mkdir(parents=True, exist_ok=True)
        returncode = 1
        try:
            with job.log_path.open("w", encoding="utf-8", errors="replace") as log_file:
                process = subprocess.Popen(
                    job.command,
                    cwd=self.repo_root,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    bufsize=1,
                )
                assert process.stdout is not None
                for line in process.stdout:
                    log_file.write(line)
                    log_file.flush()
                    with self.lock:
                        job.append_output(line)
                returncode = process.wait()
        except Exception as exc:  # noqa: BLE001 - surface unexpected worker failures in the UI.
            with self.lock:
                job.append_output(f"ui worker error: {exc}")
        finally:
            with self.lock:
                job.returncode = returncode
                job.finished_at = time.time()
                job.status = "succeeded" if returncode == 0 else "failed"

    def get_job(self, job_id: str) -> Job:
        with self.lock:
            job = self.jobs.get(job_id)
        if not job:
            raise ApiError(HTTPStatus.NOT_FOUND, f"unknown job {job_id}")
        return job

    def load_report(self, kind: str, arch: str | None) -> dict[str, Any]:
        path = self.report_path(kind, arch)
        if not path.exists():
            raise ApiError(HTTPStatus.NOT_FOUND, f"report not found: {path}")
        report = read_json(path)
        if not isinstance(report, dict):
            raise ApiError(HTTPStatus.INTERNAL_SERVER_ERROR, f"report is not a JSON object: {path}")
        report["_ui"] = {
            "report_path": str(path),
            "report_url": self.file_url(path),
            "artifacts": self.artifact_state(report.get("artifacts", {})),
        }
        return report

    def artifact_state(self, artifacts: Any) -> dict[str, dict[str, Any]]:
        if not isinstance(artifacts, dict):
            return {}
        state: dict[str, dict[str, Any]] = {}
        for name, value in artifacts.items():
            if not isinstance(value, str):
                continue
            try:
                path = self.resolve_repo_path(value, required=False)
            except ApiError:
                continue
            state[name] = {
                "path": str(path),
                "exists": path.exists(),
                "is_file": path.is_file(),
                "url": self.file_url(path) if path.is_file() else None,
            }
        return state

    def report_summary(self, kind: str, arch: str | None) -> dict[str, Any]:
        path = self.report_path(kind, arch)
        summary: dict[str, Any] = {"path": str(path), "exists": path.exists()}
        if not path.exists():
            return summary
        summary["mtime"] = path.stat().st_mtime
        try:
            report = read_json(path)
            if kind == "syscall":
                summary["differences"] = len(report.get("differences", []))
                summary["markers"] = report.get("markers", {})
            elif kind == "perf":
                summary["result"] = report.get("result")
                summary["samples"] = report.get("hotspots", {}).get("total_samples")
                summary["fix_candidates"] = len(report.get("fix_candidates", []))
                summary["host_elapsed_seconds"] = report.get("host_time_metrics", {}).get("elapsed_seconds")
            elif kind == "perf-diff":
                summary["top_changes"] = len(report.get("top_changes", []))
        except (OSError, json.JSONDecodeError):
            summary["error"] = "failed to read report"
        return summary

    def report_path(self, kind: str, arch: str | None) -> Path:
        if kind == "syscall":
            if not arch:
                raise ApiError(HTTPStatus.BAD_REQUEST, "arch is required")
            return self.syscall_report_path(require_choice(arch, SYSCALL_ARCHES, "arch"))
        if kind == "perf":
            if not arch:
                raise ApiError(HTTPStatus.BAD_REQUEST, "arch is required")
            return self.perf_report_path(require_choice(arch, PERF_ARCHES, "arch"))
        if kind == "perf-diff":
            return self.perf_diff_report_path()
        raise ApiError(HTTPStatus.BAD_REQUEST, f"unsupported report kind {kind}")

    def syscall_report_path(self, arch: str) -> Path:
        return self.artifact_root / arch / "latest/report.json"

    def perf_report_path(self, arch: str) -> Path:
        return self.artifact_root / "perf" / arch / "latest/report.json"

    def perf_diff_report_path(self) -> Path:
        return self.artifact_root / "perf-diff/report.json"

    def resolve_repo_path(self, raw_path: str, *, required: bool) -> Path:
        if not raw_path:
            if required:
                raise ApiError(HTTPStatus.BAD_REQUEST, "path is required")
            return self.repo_root
        path = Path(raw_path)
        if path.is_absolute() and len(path.parts) >= 2 and path.parts[1] == "work":
            path = self.repo_root.joinpath(*path.parts[2:])
        elif not path.is_absolute():
            path = self.repo_root / path
        resolved = path.resolve(strict=False)
        try:
            resolved.relative_to(self.repo_root)
        except ValueError as exc:
            raise ApiError(HTTPStatus.BAD_REQUEST, f"path escapes repo root: {raw_path}") from exc
        if required and not resolved.exists():
            raise ApiError(HTTPStatus.BAD_REQUEST, f"path does not exist: {resolved}")
        return resolved

    def resolve_artifact_file(self, raw_path: str) -> Path:
        path = self.resolve_repo_path(raw_path, required=True)
        try:
            path.relative_to(self.artifact_root)
        except ValueError as exc:
            raise ApiError(HTTPStatus.FORBIDDEN, "only harness artifacts can be served") from exc
        if not path.is_file():
            raise ApiError(HTTPStatus.NOT_FOUND, f"artifact is not a file: {path}")
        return path

    @staticmethod
    def file_url(path: Path) -> str:
        return "/api/file?path=" + urllib.parse.quote(str(path))


def require_choice(value: Any, choices: tuple[str, ...], name: str) -> str:
    if not isinstance(value, str) or value not in choices:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be one of {', '.join(choices)}")
    return value


def require_int(value: Any, name: str, *, minimum: int, maximum: int) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError) as exc:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be an integer") from exc
    if parsed < minimum or parsed > maximum:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be between {minimum} and {maximum}")
    return parsed


def require_float(value: Any, name: str, *, minimum: float, maximum: float) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError) as exc:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be a number") from exc
    if parsed < minimum or parsed > maximum:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be between {minimum:g} and {maximum:g}")
    return parsed


def optional_text(value: Any, name: str, *, maximum: int = MAX_TEXT_FIELD_CHARS) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be a string")
    text = value.strip()
    if not text:
        return None
    if len(text) > maximum:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be at most {maximum} characters")
    if "\x00" in text:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must not contain NUL bytes")
    return text


def parse_qemu_args(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, list):
        args = [require_qemu_arg(item, f"qemu_args[{index}]") for index, item in enumerate(value)]
        return check_qemu_args([arg for arg in args if arg])
    if not isinstance(value, str):
        raise ApiError(HTTPStatus.BAD_REQUEST, "qemu_args must be a string or an array of strings")

    text = optional_text(value, "qemu_args")
    if text is None:
        return []
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if len(lines) > 1:
        return check_qemu_args([require_qemu_arg(line, "qemu_args") for line in lines])
    try:
        args = [require_qemu_arg(arg, "qemu_args") for arg in shlex.split(text, comments=False, posix=True)]
        return check_qemu_args(args)
    except ValueError as exc:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"qemu_args shell-like parse failed: {exc}") from exc


def require_qemu_arg(value: Any, name: str) -> str:
    if not isinstance(value, str):
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} must be a string")
    arg = value.strip()
    if not arg:
        return ""
    if len(arg) > MAX_QEMU_ARG_CHARS:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} entries must be at most {MAX_QEMU_ARG_CHARS} characters")
    if "\x00" in arg or "\n" in arg or "\r" in arg:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"{name} entries must be single-line strings without NUL bytes")
    return arg


def check_qemu_args(args: list[str]) -> list[str]:
    if len(args) > MAX_QEMU_ARGS:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"qemu_args must contain at most {MAX_QEMU_ARGS} arguments")
    return args


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def make_handler(state: HarnessUiState) -> type[BaseHTTPRequestHandler]:
    class HarnessRequestHandler(BaseHTTPRequestHandler):
        server_version = "StarryHarnessUI/1.0"

        def do_GET(self) -> None:
            try:
                parsed = urllib.parse.urlparse(self.path)
                if parsed.path == "/api/status":
                    self.send_json(state.status())
                    return
                if parsed.path.startswith("/api/jobs/"):
                    job_id = parsed.path.rsplit("/", 1)[-1]
                    self.send_json(state.get_job(job_id).to_json())
                    return
                if parsed.path == "/api/report":
                    query = urllib.parse.parse_qs(parsed.query)
                    kind = query.get("kind", [""])[0]
                    arch = query.get("arch", [None])[0]
                    self.send_json(state.load_report(kind, arch))
                    return
                if parsed.path == "/api/file":
                    query = urllib.parse.parse_qs(parsed.query)
                    path = query.get("path", [""])[0]
                    self.send_file(state.resolve_artifact_file(path))
                    return
                self.send_static(parsed.path)
            except ApiError as exc:
                self.send_json({"error": exc.message}, status=exc.status)
            except Exception as exc:  # noqa: BLE001 - return useful local UI diagnostics.
                self.send_json({"error": str(exc)}, status=HTTPStatus.INTERNAL_SERVER_ERROR)

        def do_POST(self) -> None:
            try:
                parsed = urllib.parse.urlparse(self.path)
                if parsed.path != "/api/jobs":
                    raise ApiError(HTTPStatus.NOT_FOUND, "unknown endpoint")
                payload = self.read_json_body()
                job = state.start_job(payload)
                self.send_json(job.to_json(), status=HTTPStatus.ACCEPTED)
            except ApiError as exc:
                self.send_json({"error": exc.message}, status=exc.status)
            except Exception as exc:  # noqa: BLE001 - return useful local UI diagnostics.
                self.send_json({"error": str(exc)}, status=HTTPStatus.INTERNAL_SERVER_ERROR)

        def read_json_body(self) -> dict[str, Any]:
            length_text = self.headers.get("Content-Length", "0")
            try:
                length = int(length_text)
            except ValueError as exc:
                raise ApiError(HTTPStatus.BAD_REQUEST, "invalid Content-Length") from exc
            if length > MAX_REQUEST_BYTES:
                raise ApiError(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "request body is too large")
            raw = self.rfile.read(length)
            try:
                payload = json.loads(raw.decode("utf-8") if raw else "{}")
            except json.JSONDecodeError as exc:
                raise ApiError(HTTPStatus.BAD_REQUEST, "request body must be JSON") from exc
            if not isinstance(payload, dict):
                raise ApiError(HTTPStatus.BAD_REQUEST, "request body must be a JSON object")
            return payload

        def send_static(self, path: str) -> None:
            if path == "/":
                target = state.web_root / "index.html"
            else:
                relative = path.lstrip("/")
                if "/" in relative:
                    raise ApiError(HTTPStatus.NOT_FOUND, "static file not found")
                target = state.web_root / relative
            resolved = target.resolve(strict=False)
            try:
                resolved.relative_to(state.web_root)
            except ValueError as exc:
                raise ApiError(HTTPStatus.NOT_FOUND, "static file not found") from exc
            if not resolved.is_file():
                raise ApiError(HTTPStatus.NOT_FOUND, "static file not found")
            self.send_file(resolved, cache_static=True)

        def send_file(self, path: Path, *, cache_static: bool = False) -> None:
            content_type = mimetypes.guess_type(str(path))[0] or "application/octet-stream"
            data = path.read_bytes()
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "max-age=60" if cache_static else "no-store")
            self.end_headers()
            self.wfile.write(data)

        def send_json(self, payload: Any, *, status: HTTPStatus = HTTPStatus.OK) -> None:
            data = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, fmt: str, *args: Any) -> None:
            sys.stderr.write("[harness-ui] " + fmt % args + "\n")

    return HarnessRequestHandler


def serve_ui(repo_root: Path, host: str, port: int, image: str, open_browser: bool) -> int:
    state = HarnessUiState(repo_root, image)
    handler = make_handler(state)
    server = ThreadingHTTPServer((host, port), handler)
    url = f"http://{host}:{server.server_port}/"
    print(f"StarryOS harness UI: {url}", flush=True)
    print("Press Ctrl-C to stop.", flush=True)
    if open_browser:
        threading.Timer(0.3, lambda: webbrowser.open(url)).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopping StarryOS harness UI.", flush=True)
    finally:
        server.server_close()
    return 0
