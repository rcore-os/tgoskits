# StarryOS jcode Example

这个目录提供在 StarryOS x86_64 QEMU 中运行 jcode（AI coding agent）的手动示例。它不是 `test-suit/starryos` 用例，也不会进入 CI；下载 jcode、glibc-to-musl 补丁、rootfs 注入都只会在用户显式执行本目录脚本时发生。

## 什么是 jcode

jcode 是一个 Rust 编写的 AI coding agent harness，采用 TUI client + background server 双进程架构，通过 Unix domain socket 通信。项目地址：<https://github.com/1jehuang/jcode>

## Host 依赖

脚本需要以下 host 工具（Debian/Ubuntu）：

```bash
apt-get install -y patchelf binutils curl qemu-user-static
```

## 准备 jcode Rootfs

先在 host 侧准备本地 rootfs。脚本会自动：

1. 从 GitHub releases 下载 jcode linux-x86_64 二进制
2. 下载 Alpine minirootfs 作为 staging 环境
3. 使用 `patchelf` 将 glibc-linked 的 jcode 转换为 musl 兼容
4. 编译 glibc stub 共享库（提供 `mallopt`、`__res_init` 等 glibc-only 符号）
5. 注入 jcode 二进制、SSL 库、Kerberos 依赖到 rootfs

```bash
apps/starry/jcode/prepare_jcode_rootfs.sh
```

产物位于 `tmp/axbuild/rootfs/rootfs-x86_64-jcode.img`，是本地资产，不提交到仓库。

## 离线 Smoke 测试

验证 jcode 能在 StarryOS guest 中正常运行：

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/jcode/qemu-x86_64.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-jcode.img
```

期望输出包含：

```text
STARRY_JCODE_SMOKE_PASSED
```

## 手动交互

准备 rootfs 后，可以直接启动 QEMU 进入 StarryOS shell：

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-jcode.img
```

进入 `root@starry` 后：

```sh
export HOME=/root
export USER=root
export SHELL=/bin/sh
export TERM=xterm-256color
export PATH=/usr/local/bin:/usr/bin:/bin:/sbin
export JCODE_NO_AUTO_UPDATE=1

# 配置 provider（需要网络和 API key）
jcode login --provider openai-compatible

# 运行 jcode TUI
jcode
```

> **注意**：`JCODE_NO_AUTO_UPDATE=1` 必须设置。jcode 的自动更新会下载 glibc 版本的二进制，覆盖已打补丁的 musl 版本，导致 jcode 无法运行。如果误触发了自动更新，需要重新运行 `prepare_jcode_rootfs.sh`。


## 内核修复

jcode 在 StarryOS 上运行需要以下内核修复（已合入 dev 分支）：

- ext4 文件系统块分配和 JBD2 superblock 修复
- EPOLLET edge-triggered epoll 竞态修复
- 非阻塞 socket waker 注册
- ARP 队列扩容（32→256，TTL 60s→300s）
- TTY 终端 ANSI CPR 和 SIGWINCH 支持

## 边界

这个 example 只声明 x86_64 QEMU 下的手动演示流程。它不覆盖 jcode TUI 在线模式、CI 测试、或其他架构上的 jcode。
