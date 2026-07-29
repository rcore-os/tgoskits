#!/usr/bin/env bash

set -euo pipefail

workspace="${1:-/home/kali/qc-zephyrproject}"
udp_probe="${2:-/tmp/qc-udp-echo-probe.py}"
duration_seconds="${3:-35}"
stamp="${4:-$(date +%Y-%m-%d_%H-%M-%S)-$$}"
host_udp_port="${5:-14242}"
serial_port="${6:-14444}"

build_dir="${7:-${workspace}/build/echo_server_virtio_net_bus23_fixed_0x90000000_mgmt_stack_2048}"
gdb_port="${8:-}"
zephyr_elf="${build_dir}/zephyr/zephyr.elf"
sdk="${workspace}/zephyr-sdk-1.0.1"
qemu="${sdk}/hosttools/sysroots/x86_64-pokysdk-linux/usr/bin/qemu-system-aarch64"
gdb="${sdk}/gnu/aarch64-zephyr-elf/bin/aarch64-zephyr-elf-gdb"
qemu_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-qemu.log"
serial_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-serial.log"
udp_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-udp.log"
qmp_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-qmp.log"
state_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-state.log"
state_mem="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-state-memory.bin"
gdb_log="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-gdb.log"
pidfile="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb.pid"
qmp_socket="/tmp/${stamp}_native-zephyr-mgmt-stack-2048-nogdb-qmp.sock"
qemu_pid=""
serial_pid=""
gdb_args=()

if [[ -n "${gdb_port}" ]]; then
    gdb_args=(-gdb "tcp:127.0.0.1:${gdb_port}")
fi

cleanup() {
    if [[ -n "${serial_pid}" ]] && kill -0 "${serial_pid}" 2>/dev/null; then
        kill "${serial_pid}" 2>/dev/null || true
        wait "${serial_pid}" 2>/dev/null || true
    fi
    if [[ -n "${qemu_pid}" ]] && kill -0 "${qemu_pid}" 2>/dev/null; then
        kill "${qemu_pid}" 2>/dev/null || true
        wait "${qemu_pid}" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

if pgrep -f '[q]emu-system-aarch64' >/dev/null; then
    echo "A QEMU process is already running; refusing concurrent execution." >&2
    exit 10
fi

for path in "${zephyr_elf}" "${qemu}" "${udp_probe}"; do
    if [[ ! -e "${path}" ]]; then
        echo "Required path is missing: ${path}" >&2
        exit 11
    fi
done

"${qemu}" \
    -global virtio-mmio.force-legacy=false \
    -cpu cortex-a53 \
    -machine virt,gic-version=3 \
    -m 2G \
    -device virtio-net-device,netdev=net0,bus=virtio-mmio-bus.23 \
    -netdev "user,id=net0,net=192.0.2.0/24,host=192.0.2.2,hostfwd=udp:127.0.0.1:${host_udp_port}-192.0.2.20:4242" \
    -pidfile "${pidfile}" \
    -display none \
    -monitor none \
    -serial "tcp:127.0.0.1:${serial_port},server=on,wait=off" \
    "${gdb_args[@]}" \
    -icount shift=4,align=off,sleep=on \
    -rtc clock=vm \
    -S \
    -qmp "unix:${qmp_socket},server=on,wait=off" \
    -kernel "${zephyr_elf}" \
    >"${qemu_log}" 2>&1 &
qemu_pid=$!

for _ in $(seq 1 100); do
    if ! kill -0 "${qemu_pid}" 2>/dev/null; then
        echo "QEMU exited before its serial socket became ready." >&2
        cat "${qemu_log}" >&2 || true
        exit 12
    fi
    if ss -ltn 2>/dev/null | grep -Fq "127.0.0.1:${serial_port}" &&
       [[ -S "${qmp_socket}" ]]; then
        break
    fi
    sleep 0.1
done

if ! ss -ltn 2>/dev/null | grep -Fq "127.0.0.1:${serial_port}"; then
    echo "QEMU serial server did not become ready." >&2
    exit 13
fi
if [[ ! -S "${qmp_socket}" ]]; then
    echo "QEMU QMP server did not become ready." >&2
    exit 14
fi

{
    sleep 5
    printf '\r\nkernel uptime\r\n'
    sleep "$((duration_seconds - 12))"
    printf '\r\nkernel uptime\r\n'
    sleep 5
} | timeout --signal=TERM --kill-after=2s "$((duration_seconds + 3))" \
    nc 127.0.0.1 "${serial_port}" >"${serial_log}" 2>&1 &
serial_pid=$!

python3 - "${qmp_socket}" >"${qmp_log}" 2>&1 <<'PY'
import json
import socket
import sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(2.0)
sock.connect(sys.argv[1])
stream = sock.makefile("rwb", buffering=0)

greeting = json.loads(stream.readline())
print(json.dumps(greeting, sort_keys=True))
for command in ("qmp_capabilities", "cont"):
    stream.write((json.dumps({"execute": command}) + "\r\n").encode())
    while True:
        response = json.loads(stream.readline())
        print(json.dumps(response, sort_keys=True))
        if "return" in response or "error" in response:
            if "error" in response:
                raise RuntimeError(response["error"])
            break
PY

set +e
sleep 8
python3 "${udp_probe}" \
    --host 127.0.0.1 \
    --port "${host_udp_port}" \
    --count 20 \
    --warmup 2 \
    --timeout 1.0 \
    --interval 0.05 \
    --prefix "${stamp}" \
    >"${udp_log}" 2>&1
udp_status=$?
wait "${serial_pid}"
serial_status=$?
set -e
serial_pid=""

fatal_marker=0
if grep -aEiq 'FATAL ERROR|CPU exception|ESR:|ELR:|data abort|stack overflow' "${serial_log}"; then
    fatal_marker=1
fi

uptime_samples="$(grep -a -c 'Uptime:' "${serial_log}" || true)"
qemu_alive_after_test=0
if [[ -n "${qemu_pid}" ]] && kill -0 "${qemu_pid}" 2>/dev/null; then
    qemu_alive_after_test=1
fi

echo "qemu_log=${qemu_log}"
echo "serial_log=${serial_log}"
echo "qmp_log=${qmp_log}"
echo "udp_log=${udp_log}"
echo "serial_status=${serial_status}"
echo "udp_status=${udp_status}"
echo "fatal_marker=${fatal_marker}"
echo "uptime_samples=${uptime_samples}"
echo "qemu_alive_after_test=${qemu_alive_after_test}"
tail -4 "${udp_log}" || true
grep -a 'Uptime:' "${serial_log}" || true

if [[ "${udp_status}" -eq 0 &&
      "${fatal_marker}" -eq 0 &&
      "${uptime_samples}" -ge 2 &&
      "${qemu_alive_after_test}" -eq 1 ]]; then
    echo "result=PASS"
    exit 0
fi

if [[ -n "${qemu_pid}" ]] && kill -0 "${qemu_pid}" 2>/dev/null; then
    python3 - "${qmp_socket}" "${state_mem}" >"${state_log}" 2>&1 <<'PY'
import json
import re
import socket
import sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(3.0)
sock.connect(sys.argv[1])
stream = sock.makefile("rwb", buffering=0)

print(json.dumps(json.loads(stream.readline()), sort_keys=True))

def execute(command, arguments=None):
    request = {"execute": command}
    if arguments is not None:
        request["arguments"] = arguments
    stream.write((json.dumps(request) + "\r\n").encode())
    while True:
        response = json.loads(stream.readline())
        print(json.dumps(response, sort_keys=True))
        if "return" in response or "error" in response:
            return response

execute("qmp_capabilities")
execute("stop")
execute("query-status")
execute("query-cpus-fast")
register_response = execute(
    "human-monitor-command", {"command-line": "info registers"}
)
register_text = register_response.get("return", "")
execute("human-monitor-command", {"command-line": "info registers -a"})
for command_line in ("info cpus", "info irq"):
    execute("human-monitor-command", {"command-line": command_line})

registers = {}
for name in ("PC", "X22", "SP", "X29", "X30"):
    match = re.search(rf"\b{name}=([0-9A-Fa-f]+)", register_text)
    if match:
        registers[name] = int(match.group(1), 16)

for name, address in registers.items():
    execute(
        "human-monitor-command",
        {"command-line": f"x /32gx 0x{address:x}"},
    )

esf = registers.get("X22")
if esf is not None:
    start = max(0, esf - 0x100)
    execute(
        "human-monitor-command",
        {
            "command-line": (
                f'pmemsave 0x{start:x} 0x400 "{sys.argv[2]}"'
            )
        },
    )
PY
    echo "state_log=${state_log}"
    if [[ -f "${state_mem}" ]]; then
        echo "state_mem=${state_mem}"
    fi
    tail -80 "${state_log}" || true

    if [[ -n "${gdb_port}" && -x "${gdb}" ]]; then
        timeout --signal=TERM --kill-after=2s 15s \
            "${gdb}" -q -batch "${zephyr_elf}" \
            -ex "set pagination off" \
            -ex "set print pretty on" \
            -ex "target remote 127.0.0.1:${gdb_port}" \
            -ex "info registers" \
            -ex "info all-registers" \
            -ex "bt" \
            -ex "p/x &_kernel" \
            -ex "p/x _kernel.cpus[0].current" \
            -ex "p/x _kernel.cpus[0].nested" \
            -ex "p/x _kernel.ready_q.cache" \
            -ex "p/x ((struct k_thread *)_kernel.cpus[0].current)->callee_saved" \
            -ex "p/x z_main_thread.callee_saved" \
            -ex "x/48gx &_kernel" \
            -ex "x/64gx _kernel.cpus[0].current" \
            -ex "x/32gx \$x22" \
            -ex "x/96gx 0x900aaa10" \
            -ex "x/32gx 0x900ab0c0" \
            >"${gdb_log}" 2>&1 || true
        echo "gdb_log=${gdb_log}"
        grep -Eai '(^|[[:space:]])(esr|far|elr|spsr|currentel|daif)' "${gdb_log}" ||
            tail -80 "${gdb_log}" || true
    fi
fi

echo "result=FAIL"
exit 1
