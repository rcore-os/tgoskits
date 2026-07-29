#!/bin/sh

PATH=/sbin:/bin:/usr/sbin:/usr/bin
export PATH

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true

echo QC_DUAL_GUEST_LINUX_INIT=START
printf 'QC_UNAME=%s\n' "$(uname -a)"
printf 'QC_NPROC=%s\n' "$(nproc)"
printf 'QC_CPUINFO_PROCESSORS=%s\n' "$(grep -c '^processor' /proc/cpuinfo)"
printf 'QC_CPU_ONLINE=%s\n' "$(cat /sys/devices/system/cpu/online)"
printf 'QC_MEMTOTAL_KB=%s\n' "$(awk '/MemTotal/ {print $2}' /proc/meminfo)"

wait_count=0
while [ ! -d /sys/class/net/eth0 ] && [ "${wait_count}" -lt 30 ]; do
    sleep 1
    wait_count=$((wait_count + 1))
done

if [ ! -d /sys/class/net/eth0 ]; then
    echo QC_DUAL_GUEST_NETWORK_CONFIG_RESULT=NO_ETH0
    exec /bin/sh
fi

ip link set dev eth0 up
ip address flush dev eth0
ip address add 192.0.2.10/24 dev eth0

if ip neigh replace 192.0.2.20 lladdr 52:54:00:12:34:20 dev eth0 nud permanent; then
    echo QC_STATIC_ARP_APPLY=PASS
else
    echo QC_STATIC_ARP_APPLY=FAIL_IP_NEIGH
    if command -v arp >/dev/null 2>&1 && arp -s 192.0.2.20 52:54:00:12:34:20; then
        echo QC_STATIC_ARP_APPLY=PASS_ARP_FALLBACK
    else
        echo QC_STATIC_ARP_APPLY=FAIL_ARP_FALLBACK
    fi
fi

echo QC_NET_LINK_BEGIN
ip link show dev eth0
echo QC_NET_ADDRESS_BEGIN
ip address show dev eth0
echo QC_NET_ROUTE_BEGIN
ip route list
echo QC_NET_NEIGH_FAST_BEGIN
ip neigh show dev eth0 2>/dev/null || true
printf 'QC_NET_IFACE=eth0 MAC=%s STATE=%s\n' \
    "$(cat /sys/class/net/eth0/address)" \
    "$(cat /sys/class/net/eth0/operstate)"
echo QC_STATIC_ARP=192.0.2.20,52:54:00:12:34:20,nud=permanent

: "${QC_LINUX_STRESS_WORKERS:=0}"
: "${QC_LINUX_STRESS_SECONDS:=0}"
stress_pids=""
stress_timer_pid=""

start_linux_stress() {
    echo QC_LINUX_STRESS_CONFIG_WORKERS="${QC_LINUX_STRESS_WORKERS}"
    echo QC_LINUX_STRESS_CONFIG_SECONDS="${QC_LINUX_STRESS_SECONDS}"

    if [ "${QC_LINUX_STRESS_WORKERS}" -le 0 ]; then
        echo QC_LINUX_STRESS_RESULT=SKIP
        return
    fi

    stress_index=0
    while [ "${stress_index}" -lt "${QC_LINUX_STRESS_WORKERS}" ]; do
        (
            while :; do
                :
            done
        ) &
        stress_pid=$!
        stress_pids="${stress_pids} ${stress_pid}"
        printf 'QC_LINUX_STRESS_PID=%s\n' "${stress_pid}"
        stress_index=$((stress_index + 1))
    done

    if [ "${QC_LINUX_STRESS_SECONDS}" -gt 0 ]; then
        (
            sleep "${QC_LINUX_STRESS_SECONDS}"
            for stress_pid in ${stress_pids}; do
                kill "${stress_pid}" 2>/dev/null || true
            done
        ) &
        stress_timer_pid=$!
        printf 'QC_LINUX_STRESS_TIMER_PID=%s\n' "${stress_timer_pid}"
    fi

    echo QC_LINUX_STRESS_RESULT=STARTED
}

stop_linux_stress() {
    if [ -n "${stress_timer_pid}" ]; then
        kill "${stress_timer_pid}" 2>/dev/null || true
        wait "${stress_timer_pid}" 2>/dev/null || true
        stress_timer_pid=""
    fi

    if [ -z "${stress_pids}" ]; then
        return
    fi

    for stress_pid in ${stress_pids}; do
        kill "${stress_pid}" 2>/dev/null || true
    done
    for stress_pid in ${stress_pids}; do
        wait "${stress_pid}" 2>/dev/null || true
    done
    stress_pids=""
    echo QC_LINUX_STRESS_RESULT=STOPPED
}

sleep 1

start_linux_stress

echo QC_RT_PERIODIC_PROBE=START
/qc-rt-probe
rt_status=$?
printf 'QC_RT_PERIODIC_PROBE_STATUS=%s\n' "${rt_status}"

echo QC_DUAL_GUEST_UDP_PROBE=START
/qc-udp-probe
probe_status=$?
printf 'QC_DUAL_GUEST_UDP_PROBE_STATUS=%s\n' "${probe_status}"

echo QC_QCZ1_GUEST_DEMO=START
/qc-qcz1-demo
demo_status=$?
printf 'QC_QCZ1_GUEST_DEMO_STATUS=%s\n' "${demo_status}"

stop_linux_stress

echo QC_NET_DEVICE_STATS_BEGIN
cat /proc/net/dev
echo QC_INTERRUPTS_BEGIN
cat /proc/interrupts

echo QC_RTOS_SETTLE_BEGIN
sleep 5
echo QC_RTOS_SETTLE_END

if [ "${rt_status}" -eq 0 ] && [ "${probe_status}" -eq 0 ] && [ "${demo_status}" -eq 0 ]; then
    echo QC_DUAL_GUEST_LINUX_INIT=PASS
else
    echo QC_DUAL_GUEST_LINUX_INIT=FAIL
fi

exec sleep 600
