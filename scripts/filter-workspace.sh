#!/usr/bin/env bash
# Filter workspace Cargo.toml to exclude crates incompatible with the target arch.
# Only removes workspace MEMBERS (lines starting with whitespace + "components/).
# Keeps ALL [workspace.dependencies] entries untouched.

set -euo pipefail
ARCH="${1:-riscv64}"
CARGO="${2:-Cargo.toml}"

[ -f "$CARGO" ] || { echo "Not found: $CARGO"; exit 1; }

# Recover from previous crash
[ -f "$CARGO.bak" ] && mv "$CARGO.bak" "$CARGO"

cp "$CARGO" "$CARGO.bak"

# Member lines look like:    "components/<name>",
# Dep lines look like:       crate_name = { ..., path = "components/<name>" }
# We use ^[[:space:]]*"components/<name>" to match only member lines.
filter_member() { local name="$1"
    sed -i "/^[[:space:]]*\"components\/$name\",\?$/d" "$CARGO"
}

for name in \
    arm_vcpu arm_vgic aarch64_sysreg kasm-aarch64 \
    riscv-h riscv_vcpu riscv_vplic loongarch_vcpu \
    axdevice axvm someboot \
    x86_vcpu x86_vlapic \
    ; do
    case "$ARCH" in
        x86_64)
            case "$name" in x86_vcpu|x86_vlapic) continue ;; esac
            filter_member "$name"
            ;;
        riscv64)
            case "$name" in riscv*) continue ;; esac
            # riscv64 also removes x86 and arm members
            filter_member "$name"
            ;;
        aarch64)
            case "$name" in arm_*|aarch64*|kasm*) continue ;; esac
            filter_member "$name"
            ;;
    esac
done

# Also remove arch-specific apps (orangepi is aarch64-only, etc.)
case "$ARCH" in
    x86_64|riscv64)
        sed -i '/^[[:space:]]*"apps\/starry\/orangepi/d' "$CARGO"
        sed -i '/^[[:space:]]*"apps\/starry\/maix/d' "$CARGO"
        sed -i '/^[[:space:]]*"drivers\/usb\/usb-device\/uvc"/d' "$CARGO"
        ;;
esac

echo "filter-workspace: removed arch-incompatible members for $ARCH"
