# StarryOS macOS AArch64 自举编译

这个 app 用来在 Apple Silicon macOS 上复现 StarryOS 自举编译，验证环境是
Apple M3。流程复用现有 Starry app runner：host 侧构建 AArch64 StarryOS
种子内核，host 侧 `prebuild.sh` 准备 app overlay，app runner 注入 overlay
并用 QEMU HVF 启动 StarryOS guest，然后在 guest 里面直接运行 Cargo。构建
结束后，`full_self_build.sh` 从 app runner 使用的 rootfs 中取回 guest 编译
出的内核，并再次用普通 QEMU 命令启动验证。


## 具体流程

`full_self_build.sh` 是完整默认入口，流程按下面几个 stage 组织：

| 阶段 | 命令/入口 | 作用 |
| --- | --- | --- |
| Stage 1 | `prepare_host_tools.sh` | 准备 macOS host 上构建 AArch64 种子内核需要的工具 wrapper |
| Stage 2 | `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64` | 使用现有 Starry app runner 构建种子内核、确保 rootfs、执行 `prebuild.sh`、通过内部 `rootfs::inject::inject_overlay()` 注入 overlay，并启动 QEMU/HVF |
| Stage 2 / prebuild | `cargo xtask image resize <ROOTFS> --size-mib 16384` | host 侧 `prebuild.sh` 在 overlay 注入前扩容 app runner 选中的 rootfs |
| Stage 3 | QEMU/HVF guest Cargo 构建 | StarryOS guest 启动后，由 `shell_init_cmd` 执行 guest runner，并在 guest 内直接运行 `cargo build` |
| Stage 4 | `debugfs` 提取产物 | 从 app runner 使用的 rootfs 提取 guest-built 内核 ELF 和 `.bin` |

## 脚本职责

| 脚本 | 角色 | 做什么 |
| --- | --- | --- |
| `full_self_build.sh` | 完整入口 | 准备 host 工具，调用现有 Starry app QEMU runner，并在 runner 成功后从 rootfs 提取 guest-built 产物。通常只需要运行这个脚本。 |
| `prebuild.sh` | host 侧 app runner prebuild | 由 app runner 在 host OS 上执行；接收 `STARRY_ROOTFS` 和 `STARRY_OVERLAY_DIR`，扩容选中的 rootfs，组装待注入 overlay：复制工具链 overlay、打包当前源码、复制离线 Cargo registry cache、写入 guest runner 和源码 metadata。它不负责注入 overlay，也不启动 QEMU。 |
| `prepare_toolchain_overlay.sh` | 内部/调试脚本 | 下载并准备 guest 里的 Rust/Cargo、Rust 源码、LLVM/libclang、musl C 工具链和 Cargo cache。输出是目录树，不是 rootfs 镜像；默认由 `prebuild.sh` 调用。 |
| `prepare_host_tools.sh` | 内部/调试脚本 | 准备 macOS host 上构建种子内核所需的 AArch64 musl 编译器 wrapper、`rust-nm`、`rust-objdump` 等工具。 |
| `guest-selfbuild.sh` | guest 内脚本 | 在 StarryOS guest 中解包源码、写 Cargo 配置、执行 Cargo 构建、刷新 kallsyms，并复制 guest-built 内核产物。 |

rootfs 由 axbuild image storage 选择；这个 app 不维护单独的 rootfs 副本。
干净默认运行时，路径是：

```text
tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

如果设置了 `TGOS_IMAGE_LOCAL_STORAGE`，axbuild 会使用对应的 image storage。
`prebuild.sh` 会把本次 app runner 实际使用的 rootfs 记录到：

```text
target/starry-macos-selfbuild/rootfs.path
```

app runner 通过现有内部 `rootfs::inject::inject_overlay()` 路径把自举 overlay
注入这个 rootfs。本 app 不暴露也不依赖新的公共注入命令。

因为 guest-built 产物需要写回 rootfs，`qemu-aarch64.toml` 设置
`snapshot = false`，让 Starry app runner 不追加全局 `-snapshot`。下面的
单独启动验证命令仍然带 `-snapshot`，它只用于验证提取出的 `.bin` 能正常启动，
不需要把 shell 写入持久化回 rootfs。

## 前置依赖

在 Apple Silicon macOS 上安装 host 工具：

```bash
brew install qemu e2fsprogs zig llvm
```

其中，`qemu` 用于 HVF 启动，`e2fsprogs` 提供 `e2fsck`、`debugfs` 和
`resize2fs`，`zig` 用于生成 AArch64 musl 编译器 wrapper，`llvm` 用作
`rust-nm`、`rust-objdump` 等工具的 fallback。

## 完整复现

### 1.开始自举编译：

在仓库根目录执行
```bash
apps/starry/macos-selfbuild/full_self_build.sh
```

这个入口会自己调用 `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64`，
一般不需要手动运行更底层的脚本。

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
  -cpu cortex-a53 \
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

## 验证与耗时

本次耗时是删除 `target` 目录和默认 rootfs image 后，没有缓存的情况下，在
Apple M3 上运行默认 `full_self_build.sh` 流程得到的。


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

这次运行的自举输出耗时如下：

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=1460===
```

PASS marker 的 elapsed 为 `1460s`，即 `24m 20s`。它从 guest 即将执行
`cargo build` 前开始，到 Cargo 命令返回后结束；它包括 guest 内 Cargo 构建、
build script、build-std 和链接时间，不包括 QEMU 启动、kallsyms 刷新后的
产物复制和 host 侧提取。

第二步：使用 qemu 启动自举编译产物

```bash
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53 \
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

自举产物的启动验证到达：

```text
root@starry:/root #
```

这说明 guest 编译出的 `.bin` 可以作为普通 StarryOS 内核启动。
