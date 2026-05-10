#!/usr/bin/env python3
"""TGOS VS Code debug session manager.

Usage:  session.py <start|stop>

Environment:
    TGOS_DEBUG_STATE_DIR   Directory for per-session state files.
    TGOS_DEBUG_SESSION     Session name (e.g. arceos, axvisor, starry).
    TGOS_DEBUG_COMMAND     Shell command to build & launch QEMU  (start only).
    TGOS_DEBUG_PORT        GDB stub port  (default: 1234).
    TGOS_DEBUG_TEE_OUTPUT  Mirror QEMU stdout to the terminal too  (default: 1).
"""

import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

try:
    import pty
except ImportError:
    pty = None

# ---------------------------------------------------------------------------
# Configuration from environment
# ---------------------------------------------------------------------------

_state_dir_str = os.environ.get("TGOS_DEBUG_STATE_DIR", "")
_session       = os.environ.get("TGOS_DEBUG_SESSION", "")

if not _state_dir_str or not _session:
    print("missing TGOS_DEBUG_STATE_DIR or TGOS_DEBUG_SESSION", file=sys.stderr)
    sys.exit(2)

_port      = int(os.environ.get("TGOS_DEBUG_PORT", "1234"))
_tee       = os.environ.get("TGOS_DEBUG_TEE_OUTPUT", "1") == "1"
_state_dir = Path(_state_dir_str)
_log_file  = _state_dir / f"{_session}.log"
_pid_file  = _state_dir / f"{_session}.pid"
_pgid_file = _state_dir / f"{_session}.pgid"
_proc_root = Path("/proc")
_has_procfs = _proc_root.is_dir()
_has_pty = pty is not None
_has_process_groups = hasattr(os, "getpgid")
_taskkill = shutil.which("taskkill")
_new_process_group_flag = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)

# ---------------------------------------------------------------------------
# /proc helpers  (Linux-only; no external commands needed)
# ---------------------------------------------------------------------------

def _read_bytes(path: str) -> bytes:
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return b""


def _all_pids() -> list[int]:
    if not _has_procfs:
        return []
    try:
        return [int(n.name) for n in _proc_root.iterdir() if n.name.isdigit()]
    except OSError:
        return []


def _pgid_of(pid: int) -> int | None:
    """Return the process group ID of *pid* via getpgid(2)."""
    if not _has_process_groups:
        return None
    try:
        return os.getpgid(pid)
    except OSError:
        return None


def _exe_basename(pid: int) -> str:
    """Return the basename of argv[0] from /proc/<pid>/cmdline."""
    data = _read_bytes(f"/proc/{pid}/cmdline")
    return os.path.basename(data.split(b"\x00")[0]).decode(errors="replace") if data else ""


def _proc_env(pid: int) -> dict[str, str]:
    """Parse /proc/<pid>/environ into a key→value dict."""
    env: dict[str, str] = {}
    for entry in _read_bytes(f"/proc/{pid}/environ").split(b"\x00"):
        if b"=" in entry:
            k, _, v = entry.partition(b"=")
            env[k.decode(errors="replace")] = v.decode(errors="replace")
    return env


# ---------------------------------------------------------------------------
# Session-aware process queries
# ---------------------------------------------------------------------------

def _belongs_to_session(pid: int) -> bool:
    """True if *pid* carries the current session's environment markers."""
    env = _proc_env(pid)
    return (
        env.get("TGOS_DEBUG_SESSION") == _session
        and env.get("TGOS_DEBUG_STATE_DIR") == _state_dir_str
    )


def _has_qemu_in_group(pgid: int) -> bool:
    """True if any qemu-system-* process belongs to process group *pgid*."""
    return any(
        _pgid_of(pid) == pgid and _exe_basename(pid).startswith("qemu-system-")
        for pid in _all_pids()
    )


def _port_owned_by_group(pgid: int) -> bool:
    """True if the GDB stub port is held by a socket in process group *pgid*.

    Reads /proc/net/tcp[6] to find LISTEN inodes for *_port*, then checks
    whether any fd symlink of a process in *pgid* points to one of them.
    """
    target_hex = f"{_port:04X}"

    # Collect socket inodes listening on _port (kernel state 0A = TCP_LISTEN).
    inodes: set[int] = set()
    for net_path in ("/proc/net/tcp", "/proc/net/tcp6"):
        for line in _read_bytes(net_path).decode(errors="replace").splitlines()[1:]:
            fields = line.split()
            # layout: idx local_addr remote_addr state ... inode (field 9)
            if (
                len(fields) >= 10
                and fields[1].rsplit(":", 1)[-1].upper() == target_hex
                and fields[3] == "0A"
            ):
                inodes.add(int(fields[9]))

    if not inodes:
        return False

    # Find a process in *pgid* whose fd symlink matches one of those inodes.
    for pid in _all_pids():
        if _pgid_of(pid) != pgid:
            continue
        try:
            for fd in os.listdir(f"/proc/{pid}/fd"):
                try:
                    link = os.readlink(f"/proc/{pid}/fd/{fd}")
                    # Socket fds look like "socket:[<inode>]".
                    if link.startswith("socket:[") and int(link[8:-1]) in inodes:
                        return True
                except OSError:
                    pass
        except OSError:
            pass
    return False


def _tcp_connectable() -> bool:
    """Try a non-blocking TCP connection to the GDB stub port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.2)
        return s.connect_ex(("127.0.0.1", _port)) == 0


def _wait_for_qemu_ready(
    pgid: int,
    proc: subprocess.Popen,
    timeout: float = 20.0,
    interval: float = 0.1,
) -> bool:
    """Poll until QEMU is running and its GDB port accepts connections."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return False
        if _has_procfs and _has_process_groups:
            if _has_qemu_in_group(pgid) and _port_owned_by_group(pgid) and _tcp_connectable():
                return True
        elif _tcp_connectable():
            return True
        time.sleep(interval)
    return False


# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

def _group_alive(pgid: int) -> bool:
    if not _has_process_groups:
        return False
    try:
        os.kill(-pgid, 0)
        return True
    except OSError:
        return False


def _kill_group(pgid: int, sig: signal.Signals = signal.SIGTERM) -> None:
    if not _has_process_groups:
        return
    try:
        os.kill(-pgid, sig)
    except OSError:
        pass


def _kill_pid_tree(pid: int) -> None:
    if not _taskkill:
        return
    subprocess.run(
        [_taskkill, "/PID", str(pid), "/T", "/F"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )


def _orphaned_pgids() -> list[int]:
    """Return PGIDs of session processes that are not part of the current group."""
    if not _has_procfs or not _has_process_groups:
        return []
    my_pgid = os.getpgid(os.getpid())
    found: set[int] = set()
    for pid in _all_pids():
        if pid == os.getpid():
            continue
        if _belongs_to_session(pid):
            pgid = _pgid_of(pid)
            if pgid is not None and pgid != my_pgid:
                found.add(pgid)
    return list(found)


def _kill_orphans() -> None:
    """SIGTERM orphaned session groups; SIGKILL any that survive 2 s."""
    orphans = _orphaned_pgids()
    if not orphans:
        return
    for pgid in orphans:
        _kill_group(pgid)
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        if not any(_group_alive(p) for p in orphans):
            return
        time.sleep(0.1)
    for pgid in orphans:
        _kill_group(pgid, signal.SIGKILL)


def _cleanup() -> None:
    """Terminate the tracked process group and sweep up orphaned session groups."""
    if not _has_process_groups:
        if _pid_file.exists():
            try:
                pid = int(_pid_file.read_text().strip())
                _kill_pid_tree(pid)
            except (ValueError, OSError):
                pass
            _pid_file.unlink(missing_ok=True)
        _pgid_file.unlink(missing_ok=True)
        return

    # Try the whole PGID first; fall back to the bare PID if the pgid_file is gone.
    for path, use_pgid in ((_pgid_file, True), (_pid_file, False)):
        if path.exists():
            try:
                xid = int(path.read_text().strip())
                os.kill(-xid if use_pgid else xid, signal.SIGTERM)
            except (ValueError, OSError):
                pass
            path.unlink(missing_ok=True)
    _kill_orphans()


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def _start_stdin_forwarder(write_chunk: callable) -> threading.Thread | None:
    """Forward terminal input to the child process if stdin is available."""
    try:
        stdin_fd = sys.stdin.fileno()
    except (AttributeError, OSError):
        return None

    def _stdin_loop() -> None:
        try:
            while chunk := os.read(stdin_fd, 1024):
                write_chunk(chunk)
        except OSError:
            pass

    thread = threading.Thread(target=_stdin_loop, daemon=True)
    thread.start()
    return thread

def _cmd_start(debug_command: str) -> int:
    _state_dir.mkdir(parents=True, exist_ok=True)
    _cleanup()  # Evict any stale session from a previous run.
    print(f"QEMU_DEBUG_STARTING session={_session} port={_port}", flush=True)

    log_fh = open(_log_file, "wb")
    input_thread: threading.Thread | None = None
    master_fd: int | None = None

    if _tee and _has_pty:
        # Allocate a PTY so cargo / QEMU see a real terminal on their stdout.
        # Without a PTY, stdout is a PIPE and applications switch to fully-
        # buffered mode (~8 KB), which delays or completely hides output in
        # the VS Code task terminal.
        # start_new_session=True creates a new process group so we can signal
        # QEMU and all its children as a unit via kill(-pgid, ...).
        master_fd, slave_fd = pty.openpty()
        proc = subprocess.Popen(
            debug_command,
            shell=True,
            start_new_session=_has_process_groups,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            close_fds=True,
        )
        os.close(slave_fd)  # Parent only needs the master end.

        def _tee_loop() -> None:
            try:
                while chunk := os.read(master_fd, 4096):
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                    log_fh.write(chunk)
                    log_fh.flush()
            except OSError:
                pass  # EIO once all slave-side fds are closed (QEMU exited).

        tee_thread: threading.Thread | None = threading.Thread(target=_tee_loop, daemon=True)
        tee_thread.start()
        input_thread = _start_stdin_forwarder(lambda chunk: os.write(master_fd, chunk))
    else:
        # Fall back to pipe-based teeing when PTY support is unavailable.
        stdout_target = subprocess.PIPE if _tee else log_fh
        proc = subprocess.Popen(
            debug_command,
            shell=True,
            start_new_session=_has_process_groups,
            stdin=subprocess.PIPE,
            stdout=stdout_target,
            stderr=subprocess.STDOUT,
            bufsize=0,
            creationflags=_new_process_group_flag if not _has_process_groups else 0,
        )
        if _tee:
            assert proc.stdout is not None

            def _pipe_tee_loop() -> None:
                try:
                    while chunk := proc.stdout.read(4096):
                        sys.stdout.buffer.write(chunk)
                        sys.stdout.buffer.flush()
                        log_fh.write(chunk)
                        log_fh.flush()
                finally:
                    proc.stdout.close()

            tee_thread = threading.Thread(target=_pipe_tee_loop, daemon=True)
            tee_thread.start()
        else:
            tee_thread = None

        if proc.stdin is not None:
            input_thread = _start_stdin_forwarder(
                lambda chunk: (proc.stdin.write(chunk), proc.stdin.flush())
            )

    child_pgid = _pgid_of(proc.pid) or proc.pid
    _pid_file.write_text(str(proc.pid))
    _pgid_file.write_text(str(child_pgid))

    def _on_signal(signum: int, _frame: object) -> None:
        _cleanup()
        sys.exit(1)  # log_fh / master_fd are closed by the OS on process exit.

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    if _wait_for_qemu_ready(child_pgid, proc):
        print(
            f"QEMU_GDB_READY session={_session} port={_port}"
            f" pid={proc.pid} log={_log_file}",
            flush=True,
        )
        proc.wait()
        if tee_thread:
            tee_thread.join(timeout=2)
        if input_thread:
            input_thread.join(timeout=0.2)
        if master_fd is not None:
            try:
                os.close(master_fd)
            except OSError:
                pass
        log_fh.close()
        # QEMU exited cleanly (or was stopped by `stop`).  All processes in its
        # group are already gone; clear state files and skip the orphan scan.
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        signal.signal(signal.SIGTERM, signal.SIG_DFL)
        _pid_file.unlink(missing_ok=True)
        _pgid_file.unlink(missing_ok=True)
        return 0

    print(f"QEMU_DEBUG_FAILED session={_session} log={_log_file}", flush=True)
    try:
        print("\n".join(_log_file.read_text(errors="replace").splitlines()[-80:]), flush=True)
    except OSError:
        pass
    if master_fd is not None:
        try:
            os.close(master_fd)
        except OSError:
            pass
    log_fh.close()
    _cleanup()
    return 1


def _cmd_stop() -> int:
    _cleanup()
    print(f"QEMU_DEBUG_STOPPED session={_session}", flush=True)
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    match sys.argv[1] if len(sys.argv) > 1 else "":
        case "start":
            cmd = os.environ.get("TGOS_DEBUG_COMMAND", "")
            if not cmd:
                print("missing TGOS_DEBUG_COMMAND", file=sys.stderr)
                sys.exit(2)
            sys.exit(_cmd_start(cmd))
        case "stop":
            sys.exit(_cmd_stop())
        case _:
            print("Usage: session.py <start|stop>", file=sys.stderr)
            sys.exit(2)
