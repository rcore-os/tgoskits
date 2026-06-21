# StarryOS macOS AArch64 自举编译

这个 app 用来在 Apple Silicon macOS 上复现 StarryOS 自举编译，最终验证环境是
Apple M3。host 先构建一个 AArch64 StarryOS 种子内核，用 QEMU HVF 启动它，
然后在 StarryOS guest 里面直接运行 Cargo，再把 guest 编译出的内核从工作
rootfs 中取回，并再次用 QEMU 启动验证。


## 具体流程

`full_self_build.sh` 是完整默认入口，会执行：

1. 使用 `cargo xtask starry build` 构建 StarryOS AArch64 种子内核；
2. 使用 `cargo xtask image pull` 拉取托管的 AArch64 Alpine rootfs；
3. 使用 `cargo xtask image resize` 扩容这个托管 rootfs；
4. 准备 app-local 的 guest 工具链 overlay；
5. 把托管 rootfs 复制成一次运行专用的工作镜像；
6. 使用 `cargo xtask image inject` 把 app overlay 注入到这个工作镜像；
7. 不带 `-snapshot` 启动 QEMU/HVF；
8. 在 StarryOS guest 内直接运行 Cargo；
9. 刷新 kallsyms，并把 guest 编译出的内核写入工作 rootfs；
10. host 侧用 `debugfs` 从工作 rootfs 提取 ELF 和 `.bin`。

## 脚本职责

| 脚本 | 角色 | 做什么 |
| --- | --- | --- |
| `full_self_build.sh` | 完整入口 | 串起 seed kernel、rootfs 输入准备、QEMU guest 自举编译和产物提取。 |
| `build_kernel.sh` | 阶段 1 | 在 host 上调用 `cargo xtask starry build` 构建用于首次启动 guest 的 AArch64 StarryOS 种子内核。它不准备 rootfs，也不启动 QEMU。 |
| `build_rootfs.sh` | 阶段 2 | 通过 `cargo xtask image pull/resize` 准备托管 AArch64 Alpine rootfs，并刷新 guest 工具链 overlay cache。它不 patch 托管镜像，也不启动 QEMU。 |
| `run_selfbuild.sh` | 阶段 3 | 复制托管 rootfs 为一次性工作镜像，调用 `prebuild.sh` 生成 overlay，再通过 `cargo xtask image inject` 注入 overlay，启动 QEMU/HVF，触发 guest 内 Cargo 构建，并从工作镜像提取产物。 |
| `prebuild.sh` | 内部脚本 | 为单次运行组装待注入 overlay：复制工具链 overlay、打包当前源码、复制离线 Cargo registry cache、写入 guest runner 和源码 metadata。 |
| `prepare_toolchain_overlay.sh` | 内部/调试脚本 | 下载并准备 guest 里的 Rust/Cargo、Rust 源码、LLVM/libclang、musl C 工具链和 Cargo cache。输出是目录树，不是 rootfs 镜像。 |
| `prepare_host_tools.sh` | 内部/调试脚本 | 准备 macOS host 上构建种子内核所需的 AArch64 musl 编译器 wrapper、`rust-nm`、`rust-objdump` 等工具。 |
| `guest-selfbuild.sh` | guest 内脚本 | 在 StarryOS guest 中解包源码、写 Cargo 配置、执行 Cargo 构建、刷新 kallsyms，并复制 guest-built 内核产物。 |

托管 rootfs 的默认位置是：

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

它不在 `tmp/axbuild/rootfs` 下。托管镜像保持干净，只有
`target/starry-macos-selfbuild/rootfs/` 下的一次性工作副本会被修改。

## 前置依赖

在 Apple Silicon macOS 上安装 host 工具：

```bash
brew install qemu e2fsprogs zig llvm
```

## 完整复现

### 1.开始自举编译：

在仓库根目录执行
```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

自举成功时会看到：

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

### 2.使用 qemu 启动自举编译的产物

```bash
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53\
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

## 验证与耗时

本次验证为本地删除 target, tmp 目录后从零开始自举编译

最终验证运行的 host 环境是：

```text
CPU: Apple M3
内存: 16 GiB
系统: macOS 15.6, Darwin 24.6.0
QEMU: qemu-system-aarch64 with HVF
```

第一步：开始自举编译

命令：

```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

这次运行的分阶段耗时如下：

| 阶段 | 耗时 |
| --- | --- |
| StarryOS AArch64 种子内核 Cargo 构建 | 增量缓存命中：Cargo 输出 `0.62s`，axbuild 阶段 `1.09s` |
| guest Cargo 构建计时 | Cargo 输出 `21m 47s`；PASS marker 的 `elapsed=1308`，即 `21m 48s` |
| guest-built kernel 的直接 QEMU 启动验证 | 约 `1s` |

PASS marker 的 `elapsed` 从 guest 即将执行 `cargo build` 前开始，到 Cargo 命令返回后结束；它包括 guest 内 Cargo 构建、build script、build-std 和链接时间，不包括 host 侧 rootfs 准备、QEMU 启动、kallsyms 刷新后的产物复制和 host 侧提取。

第二步：使用 qemu 启动自举编译产物

```bash
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53\
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

自举产物的启动验证到达：

```text
root@starry:/root #
```