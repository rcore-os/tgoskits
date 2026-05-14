# StarryOS Codex CLI Example

这个目录提供 StarryOS x86_64 QEMU 中运行 Codex CLI 的手动示例。它不是 `test-suit/starryos` 用例，也不会进入 CI；下载 Codex CLI、注入 rootfs、在线请求模型都只会在用户显式执行本目录脚本或 QEMU 配置时发生。

## 准备 Codex Rootfs

先在 host 侧准备本地 rootfs。脚本会按固定版本下载 `@openai/codex@0.115.0-linux-x64`，提取 `codex` 和 `rg` 到 `target/codex/assets/`，然后把它们注入到本地 rootfs 镜像中。

离线 help 示例不需要认证：

```bash
examples/starry/codex-cli/prepare_codex_rootfs.sh
```

在线示例需要把 host 侧的 Codex 登录文件注入 guest。常见准备方式如下，代理地址请替换成当前 host 可被 QEMU guest 访问的地址；QEMU user network 下通常可以用 `10.0.2.2` 指向 host。

```bash
examples/starry/codex-cli/prepare_codex_rootfs.sh \
  --output-rootfs tmp/axbuild/rootfs/rootfs-x86_64-codex-online.img \
  --auth-json target/auth.json \
  --proxy http://10.0.2.2:7890
```

`target/auth.json`、`target/codex/assets/`、`tmp/axbuild/rootfs/rootfs-x86_64-codex*.img` 都是本地资产，不提交到仓库。

## 离线启动检查

离线配置只验证 Codex CLI、`rg`、`git` 和本地 workspace 命令能在 StarryOS guest 中运行，不会请求模型。

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config examples/starry/codex-cli/qemu-x86_64-codex-help.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-codex.img
```

期望输出包含：

```text
STARRY_CODEX_STAGE_G_CODEX_HELP_PASSED
```

## 在线仓库 Demo

在线配置会在 StarryOS guest 中直接 clone TGOSKits 仓库，然后让 Codex CLI 在真实仓库里寻找一个小的 syscall/内核语义差异，并尽量给出最小修复或验证计划。这个流程会访问 GitHub 和 OpenAI 服务，耗时和结果都依赖本地网络、认证、代理和模型状态。

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config examples/starry/codex-cli/qemu-x86_64-codex-syscall-hunt.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-codex-online.img
```

QEMU 内部会执行的核心动作包括：

```sh
git clone --depth 1 --branch dev https://github.com/rcore-os/tgoskits.git repo
cd repo
codex exec \
  --dangerously-bypass-approvals-and-sandbox \
  --color never \
  -C /tmp/tgoskits-syscall-hunt/repo \
  --output-last-message /tmp/codex-tgoskits-syscall-hunt.txt \
  "$(cat /tmp/tgoskits-syscall-hunt-prompt.txt)"
```

期望输出包含：

```text
STARRY_TGOSKITS_SYSCALL_HUNT_DONE
STARRY_TGOSKITS_SYSCALL_HUNT_PASSED
```

## 手动交互

如果希望自己逐步输入命令，可以只用准备好的 online rootfs 启动 QEMU：

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-codex-online.img
```

进入 `root@starry` 后：

```sh
export HOME=/root
export USER=root
export SHELL=/bin/sh
export TERM=xterm-256color
export PATH=/usr/local/bin:/usr/bin:/bin:/sbin
export CODEX_HOME=/root/.codex
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
[ -f "$CODEX_HOME/starry-online-env" ] && . "$CODEX_HOME/starry-online-env"

mkdir -p /tmp/tgoskits-demo
cd /tmp/tgoskits-demo
git clone --depth 1 --branch dev https://github.com/rcore-os/tgoskits.git repo
cd repo
codex exec \
  --dangerously-bypass-approvals-and-sandbox \
  -C /tmp/tgoskits-demo/repo \
  'Please read this repository and briefly describe its StarryOS syscall test structure. Answer in Simplified Chinese.'
```

## 边界

这个 example 只声明 x86_64 QEMU 下的手动演示流程。它不覆盖 Codex TUI、默认 Linux sandbox、CI 在线请求模型，或其他架构上的 Codex CLI。
