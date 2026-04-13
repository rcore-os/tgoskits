#!/usr/bin/env bash
set -euo pipefail

state_dir="${TGOS_DEBUG_STATE_DIR:-}"
session="${TGOS_DEBUG_SESSION:-}"

if [[ -z "${state_dir}" || -z "${session}" ]]; then
    echo "missing TGOS_DEBUG_STATE_DIR or TGOS_DEBUG_SESSION" >&2
    exit 2
fi

port="${TGOS_DEBUG_PORT:-1234}"
log_file="${state_dir}/${session}.log"
pid_file="${state_dir}/${session}.pid"
pgid_file="${state_dir}/${session}.pgid"

cleanup() {
    if [[ -f "${pgid_file}" ]]; then
        local pgid
        pgid="$(<"${pgid_file}")"
        if [[ -n "${pgid}" ]] && kill -0 "-${pgid}" 2>/dev/null; then
            kill "-${pgid}" 2>/dev/null || true
        fi
        rm -f "${pgid_file}"
    fi

    if [[ -f "${pid_file}" ]]; then
        local pid
        pid="$(<"${pid_file}")"
        if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
            kill "${pid}" 2>/dev/null || true
        fi
        rm -f "${pid_file}"
    fi
}

wait_for_port() {
    for _ in $(seq 1 200); do
        if python3 - "${port}" <<'PY' >/dev/null 2>&1
import socket
import sys

port = int(sys.argv[1])
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.settimeout(0.2)
try:
    sys.exit(0 if sock.connect_ex(("127.0.0.1", port)) == 0 else 1)
finally:
    sock.close()
PY
        then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

cmd="${1:-}"
case "${cmd}" in
    start)
        debug_command="${TGOS_DEBUG_COMMAND:-}"
        if [[ -z "${debug_command}" ]]; then
            echo "missing TGOS_DEBUG_COMMAND" >&2
            exit 2
        fi

        mkdir -p "${state_dir}"
        cleanup
        printf 'QEMU_DEBUG_STARTING session=%s port=%s\n' "${session}" "${port}"
        trap 'cleanup' INT TERM EXIT

        setsid bash -lc "${debug_command}" >"${log_file}" 2>&1 &
        child_pid=$!
        printf '%s\n' "${child_pid}" >"${pid_file}"
        child_pgid="$(ps -o pgid= -p "${child_pid}" | tr -d ' ')"
        printf '%s\n' "${child_pgid}" >"${pgid_file}"

        if wait_for_port; then
            printf 'QEMU_GDB_READY session=%s port=%s pid=%s log=%s\n' \
                "${session}" "${port}" "${child_pid}" "${log_file}"
            trap - INT TERM EXIT
            exit 0
        fi

        printf 'QEMU_DEBUG_FAILED session=%s log=%s\n' "${session}" "${log_file}"
        tail -n 80 "${log_file}" || true
        cleanup
        exit 1
        ;;
    stop)
        cleanup
        printf 'QEMU_DEBUG_STOPPED session=%s\n' "${session}"
        ;;
    *)
        echo "Usage: session.sh <start|stop>" >&2
        exit 2
        ;;
esac
