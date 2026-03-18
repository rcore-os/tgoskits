#!/bin/bash
#
# aarch64_sysreg 测试脚本
# 下载并调用 axci 仓库中的测试框架
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
AXCI_DIR="${SCRIPT_DIR}/.axci"
AXCI_REPO="https://github.com/arceos-hypervisor/axci.git"

# 下载或更新 axci 仓库
download_axci() {
    if [ -d "$AXCI_DIR" ]; then
        echo "Updating axci repository..."
        cd "$AXCI_DIR" && git pull --quiet
    else
        echo "Downloading axci repository..."
        git clone --quiet -b ndev "$AXCI_REPO" "$AXCI_DIR"
    fi
}

# 主函数
main() {
    download_axci
    
    # 在组件目录中运行测试，自动指定当前组件
    cd "$COMPONENT_DIR"
    exec bash "$AXCI_DIR/tests.sh" --component-dir "$COMPONENT_DIR" "$@"
}

main "$@"