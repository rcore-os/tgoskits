# StarryOS macOS AArch64 自举编译

这个 app 用来在 Apple Silicon macOS 上复现 StarryOS 自举编译。host 先构建
一个 AArch64 StarryOS 种子内核，用 QEMU HVF 启动它，然后在 StarryOS guest
里面直接运行 Cargo，再把 guest 编译出的内核从工作 rootfs 中取回，并再次用
QEMU 启动验证。

macOS/HVF 相关的特殊流程保留在 `apps/starry/macos-selfbuild` 内。apps 外只
暴露通用 AArch64 bootarg 语义：

```text
someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off
```

不传这些参数时，AArch64 仍走默认 EL1 CNTP/physical timer 和正常 GICv3
distributor 初始化。

## 流程做了什么

`reproduce.sh` 会执行完整默认流程：

1. 使用 `cargo xtask starry build` 构建 StarryOS AArch64 种子内核；
2. 使用 `cargo xtask image pull` 拉取托管的 AArch64 Alpine rootfs；
3. 使用 `cargo xtask image resize` 扩容这个托管 rootfs；
4. 准备 app-local 的 guest 工具链 overlay；
5. 把托管 rootfs 复制成一次运行专用的工作镜像；
6. 把 app overlay 注入到这个工作镜像；
7. 不带 `-snapshot` 启动 QEMU/HVF；
8. 在 StarryOS guest 内直接运行 Cargo；
9. 刷新 kallsyms，并把 guest 编译出的内核写入工作 rootfs；
10. host 侧用 `debugfs` 从工作 rootfs 提取 ELF 和 `.bin`。

托管 rootfs 的默认位置是：

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

它不在 `tmp/axbuild/rootfs` 下。托管镜像保持干净，只有
`target/starry-macos-selfbuild/rootfs/` 下的一次性工作副本会被 patch。

## 前置依赖

在 Apple Silicon macOS 上安装 host 工具：

```bash
brew install qemu e2fsprogs zig llvm
```

第一次执行还需要网络，用来拉取托管 rootfs、Alpine APK、Rust dist 组件，以及
`Cargo.lock` 需要的 Cargo registry archive。工具链 overlay 准备完成后，
guest 里的 Cargo 构建会离线运行。

## 完整复现

在仓库根目录执行：

```bash
apps/starry/macos-selfbuild/reproduce.sh
```

自举成功时会看到：

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

提取出来的产物是：

```text
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin
```

## 启动 guest 编译出的内核

完整自举成功后执行：

```bash
BOOT_ONLY=1 \
PREPARE_OVERLAY=0 REQUIRE_FRESH_ROOTFS=0 \
KERNEL=target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
ROOTFS=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
SMP=4 JOBS=4 MEM=8192M QEMU_NET=0 QEMU_TIMEOUT_SEC=300 \
CASE_NAME=selfbuilt-boot-verify \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

boot-only 验证成功时会进入 StarryOS shell，并写出：

```text
root@starry:
===HOST-QEMU-STOP reason=boot-only-shell ... rc=0===
```

## 复用已经准备好的输入

复用当前 rootfs 和工具链 overlay，只重新跑 QEMU：

```bash
ROOTFS_MODE=skip apps/starry/macos-selfbuild/reproduce.sh
```

只准备或刷新 rootfs 输入：

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

强制重新构建 guest 工具链 overlay：

```bash
apps/starry/macos-selfbuild/build_rootfs.sh --force-toolchain
```

## 工具链 Overlay

overlay 是一个文件系统树，不是 rootfs 镜像：

```text
target/starry-macos-selfbuild/rootfs-build/toolchain-overlay
```

它由 Alpine AArch64 APK 和官方 Rust dist 组件准备而来，包含 guest 里的
Rust/Cargo 工具、Rust 源码、LLVM/libclang、musl C 工具链，以及离线 Cargo
registry cache。app 会在 QEMU 启动前把这个目录树注入到复制出来的工作 rootfs。

## Guest Cargo 构建

guest 里执行的是直接构建 StarryOS 的 Cargo 命令：

```text
cargo build -p starryos \
  --target apps/starry/macos-selfbuild/target-aarch64-unknown-none-softfloat-pie.json \
  -Z json-target-spec -Z host-config -Z target-applies-to-host \
  --bin starryos \
  -Z build-std=core,alloc \
  -Z build-std-features=compiler-builtins-mem \
  --features plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,smp \
  --release
```

流程不会传 `--no-default-features`，也不会限制 Cargo crate 数量。2026-06-21
验证过的一次无预生成 binding 自举构建了 `420/420` 个 Cargo 单元。当前
StarryOS 依赖图保留 `membarrier`、kallsyms 和 `kprobe`。

## Bindgen 和 libclang

这个 app 不注入预生成的 Rust binding 文件。guest 内的 build script 会正常运行，
包括 `ax-posix-api` 和 `lwprintf-rs` 这类 crate 的原始 `bindgen`。

之前的 `libclang.so ... Dynamic loading not supported` 不是缺少 libclang 包，
而是 host build-script 产物被构建成了 static musl binary。guest wrapper 会在
Cargo home 写入：

```toml
[host]
rustflags = ["-C", "target-feature=-crt-static"]
```

随后 Cargo 使用 `-Z host-config -Z target-applies-to-host` 调用。这样 host
build script 可以动态加载 libclang，而 StarryOS target 仍然是自定义 AArch64 PIE
target。

## 重要参数

| 变量 | 默认值 | 含义 |
| --- | --- | --- |
| `ROOTFS_MODE` | `build-rootfs` | 设置为 `skip` 时复用已经准备好的 rootfs 输入。 |
| `ROOTFS_SIZE_MIB` | `16384` | `cargo xtask image resize` 后的托管 rootfs 大小。 |
| `TGOS_IMAGE_LOCAL_STORAGE` | `target/starry-macos-selfbuild/tgos-images` | xtask image storage 根目录。 |
| `SMP` | `4` | QEMU vCPU 数量。 |
| `JOBS` | `$SMP` | guest Cargo 并发数。 |
| `MEM` | `8192M` | QEMU 内存大小。 |
| `QEMU_APPEND` | `someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off` | macOS/HVF 使用的通用 AArch64 平台 bootarg。 |
| `QEMU_SNAPSHOT` | `0` | 自举编译需要提取产物，因此必须保持为 `0`。 |
| `PREPARE_OVERLAY` | `1` | 构建并注入 app overlay 到复制出来的工作 rootfs。 |
| `ARTIFACT_EXTRACT` | `1` | QEMU 退出后从工作 rootfs 提取 guest-built 内核。 |
| `ARTIFACT_OUT_DIR` | `target/starry-macos-selfbuild/uploaded` | host 侧内核产物输出目录。 |
| `STARRY_KALLSYMS_RESERVED` | `16M` | guest kallsyms 刷新前使用的临时 linker 预留空间。 |

## 日志和报告

每次运行的日志在：

```text
target/starry-macos-selfbuild/logs/
target/starry-macos-selfbuild/work/
```

这个分支的开发报告维护在：

```text
tmp/macos-selfbuild-report.md
```

## 维护说明

- app 专用的 rootfs 准备、QEMU wrapper 和产物提取逻辑保留在
  `apps/starry/macos-selfbuild` 内。
- apps 外的修改保持通用：rootfs resize 是 xtask image 操作，timer 选择是通用
  AArch64 bootarg，GICD SPI 访问是通用 GIC 能力开关。
- 不为 timer 或 GIC 行为增加 app-private Cargo feature。
- 不注入预生成 binding 源码；应修复 guest 构建环境。
