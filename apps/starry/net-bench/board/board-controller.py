#!/usr/bin/env python3
"""SG2002 board net-bench controller.

Flip the QEMU model: the board (StarryOS) runs iperf3 -s -1 while the PC
runs iperf3 -c locally.  Board-side /proc/net/dev snapshots are collected
via paramiko SSH and interleaved with client-side iperf3 -J JSON to produce
output that the existing core/summarize.py can parse unchanged.

Usage:
    python3 board/board-controller.py [--config board/board-config.toml]
                                     [--deploy] [--output results/]
                                     [--test tcp1]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Python 3.11+ tomllib; fall back to tomli / toml
# ---------------------------------------------------------------------------
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ImportError:
        print(
            "error: Python >= 3.11 required for tomllib, or install `tomli`",
            file=sys.stderr,
        )
        raise SystemExit(1)

# ---------------------------------------------------------------------------
# paramiko is required
# ---------------------------------------------------------------------------
try:
    import paramiko
except ImportError:
    print(
        "error: `paramiko` is required.  Install with: pip install paramiko",
        file=sys.stderr,
    )
    raise SystemExit(1)

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
BOARD_DIR = Path(__file__).resolve().parent
BENCH_ROOT = BOARD_DIR.parent
CORE_DIR = BENCH_ROOT / "core"
RESULTS_DIR = BENCH_ROOT / "results"
DEFAULT_CONFIG = BOARD_DIR / "board-config.toml"

# ---------------------------------------------------------------------------
# Test matrix defaults (overridden by config)
# ---------------------------------------------------------------------------
DEFAULT_MATRIX: dict[str, str] = {
    "tcp1": "",
    "tcp4": "-P 4 -w 128K",
    "tcp1r": "-R",
    "udp1g": "-u -b 1G",
    "udp64": "-u -l 64 -b 100M",
}

# Regexes for extracting SERVER_READY from the board channel stream.
# board-server.sh emits these markers on stdout; we scan for them.
SERVER_READY_RE = re.compile(r"^SERVER_READY\s*$", re.MULTILINE)
BOARD_ERROR_RE = re.compile(r"^BOARD_SERVER_ERROR:\s*(.*)$", re.MULTILINE)

# Per-iteration timeout padding (seconds) added to the test duration for
# SSH command execution and iperf3 client startup/shutdown.
TIMEOUT_PAD = 20


# ===================================================================
# BoardSession — paramiko SSH wrapper
# ===================================================================


class BoardSession:
    """SSH session to an SG2002 board with convenience methods."""

    def __init__(self, config: dict[str, Any]):
        board = config.get("board", {})
        self.host: str = board.get("ip", "192.168.50.1")
        self.port: int = int(board.get("ssh_port", 22))
        self.user: str = board.get("ssh_user", "root")
        self.password: str | None = board.get("ssh_password")
        self.key_path: str | None = board.get("ssh_key_path")
        self._connect_timeout: int = int(board.get("connect_timeout", 15))

        self._client: paramiko.SSHClient | None = None

    # ---- connect / close --------------------------------------------------

    def connect(self) -> None:
        """Establish SSH connection.  Raises on failure."""
        self._client = paramiko.SSHClient()
        self._client.set_missing_host_key_policy(paramiko.AutoAddPolicy())

        connect_kwargs: dict[str, Any] = {
            "hostname": self.host,
            "port": self.port,
            "username": self.user,
            "timeout": self._connect_timeout,
            "banner_timeout": self._connect_timeout,
            "auth_timeout": self._connect_timeout,
        }

        if self.key_path:
            key_path = os.path.expanduser(self.key_path)
            if os.path.isfile(key_path):
                connect_kwargs["key_filename"] = key_path
            else:
                print(
                    f"warning: SSH key not found at {key_path}, falling back to password",
                    file=sys.stderr,
                )
        if self.password and "key_filename" not in connect_kwargs:
            connect_kwargs["password"] = self.password

        try:
            self._client.connect(**connect_kwargs)
        except Exception as exc:
            raise ConnectionError(
                f"SSH connection to {self.user}@{self.host}:{self.port} failed: {exc}"
            ) from exc

        # Quick connectivity check.
        _stdin, stdout, _stderr = self._client.exec_command("uptime", timeout=10)
        status = stdout.channel.recv_exit_status()
        if status != 0:
            raise ConnectionError(
                f"SSH connected but 'uptime' returned exit status {status}"
            )

        print(
            f"[board] connected to {self.user}@{self.host}:{self.port}",
            file=sys.stderr,
        )

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> "BoardSession":
        self.connect()
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    # ---- command execution ------------------------------------------------

    def run(self, command: str, timeout: int = 30) -> str:
        """Execute a command on the board and return stdout as a string."""
        assert self._client is not None, "not connected"
        _stdin, stdout, _stderr = self._client.exec_command(command, timeout=timeout)
        return stdout.read().decode(errors="replace")

    def run_status(self, command: str, timeout: int = 30) -> tuple[str, int]:
        """Execute a command; return (stdout, exit_status)."""
        assert self._client is not None, "not connected"
        _stdin, stdout, _stderr = self._client.exec_command(command, timeout=timeout)
        out = stdout.read().decode(errors="replace")
        status = stdout.channel.recv_exit_status()
        return out, status

    # ---- file transfer ----------------------------------------------------

    def deploy(self, local_path: Path, remote_path: str = "/tmp/") -> str:
        """Upload a file to the board via exec_command heredoc.

        Uses exec_command(``cat > path && chmod +x path``) rather than
        SFTP, because dropbear (the SSH server on StarryOS) does not
        support the SFTP subsystem.
        """
        assert self._client is not None, "not connected"
        remote_full = remote_path.rstrip("/") + "/" + local_path.name
        content = local_path.read_text()
        _stdin, stdout, _stderr = self._client.exec_command(
            f"cat > {remote_full} && chmod +x {remote_full} && echo DEPLOY_OK",
            timeout=10,
        )
        _stdin.write(content)
        _stdin.channel.shutdown_write()
        out = stdout.read().decode(errors="replace")
        if "DEPLOY_OK" not in out:
            raise RuntimeError(
                f"deploy of {local_path.name} to {remote_full} failed: {out}"
            )
        print(
            f"[board] deployed {local_path.name} -> {remote_full}", file=sys.stderr,
        )
        return remote_full

    def file_exists(self, remote_path: str) -> bool:
        """Check whether a file exists on the board."""
        _, status = self.run_status(f"test -f {remote_path}", timeout=5)
        return status == 0


# ===================================================================
# Test runner
# ===================================================================


class TestRunner:
    """Orchestrate one test iteration across PC (client) and board (server)."""

    def __init__(
        self,
        board: BoardSession,
        *,
        port: int = 5201,
        duration: int = 5,
        window: str = "256K",
        server_script: str = "/tmp/board-server.sh",
    ):
        self.board = board
        self.port = port
        self.duration = duration
        self.window = window
        self.server_script = server_script

    def run_one(
        self, test_id: str, extra_args: str, warmup: bool = False
    ) -> tuple[dict[str, Any], str] | None:
        """Run one iteration.

        Returns (iperf3_json, board_output) on success, or None on failure.
        The board_output contains the NET_STATS_BEGIN/END blocks from
        board-server.sh.
        """

        transport = self.board._client.get_transport()  # type: ignore[union-attr]
        assert transport is not None, "no SSH transport"
        channel = transport.open_session()
        warmup_flag = 1 if warmup else 0

        try:
            channel.settimeout(self.duration + TIMEOUT_PAD)
            # ---- Phase 1: start board-server.sh on the board --------------
            channel.exec_command(
                f"sh {self.server_script} {self.port} {warmup_flag} {self.duration}"
            )

            # ---- Phase 2: wait for SERVER_READY ---------------------------
            board_buf = ""
            ready = False
            deadline = time.monotonic() + 30  # 30 s for server to start
            while time.monotonic() < deadline:
                if channel.recv_ready():
                    board_buf += channel.recv(4096).decode(errors="replace")
                # Check for SERVER_READY in the accumulated buffer.
                if SERVER_READY_RE.search(board_buf):
                    ready = True
                    break
                # Check for errors from board-server.sh.
                err_m = BOARD_ERROR_RE.search(board_buf)
                if err_m:
                    print(
                        f"[board] server error: {err_m.group(1)}",
                        file=sys.stderr,
                    )
                    return None
                if channel.exit_status_ready():
                    # board-server.sh exited before we saw SERVER_READY.
                    # Drain remaining output for diagnostics.
                    while channel.recv_ready():
                        board_buf += channel.recv(4096).decode(errors="replace")
                    print(
                        f"[board] server exited prematurely:\n{board_buf[-500:]}",
                        file=sys.stderr,
                    )
                    return None
                time.sleep(0.05)

            if not ready:
                print(
                    f"[board] timeout waiting for SERVER_READY (30s)",
                    file=sys.stderr,
                )
                return None

            # ---- Phase 3: run iperf3 client locally -----------------------
            args_list = _build_iperf3_args(
                host=self.board.host,
                port=self.port,
                duration=self.duration,
                window=self.window,
                extra=extra_args,
            )

            print(
                f"[iperf3] {' '.join(args_list)}", file=sys.stderr,
            )

            try:
                client_proc = subprocess.run(
                    args_list,
                    capture_output=True,
                    text=True,
                    timeout=self.duration + TIMEOUT_PAD,
                )
            except subprocess.TimeoutExpired:
                print(
                    f"[iperf3] client timed out after {self.duration + TIMEOUT_PAD}s",
                    file=sys.stderr,
                )
                return None

            if client_proc.returncode != 0 and not warmup:
                print(
                    f"[iperf3] client exited with code {client_proc.returncode}",
                    file=sys.stderr,
                )
                print(client_proc.stderr[:500], file=sys.stderr)

            # ---- Phase 4: collect board output after server exits ----------
            # After iperf3 -s -1 handles the client, board-server.sh
            # prints the after-snapshot and exits.  We wait for the channel
            # to close and collect everything.
            wait_deadline = time.monotonic() + (self.duration + TIMEOUT_PAD)
            while not channel.exit_status_ready() and time.monotonic() < wait_deadline:
                if channel.recv_ready():
                    board_buf += channel.recv(65536).decode(errors="replace")
                time.sleep(0.05)

            # Drain any remaining output.
            time.sleep(0.1)
            while channel.recv_ready():
                board_buf += channel.recv(65536).decode(errors="replace")

            # Parse iperf3 JSON (may fail for warmup iterations that failed).
            try:
                client_json = json.loads(client_proc.stdout)
            except json.JSONDecodeError:
                print(
                    f"[iperf3] failed to parse client JSON (warmup={warmup_flag})",
                    file=sys.stderr,
                )
                if not warmup:
                    return None
                client_json = {}

        except Exception as exc:
            print(f"[error] {test_id}: {exc}", file=sys.stderr)
            return None
        finally:
            channel.close()

        return client_json, board_buf


def _build_iperf3_args(
    host: str,
    port: int,
    duration: int,
    window: str,
    extra: str,
) -> list[str]:
    """Build the iperf3 client command line."""
    args = [
        "iperf3",
        "-c", host,
        "-p", str(port),
        "-t", str(duration),
        "--connect-timeout", "5000",
        "-J",
    ]
    if window:
        args.extend(["-w", window])
    if extra:
        args.extend(extra.split())
    return args


# ===================================================================
# Output formatting
# ===================================================================


NETSTATS_LINE_RE = re.compile(r"^NET_STATS_(?:BEGIN|END)\b", re.MULTILINE)


def _extract_stats_blocks(board_output: str) -> str:
    """Extract only NET_STATS_BEGIN/END blocks from raw board output.

    board-server.sh emits SERVER_READY and may include iperf3 server
    diagnostic lines between the two NET_STATS blocks.  summarize.py's
    parse_log() treats any non-NET_STATS lines between NET_BENCH_BEGIN/END
    as JSON body content, so we must strip everything except the stats
    blocks before embedding board_output in the marker block.
    """
    lines = board_output.splitlines()
    kept: list[str] = []
    in_stats = False
    for line in lines:
        if line.startswith("NET_STATS_BEGIN"):
            in_stats = True
        if in_stats:
            kept.append(line)
        if line.startswith("NET_STATS_END"):
            in_stats = False
    return "\n".join(kept) + ("\n" if kept else "")


def emit_test_output(
    test_id: str,
    iteration: int,
    warmup: bool,
    client_json: dict[str, Any],
    board_output: str,
) -> str:
    """Format one iteration as a NET_BENCH_BEGIN/END block.

    The layout is compatible with core/summarize.py:
      NET_BENCH_BEGIN test=<id> iter=<n> warmup=<0|1>
      NET_STATS_BEGIN warmup=<0|1>
      <board /proc/net/dev before>
      NET_STATS_END
      <iperf3 -J JSON from PC client>
      NET_STATS_BEGIN warmup=<0|1>
      <board /proc/net/dev after>
      NET_STATS_END
      NET_BENCH_END test=<id> iter=<n>
    """
    warmup_flag = 1 if warmup else 0
    json_str = json.dumps(client_json, indent=2)
    stats_blocks = _extract_stats_blocks(board_output)

    return (
        f"NET_BENCH_BEGIN test={test_id} iter={iteration} warmup={warmup_flag}\n"
        f"{stats_blocks}"
        f"{json_str}\n"
        f"NET_BENCH_END test={test_id} iter={iteration}\n"
    )


# ===================================================================
# Config loading
# ===================================================================


def load_config(path: Path) -> dict[str, Any]:
    """Load TOML config, filling in defaults for missing sections."""
    if not path.is_file():
        raise FileNotFoundError(f"config file not found: {path}")
    with open(path, "rb") as fh:
        cfg = tomllib.load(fh)

    # Fill defaults.
    cfg.setdefault("board", {})
    cfg.setdefault("test", {})
    cfg["test"].setdefault("iperf3_port", 5201)
    cfg["test"].setdefault("duration", 5)
    cfg["test"].setdefault("warmup", 1)
    cfg["test"].setdefault("iters", 5)
    cfg["test"].setdefault("window", "256K")
    cfg["test"].setdefault("server_script", "/tmp/board-server.sh")
    cfg["test"].setdefault("iperf3_binary", "/tmp/iperf3")
    matrix = cfg["test"].get("matrix")
    if not matrix:
        cfg["test"]["matrix"] = dict(DEFAULT_MATRIX)
    return cfg


# ===================================================================
# Main
# ===================================================================


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=DEFAULT_CONFIG,
        help=f"path to board-config.toml (default: {DEFAULT_CONFIG})",
    )
    parser.add_argument(
        "--deploy",
        action="store_true",
        help="deploy board-server.sh and (optionally) iperf3 to the board before testing",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=RESULTS_DIR,
        help=f"directory for per-run result files (default: {RESULTS_DIR})",
    )
    parser.add_argument(
        "--test",
        type=str,
        default="",
        help="run only the named test (e.g. 'tcp1') instead of the full matrix",
    )
    parser.add_argument(
        "--no-summary",
        action="store_true",
        help="skip running summarize.py at the end",
    )
    args = parser.parse_args(argv)

    # ------------------------------------------------------------------
    # Load config
    # ------------------------------------------------------------------
    try:
        cfg = load_config(args.config)
    except Exception as exc:
        print(f"error: failed to load config: {exc}", file=sys.stderr)
        return 1

    # ------------------------------------------------------------------
    # Connect
    # ------------------------------------------------------------------
    try:
        board = BoardSession(cfg)
        board.connect()
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    exit_code = 0
    result_file: Path | None = None

    try:
        # --------------------------------------------------------------
        # Deploy (if requested)
        # --------------------------------------------------------------
        server_script = cfg["test"].get("server_script", "/tmp/board-server.sh")
        server_local = BOARD_DIR / "board-server.sh"

        if args.deploy:
            deployed_path = board.deploy(server_local, "/tmp/")
            # If user specified a different server_script location, update.
            if deployed_path != server_script:
                server_script = deployed_path
                cfg["test"]["server_script"] = server_script

        # Check that board-server.sh exists on the board.
        if not board.file_exists(server_script):
            print(
                f"[board] {server_script} not found on board; deploying automatically",
                file=sys.stderr,
            )
            board.deploy(server_local, "/tmp/")
            server_script = "/tmp/board-server.sh"

        # Check that iperf3 exists on the board and is >= 3.10
        # (iperf3 -s -1 requires 3.10+).
        iperf3_bin = cfg["test"].get("iperf3_binary", "/tmp/iperf3")
        iperf3_ver_out = ""
        for candidate in (iperf3_bin, "iperf3"):
            out, st = board.run_status(f"{candidate} --version", timeout=5)
            if st == 0:
                iperf3_bin = candidate
                iperf3_ver_out = out
                break
        else:
            print(
                f"[board] iperf3 not found on board; "
                f"deploy a riscv64-static iperf3 to {iperf3_bin} and re-run",
                file=sys.stderr,
            )
            return 1

        # Parse version and require >= 3.10 for -s -1.
        _ver_ok = _check_iperf3_version(iperf3_ver_out)
        if not _ver_ok:
            print(
                f"[board] iperf3 version too old (need >= 3.10 for -s -1). "
                f"Detected: {iperf3_ver_out.strip().splitlines()[0] if iperf3_ver_out else 'unknown'}",
                file=sys.stderr,
            )
            return 1

        # --------------------------------------------------------------
        # Setup
        # --------------------------------------------------------------
        port = int(cfg["test"]["iperf3_port"])
        duration = int(cfg["test"]["duration"])
        warmup_iters = int(cfg["test"]["warmup"])
        measured_iters = int(cfg["test"]["iters"])
        window = str(cfg["test"]["window"])

        matrix: dict[str, str] = cfg["test"]["matrix"]
        if args.test:
            matrix = {k: v for k, v in matrix.items() if k == args.test}
        if not matrix:
            print(
                f"error: no tests to run (requested: {args.test!r})",
                file=sys.stderr,
            )
            return 1

        total_iters = warmup_iters + measured_iters

        # Open result file.
        args.output.mkdir(parents=True, exist_ok=True)
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
        result_file = args.output / f"board-sg2002-{timestamp}.txt"

        with open(result_file, "w", encoding="utf-8") as fh:
            _write_fingerprint(fh, cfg, timestamp)

            runner = TestRunner(
                board,
                port=port,
                duration=duration,
                window=window,
                server_script=server_script,
            )

            # ----------------------------------------------------------
            # Run test matrix
            # ----------------------------------------------------------
            test_order = [t for t in DEFAULT_MATRIX if t in matrix]
            for test_id in test_order:
                extra_args = matrix[test_id]
                label = test_id  # human-readable label used in header

                # Warmup iterations.
                for i in range(warmup_iters):
                    print(
                        f"\n=== {label} warmup {i + 1}/{warmup_iters} ===",
                        file=sys.stderr,
                    )
                    result = runner.run_one(test_id, extra_args, warmup=True)
                    if result is None:
                        print(
                            f"[warn] {test_id} warmup {i} failed (ignored)",
                            file=sys.stderr,
                        )
                        continue
                    client_json, board_output = result
                    block = emit_test_output(
                        test_id, i, warmup=True,
                        client_json=client_json, board_output=board_output,
                    )
                    print(block)
                    fh.write(block + "\n")
                    fh.flush()

                # Measured iterations.
                for i in range(warmup_iters, total_iters):
                    meas_i = i - warmup_iters
                    print(
                        f"\n=== {label} iter {meas_i + 1}/{measured_iters} ===",
                        file=sys.stderr,
                    )
                    result = runner.run_one(test_id, extra_args, warmup=False)
                    if result is None:
                        print(
                            f"[error] {test_id} measured iter {meas_i} failed",
                            file=sys.stderr,
                        )
                        fh.write(
                            f"NET_BENCH_FAILED: {test_id} iteration {i}\n"
                        )
                        fh.flush()
                        exit_code = 1
                        continue
                    client_json, board_output = result
                    block = emit_test_output(
                        test_id, i, warmup=False,
                        client_json=client_json, board_output=board_output,
                    )
                    print(block)
                    fh.write(block + "\n")
                    fh.flush()

            if exit_code == 0:
                fh.write("NET_BENCH_PASSED\n")
            else:
                fh.write(
                    f"NET_BENCH_FAILED: {exit_code} measured iteration(s) failed\n"
                )
            fh.flush()

        if exit_code == 0:
            print("NET_BENCH_PASSED")
        else:
            print(f"NET_BENCH_FAILED: {exit_code} measured iteration(s) failed")

        print(f"\n[result] raw output saved to {result_file}", file=sys.stderr)

        # --------------------------------------------------------------
        # Summarize
        # --------------------------------------------------------------
        if not args.no_summary and result_file and result_file.is_file():
            summarize_py = CORE_DIR / "summarize.py"
            if summarize_py.is_file():
                print(
                    f"\n[summary] running {summarize_py.name} ...",
                    file=sys.stderr,
                )
                subprocess.run(
                    [sys.executable, str(summarize_py), str(result_file)],
                )

    except KeyboardInterrupt:
        print("\n[aborted]", file=sys.stderr)
        exit_code = 130
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        exit_code = 1
    finally:
        board.close()

    return exit_code


def _write_fingerprint(fh, cfg: dict[str, Any], timestamp: str) -> None:
    """Write an environment fingerprint section at the top of the result file."""
    board_cfg = cfg.get("board", {})
    test_cfg = cfg.get("test", {})
    lines = [
        "# net-bench SG2002 board environment fingerprint",
        f"timestamp  : {timestamp}",
        f"board_ip   : {board_cfg.get('ip', '?')}",
        f"ssh_user   : {board_cfg.get('ssh_user', '?')}",
        f"port       : {test_cfg.get('iperf3_port', '?')}",
        f"duration   : {test_cfg.get('duration', '?')}s",
        f"warmup     : {test_cfg.get('warmup', '?')}",
        f"iters      : {test_cfg.get('iters', '?')}",
        f"window     : {test_cfg.get('window', '?')}",
        f"pc_uname   : {os.uname().nodename}",
        f"pc_iperf3  : {_iperf3_version() or '?'}",
        "",
    ]
    fh.write("\n".join(lines) + "\n")
    print("".join(f"[fingerprint] {l}\n" for l in lines if l), end="", file=sys.stderr)


def _check_iperf3_version(version_output: str) -> bool:
    """Check that iperf3 version is >= 3.10 (required for -s -1)."""
    m = re.search(r"iperf\s+(\d+)\.(\d+)", version_output)
    if not m:
        return False
    major, minor = int(m.group(1)), int(m.group(2))
    return major > 3 or (major == 3 and minor >= 10)


def _iperf3_version() -> str | None:
    try:
        proc = subprocess.run(
            ["iperf3", "--version"], capture_output=True, text=True, timeout=5
        )
        return proc.stdout.splitlines()[0].strip() if proc.stdout else None
    except Exception:
        return None


if __name__ == "__main__":
    raise SystemExit(main())
