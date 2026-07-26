#!/usr/bin/env bash
# Manage the isolated Linux TAP/NAT topology used by the AxVisor virtio-net test.

set -euo pipefail

# Administrative tools are commonly installed in sbin even when automation
# starts with a minimal root PATH.
PATH="${PATH}:/usr/local/sbin:/usr/sbin:/sbin"

BRIDGE="${AXVIRTIO_TAP_BRIDGE:-axvirtio-br0}"
TAP="${AXVIRTIO_TAP_DEVICE:-axvirtio-tap0}"
SUBNET="10.88.0.0/24"
GATEWAY="10.88.0.1"
DHCP_START="10.88.0.10"
DHCP_END="10.88.0.200"
NFT_TABLE="axvirtio_net_tap"
STATE_DIR="${AXVIRTIO_TAP_STATE_DIR:-/run/axvirtio-net-tap}"
STATE_FILE="${STATE_DIR}/state"
DNSMASQ_PID_FILE="${STATE_DIR}/dnsmasq.pid"
HTTP_PID_FILE="${STATE_DIR}/http.pid"
HTTP_PORT=18080
TEST_HOST="axvirtio.test"
TEST_TOKEN="AXVIRTIO_NET_TAP_OK"
UPLINK=""
TAP_OWNER="${SUDO_USER:-${USER:-root}}"
DRY_RUN=false

usage() {
    cat <<EOF
Usage:
  $0 setup --uplink <interface> [--tap-owner <user>] [--dry-run]
  $0 status [--dry-run]
  $0 teardown [--dry-run]
  $0 run --uplink <interface> [--tap-owner <user>] [--dry-run] -- <command...>

The setup creates ${BRIDGE} (${GATEWAY}/24), ${TAP}, a dedicated dnsmasq,
a token HTTP service, and nftables NAT scoped to table ip ${NFT_TABLE}.
The run command always tears the topology down after the child command exits.
EOF
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

info() {
    echo "[axvirtio-net-tap] $*"
}

run_cmd() {
    if ${DRY_RUN}; then
        printf '+ '
        printf '%q ' "$@"
        printf '\n'
    else
        "$@"
    fi
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

require_setup_environment() {
    [[ ${EUID} -eq 0 ]] || die "setup and teardown require root"
    [[ -c /dev/net/tun ]] || die "/dev/net/tun is unavailable"
    require_command ip
    require_command nft
    require_command dnsmasq
    require_command ethtool
    require_command python3
    [[ -n ${UPLINK} ]] || die "--uplink is required"
    [[ ${UPLINK} =~ ^[a-zA-Z0-9_.:-]+$ ]] || die "invalid uplink name: ${UPLINK}"
    ip link show dev "${UPLINK}" >/dev/null 2>&1 || die "uplink does not exist: ${UPLINK}"
    [[ "$(cat /sys/class/net/${UPLINK}/operstate)" == "up" ]] || die "uplink is not UP: ${UPLINK}"
    ip route show default dev "${UPLINK}" | grep -q '^default' \
        || die "uplink has no default route: ${UPLINK}"
}

print_topology() {
    info "bridge=${BRIDGE} gateway=${GATEWAY}/24 subnet=${SUBNET}"
    info "tap=${TAP} owner=${TAP_OWNER} uplink=${UPLINK:-unknown}"
    info "dns=${GATEWAY} (${TEST_HOST}) http=${GATEWAY}:${HTTP_PORT}/${TEST_TOKEN}"
    info "nft=ip/${NFT_TABLE} state=${STATE_DIR}"
}

write_state() {
    mkdir -p "${STATE_DIR}"
    cat >"${STATE_FILE}" <<EOF
UPLINK=${UPLINK}
TAP_OWNER=${TAP_OWNER}
IP_FORWARD_WAS=${IP_FORWARD_WAS}
EOF
}

read_state() {
    [[ -f ${STATE_FILE} ]] || return 1
    UPLINK="$(sed -n 's/^UPLINK=//p' "${STATE_FILE}")"
    TAP_OWNER="$(sed -n 's/^TAP_OWNER=//p' "${STATE_FILE}")"
    IP_FORWARD_WAS="$(sed -n 's/^IP_FORWARD_WAS=//p' "${STATE_FILE}")"
    [[ ${UPLINK} =~ ^[a-zA-Z0-9_.:-]+$ ]] || die "invalid uplink in state file"
    [[ ${TAP_OWNER} =~ ^[a-zA-Z0-9_.-]+$ ]] || die "invalid TAP owner in state file"
    [[ ${IP_FORWARD_WAS} == 0 || ${IP_FORWARD_WAS} == 1 ]] \
        || die "invalid forwarding value in state file"
}

pid_is_running() {
    local pid
    [[ -f $1 ]] || return 1
    pid="$(cat "$1")"
    [[ ${pid} =~ ^[0-9]+$ ]] || return 1
    kill -0 "${pid}" 2>/dev/null || return 1
    [[ -r /proc/${pid}/stat ]] || return 1
    [[ "$(awk '{ print $3 }' "/proc/${pid}/stat")" != Z ]]
}

start_services() {
    mkdir -p "${STATE_DIR}/web"
    printf '%s\n' "${TEST_TOKEN}" >"${STATE_DIR}/web/${TEST_TOKEN}"

    nohup dnsmasq \
        --no-daemon \
        --conf-file= \
        --interface="${BRIDGE}" \
        --bind-interfaces \
        --no-ping \
        --dhcp-range="${DHCP_START},${DHCP_END},255.255.255.0,1h" \
        --dhcp-option="3,${GATEWAY}" \
        --dhcp-option="6,${GATEWAY}" \
        --address="/${TEST_HOST}/${GATEWAY}" \
        --pid-file="${DNSMASQ_PID_FILE}" \
        --dhcp-leasefile="${STATE_DIR}/dnsmasq.leases" \
        --log-facility="${STATE_DIR}/dnsmasq.log" \
        </dev/null >>"${STATE_DIR}/dnsmasq.log" 2>&1 &
    echo $! >"${DNSMASQ_PID_FILE}"

    nohup python3 -m http.server "${HTTP_PORT}" \
        --bind "${GATEWAY}" \
        --directory "${STATE_DIR}/web" \
        </dev/null \
        >"${STATE_DIR}/http.log" 2>&1 &
    echo $! >"${HTTP_PID_FILE}"

    sleep 0.1
    pid_is_running "${DNSMASQ_PID_FILE}" || die "dnsmasq failed to start"
    pid_is_running "${HTTP_PID_FILE}" || die "HTTP token service failed to start"
}

setup_network() {
    print_topology
    if ${DRY_RUN}; then
        run_cmd ip link add name "${BRIDGE}" type bridge
        run_cmd ip tuntap add dev "${TAP}" mode tap user "${TAP_OWNER}"
        run_cmd ethtool -K "${BRIDGE}" tx off
        run_cmd nft add table ip "${NFT_TABLE}"
        info "would start dedicated dnsmasq and HTTP token service"
        return
    fi
    require_setup_environment
    if read_state; then
        info "topology is already managed; leaving it unchanged"
        status_network
        return
    fi
    ip link show dev "${BRIDGE}" >/dev/null 2>&1 \
        && die "bridge already exists but is not owned by this script: ${BRIDGE}"
    ip link show dev "${TAP}" >/dev/null 2>&1 \
        && die "TAP already exists but is not owned by this script: ${TAP}"
    nft list table ip "${NFT_TABLE}" >/dev/null 2>&1 \
        && die "nftables table already exists but is not owned by this script: ${NFT_TABLE}"

    IP_FORWARD_WAS="$(cat /proc/sys/net/ipv4/ip_forward)"
    write_state
    trap 'teardown_network' ERR INT TERM

    run_cmd ip link add name "${BRIDGE}" type bridge
    run_cmd ip address add "${GATEWAY}/24" dev "${BRIDGE}"
    run_cmd ip link set dev "${BRIDGE}" up
    run_cmd ip tuntap add dev "${TAP}" mode tap user "${TAP_OWNER}"
    run_cmd ip link set dev "${TAP}" master "${BRIDGE}"
    run_cmd ip link set dev "${TAP}" up
    # Local TCP replies can otherwise reach TAP with CHECKSUM_PARTIAL while the
    # AxVisor host virtio-net frontend intentionally negotiates no offloads.
    run_cmd ethtool -K "${BRIDGE}" tx off

    run_cmd nft add table ip "${NFT_TABLE}"
    run_cmd nft 'add chain ip axvirtio_net_tap forward { type filter hook forward priority filter; policy accept; }'
    run_cmd nft add rule ip "${NFT_TABLE}" forward iifname "${BRIDGE}" oifname "${UPLINK}" accept
    run_cmd nft add rule ip "${NFT_TABLE}" forward iifname "${UPLINK}" oifname "${BRIDGE}" ct state established,related accept
    run_cmd nft 'add chain ip axvirtio_net_tap postrouting { type nat hook postrouting priority srcnat; policy accept; }'
    run_cmd nft add rule ip "${NFT_TABLE}" postrouting ip saddr "${SUBNET}" oifname "${UPLINK}" masquerade
    printf '1\n' >/proc/sys/net/ipv4/ip_forward
    start_services
    trap - ERR INT TERM
    info "setup complete"
}

status_network() {
    if read_state; then
        info "state: managed"
        print_topology
    else
        info "state: absent"
    fi
    ${DRY_RUN} && return
    ip -brief link show dev "${BRIDGE}" 2>/dev/null || true
    ip -brief link show dev "${TAP}" 2>/dev/null || true
    nft list table ip "${NFT_TABLE}" 2>/dev/null || true
    pid_is_running "${DNSMASQ_PID_FILE}" && info "dnsmasq: running" || info "dnsmasq: stopped"
    pid_is_running "${HTTP_PID_FILE}" && info "http: running" || info "http: stopped"
}

stop_pid_file() {
    local pid_file="$1"
    if pid_is_running "${pid_file}"; then
        kill "$(cat "${pid_file}")"
    fi
    rm -f "${pid_file}"
}

teardown_network() {
    if ${DRY_RUN}; then
        print_topology
        run_cmd nft delete table ip "${NFT_TABLE}"
        run_cmd ip link delete dev "${TAP}"
        run_cmd ip link delete dev "${BRIDGE}" type bridge
        info "would restore the saved IPv4 forwarding state"
        return
    fi
    [[ ${EUID} -eq 0 ]] || die "setup and teardown require root"
    if ! read_state; then
        info "no managed topology exists"
        return
    fi
    stop_pid_file "${HTTP_PID_FILE}"
    stop_pid_file "${DNSMASQ_PID_FILE}"
    nft delete table ip "${NFT_TABLE}" 2>/dev/null || true
    ip link delete dev "${TAP}" 2>/dev/null || true
    ip link delete dev "${BRIDGE}" type bridge 2>/dev/null || true
    if [[ ${IP_FORWARD_WAS:-1} == 0 ]]; then
        printf '0\n' >/proc/sys/net/ipv4/ip_forward
    fi
    rm -rf "${STATE_DIR}"
    info "teardown complete"
}

COMMAND="${1:-}"
[[ -n ${COMMAND} ]] || { usage; exit 2; }
shift
CHILD_COMMAND=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --uplink) [[ $# -ge 2 ]] || die "--uplink requires a value"; UPLINK="$2"; shift 2 ;;
        --tap-owner) [[ $# -ge 2 ]] || die "--tap-owner requires a value"; TAP_OWNER="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        --) shift; CHILD_COMMAND=("$@"); break ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

case "${COMMAND}" in
    setup) setup_network ;;
    status) status_network ;;
    teardown) teardown_network ;;
    run)
        [[ ${#CHILD_COMMAND[@]} -gt 0 ]] || die "run requires a command after --"
        setup_network
        if ${DRY_RUN}; then
            run_cmd "${CHILD_COMMAND[@]}"
        else
            trap 'teardown_network' EXIT INT TERM
            "${CHILD_COMMAND[@]}"
        fi
        ;;
    -h|--help) usage ;;
    *) usage; die "unknown command: ${COMMAND}" ;;
esac
