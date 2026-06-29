#!/usr/bin/env bash
# env/teardown.sh - 自动回退清理所有配置
#
# 功能：
#   1. 读取状态文件 .bench-state.json
#   2. 停止所有启动的进程
#   3. 删除所有创建的网络资源
#   4. 清理状态文件

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE_FILE="${SCRIPT_DIR}/../.bench-state.json"

# 颜色输出
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# 检查 root 权限
check_root() {
    if [[ $EUID -ne 0 ]]; then
        error "需要 root 权限（使用 sudo）"
        exit 1
    fi
}

# 检查状态文件是否存在
check_state_file() {
    if [[ ! -f "$STATE_FILE" ]]; then
        warn "状态文件不存在: $STATE_FILE"
        warn "没有需要清理的资源"
        return 1
    fi
    return 0
}

# 停止进程
stop_processes() {
    info "停止启动的进程..."

    local process_count
    process_count=$(jq '.processes | length' "$STATE_FILE" 2>/dev/null || echo 0)

    if [[ "$process_count" -eq 0 ]]; then
        info "  没有需要停止的进程"
        return 0
    fi

    for i in $(seq 0 $((process_count - 1))); do
        local pid cmd
        pid=$(jq -r ".processes[$i].pid" "$STATE_FILE")
        cmd=$(jq -r ".processes[$i].cmd" "$STATE_FILE")

        if kill -0 "$pid" 2>/dev/null; then
            info "  停止进程 $pid: $cmd"
            kill "$pid" 2>/dev/null || warn "    无法停止进程 $pid"
            sleep 0.5

            # 如果进程仍在运行，强制终止
            if kill -0 "$pid" 2>/dev/null; then
                warn "    进程 $pid 未响应，强制终止"
                kill -9 "$pid" 2>/dev/null || true
            fi
        else
            info "  进程 $pid 已不存在"
        fi
    done
}

# 删除网络资源
cleanup_resources() {
    info "清理网络资源..."

    local resource_count
    resource_count=$(jq '.created_resources | length' "$STATE_FILE" 2>/dev/null || echo 0)

    if [[ "$resource_count" -eq 0 ]]; then
        info "  没有需要清理的资源"
        return 0
    fi

    # 按逆序删除（先删除依赖资源）
    for i in $(seq $((resource_count - 1)) -1 0); do
        local type name details
        type=$(jq -r ".created_resources[$i].type" "$STATE_FILE")
        name=$(jq -r ".created_resources[$i].name" "$STATE_FILE")
        details=$(jq -r ".created_resources[$i].details" "$STATE_FILE")

        case "$type" in
            tap)
                if ip link show "$name" >/dev/null 2>&1; then
                    info "  删除 TAP 设备: $name"
                    ip link set "$name" down 2>/dev/null || true
                    ip link delete "$name" 2>/dev/null || warn "    无法删除 $name"
                else
                    info "  TAP 设备 $name 已不存在"
                fi
                ;;
            bridge)
                if ip link show "$name" >/dev/null 2>&1; then
                    info "  删除 bridge: $name"
                    ip link set "$name" down 2>/dev/null || true
                    ip link delete "$name" 2>/dev/null || warn "    无法删除 $name"
                else
                    info "  Bridge $name 已不存在"
                fi
                ;;
            *)
                warn "  未知资源类型: $type ($name)"
                ;;
        esac
    done
}

# 清理状态文件
cleanup_state_file() {
    if [[ -f "$STATE_FILE" ]]; then
        info "清理状态文件: $STATE_FILE"
        rm -f "$STATE_FILE"
    fi
}

# 显示当前状态
show_status() {
    if ! check_state_file; then
        return 0
    fi

    info ""
    info "=== 当前配置状态 ==="

    local process_count resource_count
    process_count=$(jq '.processes | length' "$STATE_FILE" 2>/dev/null || echo 0)
    resource_count=$(jq '.created_resources | length' "$STATE_FILE" 2>/dev/null || echo 0)

    info "进程数: $process_count"
    if [[ "$process_count" -gt 0 ]]; then
        jq -r '.processes[] | "  PID \(.pid): \(.cmd)"' "$STATE_FILE"
    fi

    info "网络资源数: $resource_count"
    if [[ "$resource_count" -gt 0 ]]; then
        jq -r '.created_resources[] | "  \(.type): \(.name) (\(.details))"' "$STATE_FILE"
    fi

    info ""
}

# 主函数
main() {
    local action="${1:-teardown}"

    if [[ "$action" == "status" ]]; then
        show_status
        exit 0
    fi

    check_root

    info "=== net-bench 环境清理 ==="

    if ! check_state_file; then
        info "环境已清理或未配置"
        exit 0
    fi

    # 显示将要清理的内容
    show_status

    # 执行清理
    stop_processes
    cleanup_resources
    cleanup_state_file

    info ""
    info "=== 清理完成 ==="
}

# 设置退出时清理（如果脚本被中断）
trap 'error "清理过程被中断"' INT TERM

main "$@"
