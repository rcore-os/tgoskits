# StarryOS 自编译

## x86_64：直接通过 Starry App 运行

x86_64 自编译的唯一主入口是：

```bash
cargo starry app qemu -t selfhost/selfhost-full-kernel --arch x86_64
```

该命令使用项目的 Starry app runner：构建种子内核、创建或复用 app 专用 rootfs、执行
`prebuild.sh`、注入 overlay，并通过 `shell_init_cmd` 启动来宾 runner。它不依赖
`scripts/self-compile.sh`、`expect`、loop mount 或 host sudo。

### 来宾流程

首次运行时，app runner 从默认 Alpine rootfs 创建受管理的
`rootfs-x86_64-selfhost.img`，并由 prebuild 扩容到 16 GiB。prebuild 会把当前 checkout
（包含未提交修改）和 source metadata 打包注入 rootfs；不会复制 host 的 Rust 或 GNU
二进制。

QEMU 通过 user-mode networking 联网。来宾 runner 会：

1. 用 `apk` 安装构建依赖、`libudev-zero-dev`、git 和 curl；
2. 原生安装 `nightly-2026-05-28` 的 `x86_64-unknown-linux-musl` Rust toolchain；
3. 安装 `rust-src`、`llvm-tools-preview`、`x86_64-unknown-none`、`cargo-binutils 0.4.0`
   和 `ksym 0.6.0`；
4. 将源码解包到 `/tmp`，以 musl host triple 编译 `tg-xtask`，然后执行 canonical
   `tg-xtask starry build -c apps/starry/selfhost/build-x86_64-unknown-none.toml --arch x86_64`；
5. 将生成的 ELF 持久化为 `/opt/starryos-selfbuilt`，并输出 `SELF_COMPILE_SUCCESS`。

失败会输出 `SELF_COMPILE_FAILED` 并让 app 命令以非零状态退出。成功前会先 `sync`，因此
app runner 检测到成功标记后可安全结束 QEMU。

### 前置条件

- Linux x86_64 host，建议可访问 `/dev/kvm`；
- `qemu-system-x86_64`、OVMF、`debugfs` 和 ext4 resize 工具；
- host 和 QEMU guest 都可访问网络。

正常流程不需要 sudo。第一次运行需要下载 Alpine 包、Rust toolchain 与 Cargo crates，后续
运行复用同一个 app rootfs 中的系统包、Rust 和 Cargo 缓存。

### 验证并启动自编译产物

app 成功后，先确认 rootfs 中的产物非空：

```bash
ROOTFS="${TGOS_IMAGE_LOCAL_STORAGE:-$PWD/tmp/axbuild/rootfs}/rootfs-x86_64-selfhost.img/rootfs-x86_64-selfhost.img"
debugfs -R 'stat /opt/starryos-selfbuilt' "$ROOTFS"
```

可选的二次启动烟测仍使用现有脚本，但它只消费 app 生成的 rootfs，不再调用
`self-compile.sh`：

```bash
scripts/run-selfbuilt-kernel.sh --arch x86_64
```

脚本会重新从 rootfs 提取 ELF，转换为 EFI payload，并通过 OVMF 启动。QEMU 到达
`root@starry:` 提示符表示自编译产物可以引导。

## riscv64 旧流程

riscv64 的离线 Debian selfhost 流程和 `scripts/self-compile.sh` 保持不变。它是与 x86_64
app runner 独立的兼容路径；不要用该脚本准备或运行 x86_64 自编译。
