#!/bin/bash
#
# riscv-h 代码检查脚本
# 下载并调用 axci 仓库中的检查脚本
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
COMPONENT_NAME="$(basename "$COMPONENT_DIR")"
AXCI_DIR="${SCRIPT_DIR}/.axci"
AXCI_REPO="https://github.com/arceos-hypervisor/axci.git"

# 下载或更新 axci 仓库
download_axci() {
    if [ -d "$AXCI_DIR" ]; then
        echo "Updating axci repository..."
        cd "$AXCI_DIR" && git pull --quiet
    else
        echo "Downloading axci repository..."
        git clone --quiet "$AXCI_REPO" "$AXCI_DIR"
    fi
}

# 主函数
main() {
    download_axci
    
    # 在组件目录中运行检查
    cd "$COMPONENT_DIR"
    exec bash "$AXCI_DIR/check.sh" --component-dir "$COMPONENT_DIR" "$@"
}

main "$@"
