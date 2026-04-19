#!/bin/bash
#
# AxVisor Environment Setup and Launch Script
# Supported platforms: qemu-aarch64, qemu-riscv64, qemu-x86_64, phytiumpi, roc-rk3568-pc, rdk-s100, rdk-s100p
# Documentation: https://arceos-hypervisor.github.io/axvisorbook/docs/quickstart
#

set -e  # Exit on error

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Execute command and display it
run_cmd() {
    echo -e "${BLUE}$@${NC}"
    "$@"
}

# Check if running in AxVisor root directory
check_root_dir() {
    if [ ! -f "Cargo.toml" ]; then
        error "Cargo.toml not found. Please run this script in the AxVisor root directory."
        exit 1
    fi

    if [ ! -d "configs" ]; then
        error "configs directory not found. Please run this script in the AxVisor root directory."
        exit 1
    fi

    # Check if package name in Cargo.toml is axvisor
    if ! grep -q '^\[package\]' Cargo.toml; then
        error "Invalid Cargo.toml format. Please run this script in the AxVisor root directory."
        exit 1
    fi

    # Extract package name and check
    local package_name
    package_name=$(awk '/^\[package\]/{flag=1} flag && /^name\s*=/ {gsub(/["'\'']/, "", $3); print $3; exit}' Cargo.toml)
    if [ "$package_name" != "axvisor" ]; then
        error "Current project is not AxVisor (package name: $package_name). Please run this script in the AxVisor root directory."
        exit 1
    fi
}

# Show usage help
show_help() {
    cat << EOF
Usage: $0 <platform> <command> [options]

AxVisor Environment Setup and Launch Script

Platforms:
    qemu-aarch64       QEMU AArch64 (ArceOS/Linux)
    qemu-riscv64       QEMU RISC-V64 (ArceOS)
    qemu-x86_64        QEMU x86_64 (NimbOS)
    phytiumpi          Phytium Pi Board (ArceOS/Linux)
    roc-rk3568-pc      ROC-RK3568-PC Board (ArceOS/Linux)
    rdk-s100           D-Robotics RDK S100P Board (ArceOS/Linux)
    rdk-s100p          Alias of rdk-s100

Commands:
    setup               Prepare environment (download images, config files, etc.)
    run                 Launch AxVisor (requires setup first)
    start               One-step setup + launch (recommended)

Setup Options (for setup/start commands):
    --serial <device>   Specify serial device
                        Required for board platforms (phytiumpi, roc-rk3568-pc, rdk-s100)
                        If not specified, setup will prepare config but NOT launch

Launch Options (for run/start commands):
    QEMU AArch64:
        -a, --arceos        Launch single ArceOS guest (default)
        -l, --linux         Launch single Linux guest
        -m, --multi         Launch multiple guests (ArceOS+Linux)
    QEMU RISC-V64:
        -a, --arceos        Launch single ArceOS guest (default)
    QEMU x86_64:
        -n, --nimbos        Launch single NimbOS guest (default)
    Phytium Pi:
        -a, --arceos        Launch single ArceOS guest (default)
        -l, --linux         Launch single Linux guest
        -m, --multi         Launch multiple guests (ArceOS+Linux)
    ROC-RK3568-PC:
        -a, --arceos        Launch single ArceOS guest (default)
        -l, --linux         Launch single Linux guest
        -m, --multi         Launch multiple guests (ArceOS+Linux)
    RDK S100P:
        -a, --arceos        Launch single ArceOS guest (default)
        -l, --linux         Launch single Linux guest
        -m, --multi         Launch multiple guests (ArceOS+Linux)
    -h, --help              Show this help message

Examples:
    # QEMU AArch64
    $0 qemu-aarch64 start --arceos                  # One-step: prepare + launch ArceOS
    $0 qemu-aarch64 start --linux                   # One-step: prepare + launch Linux
    $0 qemu-aarch64 start --multi                   # One-step: prepare + launch multiple guests

    # QEMU RISC-V64
    $0 qemu-riscv64 start --arceos                  # One-step: prepare + launch ArceOS

    # QEMU x86_64
    $0 qemu-x86_64 start --nimbos                   # One-step: prepare + launch NimbOS
    $0 qemu-x86_64 start                            # Same as above (default nimbos)

    # Phytium Pi (requires --serial for start)
    $0 phytiumpi setup                              # Prepare environment only
    $0 phytiumpi setup --serial /dev/ttyUSB1        # Prepare with custom serial device
    $0 phytiumpi run --arceos                       # Launch ArceOS (serial must be set in config)
    $0 phytiumpi start --serial /dev/ttyUSB0 --arceos # One-step: prepare + launch
    $0 phytiumpi start --serial /dev/ttyUSB1 --linux # One-step with custom serial device

    # ROC-RK3568-PC (requires --serial for start)
    $0 roc-rk3568-pc setup                          # Prepare environment only
    $0 roc-rk3568-pc setup --serial /dev/ttyACM0    # Prepare with custom serial device
    $0 roc-rk3568-pc run --arceos                   # Launch ArceOS (serial must be set in config)
    $0 roc-rk3568-pc start --serial /dev/ttyUSB0 --arceos # One-step: prepare + launch
    $0 roc-rk3568-pc start --serial /dev/ttyACM0 --multi # One-step with custom serial device

    # RDK S100P (requires --serial for start)
    $0 rdk-s100 setup                               # Prepare environment only
    $0 rdk-s100 setup --serial /dev/ttyUSB0         # Prepare with custom serial device
    $0 rdk-s100 run --arceos                        # Launch ArceOS (serial must be set in config)
    $0 rdk-s100 start --serial /dev/ttyUSB0 --linux # One-step: prepare + launch Linux
    $0 rdk-s100 start --serial /dev/ttyUSB0 --multi # One-step with custom serial device
    $0 rdk-s100p start --serial /dev/ttyUSB0 --linux # Same as rdk-s100

    # Step-by-step execution
    $0 qemu-aarch64 setup                           # Prepare environment only
    $0 qemu-aarch64 run --arceos                    # Launch only

EOF
}

run_axvisor_qemu() {
    run_cmd cargo xtask qemu "$@"
}

run_axvisor_uboot() {
    run_cmd cargo xtask uboot "$@"
}

ensure_ostool() {
    info "Checking/installing ostool..."
    if ! command -v ostool &> /dev/null; then
        info "ostool not installed, installing..."
        cargo install ostool
    else
        info "ostool already installed"
    fi
}

# ============================================================================
# QEMU AArch64 Architecture Setup
# ============================================================================

setup_qemu_aarch64() {
    info "=== QEMU AArch64 Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    info "Downloading ArceOS image..."
    run_cmd cargo axvisor image pull qemu_aarch64_arceos --output-dir tmp/images

    info "Downloading Linux image..."
    run_cmd cargo axvisor image pull qemu_aarch64_linux --output-dir tmp/images

    info "Preparing board config file..."
    run_cmd cp configs/board/qemu-aarch64.toml tmp/configs/

    info "Preparing guest config files..."
    run_cmd cp configs/vms/arceos-aarch64-qemu-smp1.toml tmp/configs/
    run_cmd cp configs/vms/linux-aarch64-qemu-smp1.toml tmp/configs/

    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/qemu_aarch64_arceos/qemu-aarch64"|g' tmp/configs/arceos-aarch64-qemu-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/arceos-aarch64-qemu-smp1.toml
    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/qemu_aarch64_linux/qemu-aarch64"|g' tmp/configs/linux-aarch64-qemu-smp1.toml
    run_cmd sed -i 's/^id = 1$/id = 2/' tmp/configs/linux-aarch64-qemu-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/linux-aarch64-qemu-smp1.toml

    info "Preparing QEMU config file..."
    run_cmd cp .github/workflows/qemu-aarch64.toml tmp/configs/qemu-aarch64-runtime.toml

    ROOTFS_PATH="$(pwd)/tmp/images/qemu_aarch64_linux/rootfs.img"
    run_cmd sed -i 's|^  # "-drive",$|  "-drive",|g' tmp/configs/qemu-aarch64-runtime.toml
    run_cmd sed -i 's|^  # "id=disk0,if=none,format=raw,file=|  "id=disk0,if=none,format=raw,file=|g' tmp/configs/qemu-aarch64-runtime.toml
    run_cmd sed -i 's|file=${workspaceFolder}/tmp/rootfs.img|file='"$ROOTFS_PATH"'|g' tmp/configs/qemu-aarch64-runtime.toml
    run_cmd sed -i '/success_regex = \[/,/\]/c\success_regex = []' tmp/configs/qemu-aarch64-runtime.toml

    info "=== QEMU AArch64 Preparation Complete ==="
}

run_qemu_aarch64_arceos() {
    info "=== Launching QEMU AArch64 ArceOS Guest ==="
    run_axvisor_qemu \
        --config "$(pwd)/tmp/configs/qemu-aarch64.toml" \
        --qemu-config "$(pwd)/tmp/configs/qemu-aarch64-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-qemu-smp1.toml"
}

run_qemu_aarch64_linux() {
    info "=== Launching QEMU AArch64 Linux Guest ==="
    run_axvisor_qemu \
        --config "$(pwd)/tmp/configs/qemu-aarch64.toml" \
        --qemu-config "$(pwd)/tmp/configs/qemu-aarch64-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-qemu-smp1.toml"
}

run_qemu_aarch64_multi() {
    info "=== Launching QEMU AArch64 Multiple Guests (ArceOS + Linux) ==="
    run_axvisor_qemu \
        --config "$(pwd)/tmp/configs/qemu-aarch64.toml" \
        --qemu-config "$(pwd)/tmp/configs/qemu-aarch64-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-qemu-smp1.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-qemu-smp1.toml"
}

# ============================================================================
# QEMU RISC-V64 Architecture Setup
# ============================================================================

setup_qemu_riscv64() {
    info "=== QEMU RISC-V64 Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    info "Downloading ArceOS image..."
    run_cmd cargo axvisor image pull qemu_riscv64_arceos --output-dir tmp/images

    info "Preparing board config file..."
    run_cmd cp configs/board/qemu-riscv64.toml tmp/configs/

    info "Preparing guest config file..."
    run_cmd cp configs/vms/arceos-riscv64-qemu-smp1.toml tmp/configs/

    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/qemu_riscv64_arceos/qemu-riscv64"|g' tmp/configs/arceos-riscv64-qemu-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/arceos-riscv64-qemu-smp1.toml

    info "Preparing QEMU config file..."
    run_cmd cp .github/workflows/qemu-riscv64.toml tmp/configs/qemu-riscv64-runtime.toml
    run_cmd cp tmp/images/qemu_riscv64_arceos/rootfs.img tmp/rootfs.img

    info "=== QEMU RISC-V64 Preparation Complete ==="
}

run_qemu_riscv64_arceos() {
    info "=== Launching QEMU RISC-V64 ArceOS Guest ==="
    run_axvisor_qemu \
        --config "$(pwd)/tmp/configs/qemu-riscv64.toml" \
        --qemu-config "$(pwd)/tmp/configs/qemu-riscv64-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-riscv64-qemu-smp1.toml"
}

# ============================================================================
# QEMU x86_64 Architecture Setup
# ============================================================================

setup_qemu_x86_64() {
    info "=== QEMU x86_64 Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    info "Downloading NimbOS image..."
    run_cmd cargo axvisor image pull qemu_x86_64_nimbos --output-dir tmp/images

    info "Preparing board config file..."
    run_cmd cp configs/board/qemu-x86_64.toml tmp/configs/

    info "Preparing guest config file..."
    run_cmd cp configs/vms/nimbos-x86_64-qemu-smp1.toml tmp/configs/

    info "Preparing QEMU config file..."
    run_cmd cp .github/workflows/qemu-x86_64.toml tmp/configs/qemu-x86_64-runtime.toml

    ROOTFS_PATH="$(pwd)/tmp/images/qemu_x86_64_nimbos/rootfs.img"
    run_cmd sed -i 's|file=${workspaceFolder}/tmp/rootfs.img|file='"$ROOTFS_PATH"'|g' tmp/configs/qemu-x86_64-runtime.toml

    info "=== QEMU x86_64 Preparation Complete ==="
}

run_qemu_x86_64_nimbos() {
    info "=== Launching QEMU x86_64 NimbOS Guest ==="
    run_axvisor_qemu \
        --config "$(pwd)/tmp/configs/qemu-x86_64.toml" \
        --qemu-config "$(pwd)/tmp/configs/qemu-x86_64-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/nimbos-x86_64-qemu-smp1.toml"
}

# ============================================================================
# Phytium Pi Board Setup
# ============================================================================

setup_phytiumpi() {
    local serial_device=""
    local serial_specified=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_device="$2"
                serial_specified=true
                shift 2
                ;;
            *)
                error "Unknown option: $1"
                echo ""
                echo "Phytium Pi setup supports the following options:"
                echo "  --serial <device>   Specify serial device"
                exit 1
                ;;
        esac
    done

    info "=== Phytium Pi Board Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    ensure_ostool

    info "Downloading ArceOS image..."
    run_cmd cargo axvisor image pull phytiumpi_arceos --output-dir tmp/images

    info "Downloading Linux image (including device tree)..."
    run_cmd cargo axvisor image pull phytiumpi_linux --output-dir tmp/images

    info "Preparing board config file..."
    run_cmd cp configs/board/phytiumpi.toml tmp/configs/

    info "Preparing guest config files..."
    run_cmd cp configs/vms/arceos-aarch64-e2000-smp1.toml tmp/configs/
    run_cmd cp configs/vms/linux-aarch64-e2000-smp1.toml tmp/configs/

    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/phytiumpi_arceos/phytiumpi"|g' tmp/configs/arceos-aarch64-e2000-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/arceos-aarch64-e2000-smp1.toml
    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/phytiumpi_linux/phytiumpi"|g' tmp/configs/linux-aarch64-e2000-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/linux-aarch64-e2000-smp1.toml

    info "Preparing uboot config file..."
    run_cmd cp .github/workflows/uboot.toml tmp/configs/phytiumpi-runtime.toml
    run_cmd sed -i '/success_regex = \[/,/\]/c\success_regex = []' tmp/configs/phytiumpi-runtime.toml

    # Remove unnecessary commands
    run_cmd sed -i '/^board_power_off_cmd = "\${env:BOARD_POWER_OFF}"/d' tmp/configs/phytiumpi-runtime.toml
    run_cmd sed -i '/^board_reset_cmd = "\${env:BOARD_POWER_RESET}"/d' tmp/configs/phytiumpi-runtime.toml

    # Remove [net] config section
    run_cmd sed -i '/^\[net\]/d' tmp/configs/phytiumpi-runtime.toml
    run_cmd sed -i '/^interface = "\${env:BOARD_COMM_NET_IFACE}"/d' tmp/configs/phytiumpi-runtime.toml
    run_cmd sed -i '/^tftp_dir = "\${env:TFTP_DIR}"/d' tmp/configs/phytiumpi-runtime.toml

    # Set baud rate
    run_cmd sed -i 's|^baud_rate = "\${env:BOARD_COMM_UART_BAUD}"|baud_rate = "115200"|g' tmp/configs/phytiumpi-runtime.toml

    # Set serial device only if specified
    if [ "$serial_specified" = true ]; then
        info "Setting serial device to: $serial_device"
        run_cmd sed -i 's|^serial = "\${env:BOARD_COMM_UART_DEV}"|serial = "'"$serial_device"'"|g' tmp/configs/phytiumpi-runtime.toml
    else
        # Remove serial line to keep it as environment variable
        run_cmd sed -i '/^serial = "\${env:BOARD_COMM_UART_DEV}"/d' tmp/configs/phytiumpi-runtime.toml
    fi

    info "Adding device tree file path to uboot config..."
    DTB_PATH="$(pwd)/tmp/images/phytiumpi_linux/phytiumpi.dtb"
    run_cmd sed -i 's|^dtb_file = "\${env:BOARD_DTB}"|dtb_file = "'"$DTB_PATH"'"|g' tmp/configs/phytiumpi-runtime.toml

    info "=== Phytium Pi Board Preparation Complete ==="
    if [ "$serial_specified" = true ]; then
        info "Serial device set to: $serial_device"
    else
        warn "IMPORTANT: Please set the correct serial device in tmp/configs/phytiumpi-runtime.toml"
        warn "Example: serial = \"/dev/ttyUSB0\""
        warn "Then run: $0 phytiumpi run --arceos"
    fi
}

run_phytiumpi_arceos() {
    info "=== Launching Phytium Pi ArceOS Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/phytiumpi.toml" \
        --uboot-config "$(pwd)/tmp/configs/phytiumpi-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-e2000-smp1.toml"
}

run_phytiumpi_linux() {
    info "=== Launching Phytium Pi Linux Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/phytiumpi.toml" \
        --uboot-config "$(pwd)/tmp/configs/phytiumpi-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-e2000-smp1.toml"
}

run_phytiumpi_multi() {
    info "=== Launching Phytium Pi Multiple Guests (ArceOS + Linux) ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/phytiumpi.toml" \
        --uboot-config "$(pwd)/tmp/configs/phytiumpi-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-e2000-smp1.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-e2000-smp1.toml"
}

# ============================================================================
# ROC-RK3568-PC Board Setup
# ============================================================================

setup_roc_rk3568_pc() {
    local serial_device=""
    local serial_specified=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_device="$2"
                serial_specified=true
                shift 2
                ;;
            *)
                error "Unknown option: $1"
                echo ""
                echo "ROC-RK3568-PC setup supports the following options:"
                echo "  --serial <device>   Specify serial device"
                exit 1
                ;;
        esac
    done

    info "=== ROC-RK3568-PC Board Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    ensure_ostool

    info "Downloading ArceOS image..."
    run_cmd cargo axvisor image pull roc-rk3568-pc_arceos --output-dir tmp/images

    info "Downloading Linux image (including device tree)..."
    run_cmd cargo axvisor image pull roc-rk3568-pc_linux --output-dir tmp/images

    info "Preparing board config file..."
    run_cmd cp configs/board/roc-rk3568-pc.toml tmp/configs/

    info "Preparing guest config files..."
    run_cmd cp configs/vms/arceos-aarch64-rk3568-smp1.toml tmp/configs/
    run_cmd cp configs/vms/linux-aarch64-rk3568-smp1.toml tmp/configs/

    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/roc-rk3568-pc_arceos/roc-rk3568-pc"|g' tmp/configs/arceos-aarch64-rk3568-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/arceos-aarch64-rk3568-smp1.toml
    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/roc-rk3568-pc_linux/roc-rk3568-pc"|g' tmp/configs/linux-aarch64-rk3568-smp1.toml
    run_cmd sed -i 's|^image_location = "fs"|image_location = "memory"|g' tmp/configs/linux-aarch64-rk3568-smp1.toml

    info "Preparing uboot config file..."
    run_cmd cp .github/workflows/uboot.toml tmp/configs/roc-rk3568-pc-runtime.toml
    run_cmd sed -i '/success_regex = \[/,/\]/c\success_regex = []' tmp/configs/roc-rk3568-pc-runtime.toml

    # Remove unnecessary commands
    run_cmd sed -i '/^board_power_off_cmd = "\${env:BOARD_POWER_OFF}"/d' tmp/configs/roc-rk3568-pc-runtime.toml
    run_cmd sed -i '/^board_reset_cmd = "\${env:BOARD_POWER_RESET}"/d' tmp/configs/roc-rk3568-pc-runtime.toml

    # Remove [net] config section
    run_cmd sed -i '/^\[net\]/d' tmp/configs/roc-rk3568-pc-runtime.toml
    run_cmd sed -i '/^interface = "\${env:BOARD_COMM_NET_IFACE}"/d' tmp/configs/roc-rk3568-pc-runtime.toml
    run_cmd sed -i '/^tftp_dir = "\${env:TFTP_DIR}"/d' tmp/configs/roc-rk3568-pc-runtime.toml

    info "Setting baud rate to 1500000..."
    run_cmd sed -i 's|baud_rate\s*=.*|baud_rate = "1500000"|g' tmp/configs/roc-rk3568-pc-runtime.toml

    # Set serial device only if specified
    if [ "$serial_specified" = true ]; then
        info "Setting serial device to: $serial_device"
        run_cmd sed -i 's|^serial = "\${env:BOARD_COMM_UART_DEV}"|serial = "'"$serial_device"'"|g' tmp/configs/roc-rk3568-pc-runtime.toml
    else
        # Remove serial line to keep it as environment variable
        run_cmd sed -i '/^serial = "\${env:BOARD_COMM_UART_DEV}"/d' tmp/configs/roc-rk3568-pc-runtime.toml
    fi

    info "Adding device tree file path to uboot config..."
    DTB_PATH="$(pwd)/tmp/images/roc-rk3568-pc_linux/roc-rk3568-pc.dtb"
    run_cmd sed -i 's|^dtb_file = "\${env:BOARD_DTB}"|dtb_file = "'"$DTB_PATH"'"|g' tmp/configs/roc-rk3568-pc-runtime.toml

    info "=== ROC-RK3568-PC Board Preparation Complete ==="
    info "Baud rate has been set to 1500000"
    if [ "$serial_specified" = true ]; then
        info "Serial device set to: $serial_device"
    else
        warn "IMPORTANT: Please set the correct serial device in tmp/configs/roc-rk3568-pc-runtime.toml"
        warn "Example: serial = \"/dev/ttyUSB0\""
        warn "Then run: $0 roc-rk3568-pc run --arceos"
    fi
}

run_roc_rk3568_pc_arceos() {
    info "=== Launching ROC-RK3568-PC ArceOS Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/roc-rk3568-pc.toml" \
        --uboot-config "$(pwd)/tmp/configs/roc-rk3568-pc-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-rk3568-smp1.toml"
}

run_roc_rk3568_pc_linux() {
    info "=== Launching ROC-RK3568-PC Linux Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/roc-rk3568-pc.toml" \
        --uboot-config "$(pwd)/tmp/configs/roc-rk3568-pc-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-rk3568-smp1.toml"
}

run_roc_rk3568_pc_multi() {
    info "=== Launching ROC-RK3568-PC Multiple Guests (ArceOS + Linux) ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/roc-rk3568-pc.toml" \
        --uboot-config "$(pwd)/tmp/configs/roc-rk3568-pc-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-rk3568-smp1.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-rk3568-smp1.toml"
}

# ============================================================================
# RDK S100P Board Setup
# ============================================================================

setup_rdk_s100() {
    local serial_device=""
    local serial_specified=false
    local arceos_image="${AXVISOR_RDK_S100P_ARCEOS_IMAGE:-rdk-s100p_arceos}"
    local linux_image="${AXVISOR_RDK_S100P_LINUX_IMAGE:-rdk-s100p_linux}"
    local image_storage="${AXVISOR_IMAGE_LOCAL_STORAGE:-/tmp/.axvisor-images}"
    local image_pull_args=()

    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_device="$2"
                serial_specified=true
                shift 2
                ;;
            *)
                error "Unknown option: $1"
                echo ""
                echo "RDK S100P setup supports the following options:"
                echo "  --serial <device>   Specify serial device"
                exit 1
                ;;
        esac
    done

    info "=== RDK S100P Board Preparation ==="

    run_cmd mkdir -p tmp/{configs,images}

    if [ -n "${AXVISOR_IMAGE_LOCAL_STORAGE:-}" ]; then
        image_pull_args+=(-S "${AXVISOR_IMAGE_LOCAL_STORAGE}")
    fi
    if [ -n "${AXVISOR_IMAGE_REGISTRY:-}" ]; then
        image_pull_args+=(-R "${AXVISOR_IMAGE_REGISTRY}")
    fi
    if [ "${AXVISOR_IMAGE_FORCE_SYNC:-0}" = "1" ]; then
        run_cmd rm -f "${image_storage}/images.toml" "${image_storage}/.last_sync"
    fi

    ensure_ostool

    if [ -d "tmp/images/${arceos_image}" ]; then
        info "Using existing ArceOS image directory: tmp/images/${arceos_image}"
    else
        info "Downloading ArceOS image..."
        run_cmd cargo axvisor image "${image_pull_args[@]}" pull "${arceos_image}" --output-dir tmp/images
    fi

    if [ -d "tmp/images/${linux_image}" ]; then
        info "Using existing Linux image directory: tmp/images/${linux_image}"
    else
        info "Downloading Linux image (including device tree)..."
        run_cmd cargo axvisor image "${image_pull_args[@]}" pull "${linux_image}" --output-dir tmp/images
    fi

    info "Preparing board config file..."
    run_cmd cp configs/board/rdk-s100.toml tmp/configs/

    info "Preparing guest config files..."
    run_cmd cp configs/vms/arceos-aarch64-s100-smp1.toml tmp/configs/
    run_cmd cp configs/vms/linux-aarch64-s100-smp1.toml tmp/configs/

    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/'"${arceos_image}"'/rdk-s100p"|g' tmp/configs/arceos-aarch64-s100-smp1.toml
    run_cmd sed -i 's|^kernel_path = .*|kernel_path = "../images/'"${linux_image}"'/rdk-s100p"|g' tmp/configs/linux-aarch64-s100-smp1.toml

    info "Preparing uboot config file..."
    local workspace_root
    local uboot_template
    workspace_root="$(cd ../.. && pwd)"
    uboot_template="${workspace_root}/.github/workflows/uboot-rdk-s100.toml"
    if [ ! -f "$uboot_template" ]; then
        error "RDK S100P U-Boot config not found: $uboot_template"
        exit 1
    fi
    run_cmd cp "$uboot_template" tmp/configs/rdk-s100-runtime.toml
    run_cmd sed -i '/success_regex = \[/,/\]/c\success_regex = []' tmp/configs/rdk-s100-runtime.toml

    if [ "$serial_specified" = true ]; then
        info "Setting serial device to: $serial_device"
        run_cmd sed -i 's|^serial = "\${env:BOARD_COMM_UART_DEV}"|serial = "'"$serial_device"'"|g' tmp/configs/rdk-s100-runtime.toml
    else
        run_cmd sed -i '/^serial = "\${env:BOARD_COMM_UART_DEV}"/d' tmp/configs/rdk-s100-runtime.toml
    fi

    info "Adding device tree file path to uboot config..."
    DTB_PATH="$(pwd)/tmp/images/${linux_image}/rdk-s100p-v1p0.dtb"
    run_cmd sed -i 's|^dtb_file = "\${env:BOARD_DTB}"|dtb_file = "'"$DTB_PATH"'"|g' tmp/configs/rdk-s100-runtime.toml

    info "=== RDK S100P Board Preparation Complete ==="
    info "Baud rate has been set to 921600"
    if [ "$serial_specified" = true ]; then
        info "Serial device set to: $serial_device"
    else
        warn "IMPORTANT: tmp/configs/rdk-s100-runtime.toml is not runnable until serial is set"
        warn "Preferred: rerun setup with --serial, for example:"
        warn "  $0 rdk-s100 setup --serial /dev/ttyUSB0"
        warn "Or edit tmp/configs/rdk-s100-runtime.toml and add: serial = \"/dev/ttyUSB0\""
    fi
}

run_rdk_s100_arceos() {
    info "=== Launching RDK S100P ArceOS Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/rdk-s100.toml" \
        --uboot-config "$(pwd)/tmp/configs/rdk-s100-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-s100-smp1.toml"
}

run_rdk_s100_linux() {
    info "=== Launching RDK S100P Linux Guest ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/rdk-s100.toml" \
        --uboot-config "$(pwd)/tmp/configs/rdk-s100-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-s100-smp1.toml"
}

run_rdk_s100_multi() {
    info "=== Launching RDK S100P Multiple Guests (ArceOS + Linux) ==="
    run_axvisor_uboot \
        --config "$(pwd)/tmp/configs/rdk-s100.toml" \
        --uboot-config "$(pwd)/tmp/configs/rdk-s100-runtime.toml" \
        --vmconfigs "$(pwd)/tmp/configs/arceos-aarch64-s100-smp1.toml" \
        --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-s100-smp1.toml"
}

# ============================================================================
# QEMU AArch64 Command Handling
# ============================================================================

cmd_setup_qemu_aarch64() {
    setup_qemu_aarch64
}

cmd_run_qemu_aarch64() {
    local mode="$1"

    case "$mode" in
        -a|--arceos|"")
            run_qemu_aarch64_arceos
            ;;
        -l|--linux)
            run_qemu_aarch64_linux
            ;;
        -m|--multi)
            run_qemu_aarch64_multi
            ;;
        -n|--nimbos)
            error "Unsupported combination: QEMU AArch64 does not support NimbOS"
            echo ""
            echo "QEMU AArch64 platform supports the following guest systems:"
            echo "  - ArceOS (use --arceos)"
            echo "  - Linux  (use --linux)"
            echo "  - Multiple guests (use --multi)"
            echo ""
            echo "To run NimbOS, please use QEMU x86_64 platform:"
            echo "  $0 qemu-x86_64 start --nimbos"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "QEMU AArch64 platform supports the following options:"
            echo "  -a, --arceos    Launch ArceOS guest"
            echo "  -l, --linux     Launch Linux guest"
            echo "  -m, --multi     Launch multiple guests (ArceOS + Linux)"
            exit 1
            ;;
    esac
}

cmd_start_qemu_aarch64() {
    local mode="$1"
    # Validate parameters first
    case "$mode" in
        -a|--arceos|-l|--linux|-m|--multi|"")
            # Valid parameters, continue
            ;;
        *)
            # Invalid parameters, report error without executing setup
            cmd_run_qemu_aarch64 "$mode"
            return
            ;;
    esac
    setup_qemu_aarch64
    echo ""
    cmd_run_qemu_aarch64 "$mode"
}

# ============================================================================
# QEMU RISC-V64 Command Handling
# ============================================================================

cmd_setup_qemu_riscv64() {
    setup_qemu_riscv64
}

cmd_run_qemu_riscv64() {
    local mode="$1"

    case "$mode" in
        -a|--arceos|"")
            run_qemu_riscv64_arceos
            ;;
        -l|--linux)
            error "Unsupported combination: QEMU RISC-V64 quick start does not support Linux yet"
            echo ""
            echo "QEMU RISC-V64 platform currently supports the following guest system:"
            echo "  - ArceOS (use --arceos)"
            echo ""
            echo "Cross-ISA guest boot (for example: riscv64 AxVisor -> aarch64 guest) is not"
            echo "available in the current AxVisor hypervisor stack."
            exit 1
            ;;
        -m|--multi)
            error "Unsupported combination: QEMU RISC-V64 does not support multi-guest mode"
            echo ""
            echo "QEMU RISC-V64 platform currently supports the following guest system:"
            echo "  - ArceOS (use --arceos)"
            exit 1
            ;;
        -n|--nimbos)
            error "Unsupported combination: QEMU RISC-V64 does not support NimbOS"
            echo ""
            echo "QEMU RISC-V64 platform currently supports the following guest system:"
            echo "  - ArceOS (use --arceos)"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "QEMU RISC-V64 platform supports the following options:"
            echo "  -a, --arceos    Launch ArceOS guest"
            exit 1
            ;;
    esac
}

cmd_start_qemu_riscv64() {
    local mode="$1"
    case "$mode" in
        -a|--arceos|"")
            ;;
        *)
            cmd_run_qemu_riscv64 "$mode"
            return
            ;;
    esac
    setup_qemu_riscv64
    echo ""
    cmd_run_qemu_riscv64 "$mode"
}

# ============================================================================
# QEMU x86_64 Command Handling
# ============================================================================

cmd_setup_qemu_x86_64() {
    setup_qemu_x86_64
}

cmd_run_qemu_x86_64() {
    local mode="$1"

    case "$mode" in
        -n|--nimbos|"")
            run_qemu_x86_64_nimbos
            ;;
        -a|--arceos)
            error "Unsupported combination: QEMU x86_64 does not support ArceOS"
            echo ""
            echo "QEMU x86_64 platform only supports the following guest system:"
            echo "  - NimbOS (use --nimbos)"
            echo ""
            echo "To run ArceOS, please use one of the following platforms:"
            echo "  $0 qemu-aarch64 start --arceos"
            echo "  $0 phytiumpi setup && $0 phytiumpi run --arceos"
            echo "  $0 roc-rk3568-pc setup && $0 roc-rk3568-pc run --arceos"
            echo "  $0 rdk-s100 setup && $0 rdk-s100 run --arceos"
            exit 1
            ;;
        -l|--linux)
            error "Unsupported combination: QEMU x86_64 does not support Linux"
            echo ""
            echo "QEMU x86_64 platform only supports the following guest system:"
            echo "  - NimbOS (use --nimbos)"
            echo ""
            echo "To run Linux, please use one of the following platforms:"
            echo "  $0 qemu-aarch64 start --linux"
            echo "  $0 phytiumpi setup && $0 phytiumpi run --linux"
            echo "  $0 roc-rk3568-pc setup && $0 roc-rk3568-pc run --linux"
            echo "  $0 rdk-s100 setup && $0 rdk-s100 run --linux"
            exit 1
            ;;
        -m|--multi)
            error "Unsupported combination: QEMU x86_64 does not support multi-guest mode"
            echo ""
            echo "QEMU x86_64 platform only supports the following guest system:"
            echo "  - NimbOS (use --nimbos)"
            echo ""
            echo "To run multiple guests, please use one of the following platforms:"
            echo "  $0 qemu-aarch64 start --multi"
            echo "  $0 phytiumpi setup && $0 phytiumpi run --multi"
            echo "  $0 roc-rk3568-pc setup && $0 roc-rk3568-pc run --multi"
            echo "  $0 rdk-s100 setup && $0 rdk-s100 run --multi"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "QEMU x86_64 platform supports the following options:"
            echo "  -n, --nimbos    Launch NimbOS guest"
            exit 1
            ;;
    esac
}

cmd_start_qemu_x86_64() {
    local mode="$1"
    # Validate parameters first
    case "$mode" in
        -n|--nimbos|"")
            # Valid parameters, continue
            ;;
        *)
            # Invalid parameters, report error without executing setup
            cmd_run_qemu_x86_64 "$mode"
            return
            ;;
    esac
    setup_qemu_x86_64
    echo ""
    cmd_run_qemu_x86_64 "$mode"
}

# ============================================================================
# Phytium Pi Command Handling
# ============================================================================

cmd_setup_phytiumpi() {
    setup_phytiumpi "$@"
}

cmd_start_phytiumpi() {
    local serial_args=()
    local guest_mode=""
    local serial_specified=false

    # Parse arguments to separate --serial from guest mode
    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_args+=("$1" "$2")
                serial_specified=true
                shift 2
                ;;
            -a|--arceos|-l|--linux|-m|--multi|"")
                guest_mode="$1"
                shift
                ;;
            *)
                error "Unknown option: $1"
                exit 1
                ;;
        esac
    done

    # First run setup with serial args
    setup_phytiumpi "${serial_args[@]}"

    # If serial not specified, stop after setup
    if [ "$serial_specified" = false ]; then
        return
    fi

    echo ""
    # Then run with guest mode
    cmd_run_phytiumpi "$guest_mode"
}

cmd_run_phytiumpi() {
    local mode="$1"

    case "$mode" in
        -a|--arceos|"")
            run_phytiumpi_arceos
            ;;
        -l|--linux)
            run_phytiumpi_linux
            ;;
        -m|--multi)
            run_phytiumpi_multi
            ;;
        -n|--nimbos)
            error "Unsupported combination: Phytium Pi board does not support NimbOS"
            echo ""
            echo "Phytium Pi board supports the following guest systems:"
            echo "  - ArceOS (use --arceos)"
            echo "  - Linux  (use --linux)"
            echo "  - Multiple guests (use --multi)"
            echo ""
            echo "To run NimbOS, please use QEMU x86_64 platform:"
            echo "  $0 qemu-x86_64 start --nimbos"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "Phytium Pi board supports the following options:"
            echo "  -a, --arceos    Launch ArceOS guest"
            echo "  -l, --linux     Launch Linux guest"
            echo "  -m, --multi     Launch multiple guests (ArceOS + Linux)"
            exit 1
            ;;
    esac
}

# ============================================================================
# ROC-RK3568-PC Command Handling
# ============================================================================

cmd_setup_roc_rk3568_pc() {
    setup_roc_rk3568_pc "$@"
}

cmd_start_roc_rk3568_pc() {
    local serial_args=()
    local guest_mode=""
    local serial_specified=false

    # Parse arguments to separate --serial from guest mode
    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_args+=("$1" "$2")
                serial_specified=true
                shift 2
                ;;
            -a|--arceos|-l|--linux|-m|--multi|"")
                guest_mode="$1"
                shift
                ;;
            *)
                error "Unknown option: $1"
                exit 1
                ;;
        esac
    done

    # First run setup with serial args
    setup_roc_rk3568_pc "${serial_args[@]}"

    # If serial not specified, stop after setup
    if [ "$serial_specified" = false ]; then
        return
    fi

    echo ""
    # Then run with guest mode
    cmd_run_roc_rk3568_pc "$guest_mode"
}

cmd_run_roc_rk3568_pc() {
    local mode="$1"

    case "$mode" in
        -a|--arceos|"")
            run_roc_rk3568_pc_arceos
            ;;
        -l|--linux)
            run_roc_rk3568_pc_linux
            ;;
        -m|--multi)
            run_roc_rk3568_pc_multi
            ;;
        -n|--nimbos)
            error "Unsupported combination: ROC-RK3568-PC board does not support NimbOS"
            echo ""
            echo "ROC-RK3568-PC board supports the following guest systems:"
            echo "  - ArceOS (use --arceos)"
            echo "  - Linux  (use --linux)"
            echo "  - Multiple guests (use --multi)"
            echo ""
            echo "To run NimbOS, please use QEMU x86_64 platform:"
            echo "  $0 qemu-x86_64 start --nimbos"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "ROC-RK3568-PC board supports the following options:"
            echo "  -a, --arceos    Launch ArceOS guest"
            echo "  -l, --linux     Launch Linux guest"
            echo "  -m, --multi     Launch multiple guests (ArceOS + Linux)"
            exit 1
            ;;
    esac
}

# ============================================================================
# RDK S100P Command Handling
# ============================================================================

cmd_setup_rdk_s100() {
    setup_rdk_s100 "$@"
}

cmd_start_rdk_s100() {
    local serial_args=()
    local guest_mode=""
    local serial_specified=false

    while [[ $# -gt 0 ]]; do
        case $1 in
            --serial)
                serial_args+=("$1" "$2")
                serial_specified=true
                shift 2
                ;;
            -a|--arceos|-l|--linux|-m|--multi|"")
                guest_mode="$1"
                shift
                ;;
            *)
                error "Unknown option: $1"
                exit 1
                ;;
        esac
    done

    setup_rdk_s100 "${serial_args[@]}"

    if [ "$serial_specified" = false ]; then
        return
    fi

    echo ""
    cmd_run_rdk_s100 "$guest_mode"
}

cmd_run_rdk_s100() {
    local mode="$1"

    case "$mode" in
        -a|--arceos|"")
            run_rdk_s100_arceos
            ;;
        -l|--linux)
            run_rdk_s100_linux
            ;;
        -m|--multi)
            run_rdk_s100_multi
            ;;
        -n|--nimbos)
            error "Unsupported combination: RDK S100P board does not support NimbOS"
            echo ""
            echo "RDK S100P board supports the following guest systems:"
            echo "  - ArceOS (use --arceos)"
            echo "  - Linux  (use --linux)"
            echo "  - Multiple guests (use --multi)"
            echo ""
            echo "To run NimbOS, please use QEMU x86_64 platform:"
            echo "  $0 qemu-x86_64 start --nimbos"
            exit 1
            ;;
        *)
            error "Unknown option: $mode"
            echo ""
            echo "RDK S100P board supports the following options:"
            echo "  -a, --arceos    Launch ArceOS guest"
            echo "  -l, --linux     Launch Linux guest"
            echo "  -m, --multi     Launch multiple guests (ArceOS + Linux)"
            exit 1
            ;;
    esac
}

# ============================================================================
# Main Program
# ============================================================================

check_root_dir

if [ $# -eq 0 ]; then
    # Default to QEMU AArch64 ArceOS
    cmd_start_qemu_aarch64 ""
    exit 0
fi

PLATFORM="$1"
shift

case "$PLATFORM" in
    qemu-aarch64)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_qemu_aarch64
                ;;
            run)
                cmd_run_qemu_aarch64 "$@"
                ;;
            start)
                cmd_start_qemu_aarch64 "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                show_help
                exit 1
                ;;
        esac
        ;;
    qemu-riscv64)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_qemu_riscv64
                ;;
            run)
                cmd_run_qemu_riscv64 "$@"
                ;;
            start)
                cmd_start_qemu_riscv64 "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                show_help
                exit 1
                ;;
        esac
        ;;
    qemu-x86_64)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_qemu_x86_64
                ;;
            run)
                cmd_run_qemu_x86_64 "$@"
                ;;
            start)
                cmd_start_qemu_x86_64 "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                show_help
                exit 1
                ;;
        esac
        ;;
    phytiumpi)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_phytiumpi "$@"
                ;;
            run)
                cmd_run_phytiumpi "$@"
                ;;
            start)
                cmd_start_phytiumpi "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                echo ""
                echo "Phytium Pi board supports the following commands:"
                echo "  setup [--serial <device>]   Prepare board environment (download images, prepare config files)"
                echo "  run [--arceos|--linux|--multi]    Launch guest"
                echo "  start [--serial <device>] [--arceos|--linux|--multi]   One-step setup + launch"
                exit 1
                ;;
        esac
        ;;
    roc-rk3568-pc)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_roc_rk3568_pc "$@"
                ;;
            run)
                cmd_run_roc_rk3568_pc "$@"
                ;;
            start)
                cmd_start_roc_rk3568_pc "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                echo ""
                echo "ROC-RK3568-PC board supports the following commands:"
                echo "  setup [--serial <device>]   Prepare board environment (download images, prepare config files)"
                echo "  run [--arceos|--linux|--multi]    Launch guest"
                echo "  start [--serial <device>] [--arceos|--linux|--multi]   One-step setup + launch"
                exit 1
                ;;
        esac
        ;;
    rdk-s100|rdk-s100p)
        if [ $# -eq 0 ]; then
            show_help
            exit 0
        fi
        CMD="$1"
        shift
        case "$CMD" in
            setup)
                cmd_setup_rdk_s100 "$@"
                ;;
            run)
                cmd_run_rdk_s100 "$@"
                ;;
            start)
                cmd_start_rdk_s100 "$@"
                ;;
            *)
                error "Unknown command: $CMD"
                echo ""
                echo "RDK S100P board supports the following commands:"
                echo "  setup [--serial <device>]   Prepare board environment (serial is ignored for compatibility)"
                echo "  run [--arceos|--linux|--multi]    Launch guest"
                echo "  start [--serial <device>] [--arceos|--linux|--multi]   One-step setup + launch"
                exit 1
                ;;
        esac
        ;;
    -h|--help)
        show_help
        ;;
    *)
        error "Unknown platform: $PLATFORM"
        show_help
        exit 1
        ;;
esac
