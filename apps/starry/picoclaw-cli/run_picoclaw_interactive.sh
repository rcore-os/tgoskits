#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

image="${STARRYOS_DOCKER_IMAGE:-starryos-dev:ubuntu-qemu10.2.1}"
rootfs="${PICOCLAW_USER_ROOTFS:-tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-user.img}"
qemu_config="${PICOCLAW_USER_QEMU_CONFIG:-apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-interactive.toml}"
provider="${PICOCLAW_PROVIDER:-openai}"
model_name="${PICOCLAW_MODEL_NAME:-mimo-v25}"
model="${PICOCLAW_MODEL:-mimo-v2.5}"
api_base="${PICOCLAW_API_BASE:-https://token-plan-cn.xiaomimimo.com/v1}"
prepare_mode="auto"

usage() {
    cat <<EOF
Usage: $0 [--rebuild-rootfs] [--no-prepare] [--prepare-only]

Start an interactive StarryOS x86_64 QEMU shell with PicoClaw ready to use.

Default behavior:
  - reuse ${rootfs} when it already exists;
  - create it when it is missing;
  - keep QEMU running so you can type PicoClaw commands yourself.

Options:
  --rebuild-rootfs  Recreate the user rootfs even if it already exists.
  --no-prepare      Do not create or update the rootfs before booting.
  --prepare-only    Prepare the user rootfs and exit without booting QEMU.
  -h, --help        Show this help.

Required secret when creating/rebuilding the rootfs:
  PICOCLAW_API_KEY  API key for the configured endpoint. If it is unset,
                    this script asks for it without echoing.

Optional environment:
  STARRYOS_DOCKER_IMAGE default: ${image}
  PICOCLAW_PROVIDER     default: ${provider}
  PICOCLAW_MODEL_NAME   default: ${model_name}
  PICOCLAW_MODEL        default: ${model}
  PICOCLAW_API_BASE     default: ${api_base}
  PICOCLAW_USER_ROOTFS  default: ${rootfs}

Inside StarryOS, try:
  picoclaw status
  picoclaw agent

Exit QEMU with Ctrl-a x.

The generated rootfs contains online config and secret material. Keep it local.
EOF
}

while (($#)); do
    case "$1" in
        --rebuild-rootfs)
            prepare_mode="rebuild"
            shift
            ;;
        --no-prepare)
            prepare_mode="never"
            shift
            ;;
        --prepare-only)
            prepare_mode="only"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

say() {
    printf '\n\033[1;36m==> %s\033[0m\n' "$1"
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

image_exists() {
    docker image inspect "$image" >/dev/null 2>&1 && return 0
    docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -Fx "$image" >/dev/null
}

run_docker() {
    docker run --rm \
        -v "$workspace:/mnt" \
        -w /mnt \
        -e PICOCLAW_API_KEY \
        -e PICOCLAW_PROVIDER="$provider" \
        -e PICOCLAW_MODEL_NAME="$model_name" \
        -e PICOCLAW_MODEL="$model" \
        -e PICOCLAW_API_BASE="$api_base" \
        "$image" \
        bash -lc "$1"
}

run_docker_interactive() {
    local tty_args=()
    if [[ -t 0 && -t 1 ]]; then
        tty_args=(-it)
    fi

    docker run --rm "${tty_args[@]}" \
        -v "$workspace:/mnt" \
        -w /mnt \
        "$image" \
        bash -lc "$1"
}

need_cmd docker

say "检查 Docker 和 StarryOS 开发镜像"
docker info >/dev/null
if ! image_exists; then
    echo "Docker image not found: $image" >&2
    echo "Available StarryOS-like images:" >&2
    docker image ls --format '  {{.Repository}}:{{.Tag}}  {{.ID}}' | grep -E 'starry|rcore|qemu' >&2 || true
    echo "Set STARRYOS_DOCKER_IMAGE to an available image name if needed." >&2
    exit 1
fi
echo "Using image: $image"

rootfs_abs="${workspace}/${rootfs}"
should_prepare=0
case "$prepare_mode" in
    rebuild|only)
        should_prepare=1
        ;;
    auto)
        if [[ ! -f "$rootfs_abs" ]]; then
            should_prepare=1
        fi
        ;;
    never)
        if [[ ! -f "$rootfs_abs" ]]; then
            echo "rootfs does not exist and --no-prepare was selected: $rootfs" >&2
            exit 1
        fi
        ;;
esac

if [[ "$should_prepare" -eq 1 ]]; then
    if [[ -z "${PICOCLAW_API_KEY:-}" ]]; then
        printf '请输入 PicoClaw/API endpoint key，输入时不会回显: '
        read -r -s PICOCLAW_API_KEY
        printf '\n'
        export PICOCLAW_API_KEY
    fi

    if [[ -z "$PICOCLAW_API_KEY" ]]; then
        echo "PICOCLAW_API_KEY is empty" >&2
        exit 1
    fi

    say "准备可长期使用的 PicoClaw rootfs"
    echo "provider:  $provider"
    echo "model:     $model"
    echo "api_base:  $api_base"
    echo "rootfs:    $rootfs"
    run_docker "export PATH=/opt/qemu-10.2.1/bin:\$PATH; apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh --rootfs '$rootfs'"
else
    say "复用已有 PicoClaw rootfs"
    echo "rootfs: $rootfs"
fi

if [[ "$prepare_mode" == "only" ]]; then
    say "rootfs 已准备完成"
    exit 0
fi

say "启动交互式 StarryOS PicoClaw 环境"
echo "进入 StarryOS 后可以直接输入："
echo "  picoclaw status"
echo "  picoclaw agent"
echo
echo "退出 QEMU：Ctrl-a x"

run_docker_interactive "export PATH=/opt/qemu-10.2.1/bin:/opt/x86_64-linux-musl-cross/bin:\$PATH; cargo xtask starry qemu --arch x86_64 --qemu-config '$qemu_config' --rootfs '$rootfs'"
