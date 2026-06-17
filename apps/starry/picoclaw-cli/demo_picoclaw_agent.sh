#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

image="${STARRYOS_DOCKER_IMAGE:-starryos-dev:ubuntu-qemu10.2.1}"
rootfs="${PICOCLAW_DEMO_ROOTFS:-tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img}"
qemu_config="${PICOCLAW_DEMO_QEMU_CONFIG:-apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml}"
provider="${PICOCLAW_PROVIDER:-openai}"
model_name="${PICOCLAW_MODEL_NAME:-mimo-v25}"
model="${PICOCLAW_MODEL:-mimo-v2.5}"
api_base="${PICOCLAW_API_BASE:-https://token-plan-cn.xiaomimimo.com/v1}"
pause_enabled=1

usage() {
    cat <<EOF
Usage: $0 [--no-pause]

Run a teacher-facing Phase 2 demo:
  1. check Docker and the StarryOS development image;
  2. prepare a PicoClaw online rootfs inside Docker;
  3. boot StarryOS x86_64 QEMU inside Docker;
  4. run one verifiable PicoClaw request and several short chats.

Required secret:
  PICOCLAW_API_KEY      API key for the configured endpoint. If it is unset,
                        this script asks for it without echoing.

Optional environment:
  STARRYOS_DOCKER_IMAGE default: ${image}
  PICOCLAW_PROVIDER     default: ${provider}
  PICOCLAW_MODEL_NAME   default: ${model_name}
  PICOCLAW_MODEL        default: ${model}
  PICOCLAW_API_BASE     default: ${api_base}
  PICOCLAW_DEMO_ROOTFS  default: ${rootfs}

The API key is not written to tracked files. The generated rootfs contains the
key and must stay local.
EOF
}

while (($#)); do
    case "$1" in
        --no-pause)
            pause_enabled=0
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

pause() {
    if [[ "$pause_enabled" -eq 1 ]]; then
        printf '\n按 Enter 继续...'
        read -r _
    fi
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

need_cmd docker

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

say "Step 1: 检查 Docker 可用性"
docker info >/dev/null
if ! image_exists; then
    echo "Docker image not found: $image" >&2
    echo "Available StarryOS-like images:" >&2
    docker image ls --format '  {{.Repository}}:{{.Tag}}  {{.ID}}' | grep -E 'starry|rcore|qemu' >&2 || true
    echo "Set STARRYOS_DOCKER_IMAGE to an available image name if needed." >&2
    exit 1
fi
echo "Docker OK"
echo "Using image: $image"
pause

say "Step 2: 展示本次在线对话配置"
echo "provider:  $provider"
echo "model:     $model"
echo "api_base:  $api_base"
echo "rootfs:    $rootfs"
echo "qemu conf: $qemu_config"
echo "API key:   <hidden>"
pause

say "Step 3: 在 Docker 中确认 StarryOS/PicoClaw 所需工具"
run_docker 'command -v debugfs; command -v qemu-system-x86_64; command -v cargo; rustup toolchain list | sed -n "1,3p"'
pause

say "Step 4: 生成带 PicoClaw 和在线配置的 StarryOS rootfs"
run_docker "export PATH=/opt/qemu-10.2.1/bin:\$PATH; apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh --rootfs '$rootfs'"
pause

say "Step 5: 启动 StarryOS x86_64 QEMU，并让 PicoClaw 完成多次真实对话"
echo "成功时请向老师指出这两行："
echo "  STARRY_PICOCLAW_AGENT_OK"
echo "  STARRY_PICOCLAW_AGENT_PASSED"
echo "中间还会出现多段 PicoClaw chat，展示它在 StarryOS guest 里连续闲聊。"
pause
run_docker "export PATH=/opt/qemu-10.2.1/bin:/opt/x86_64-linux-musl-cross/bin:\$PATH; cargo xtask starry qemu --arch x86_64 --qemu-config '$qemu_config' --rootfs '$rootfs'"

say "演示完成"
echo "StarryOS 已在 QEMU 中运行 PicoClaw，并完成多次真实 API 对话。"
echo "注意：$rootfs 是本地生成物，里面包含在线配置和 secret，不要提交。"
