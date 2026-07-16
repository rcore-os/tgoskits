---
sidebar_position: 9
sidebar_label: "自编译"
---

# StarryOS 自编译

## x86_64：直接通过 Starry App 运行

x86_64 自编译的唯一主入口是：

```bash
cargo starry app qemu -t selfhost/selfhost-full-kernel --arch x86_64
```

当前验收状态：清理前的提交已在启用 KVM 的 QEMU 中完成来宾工具链安装，并将
`tg-xtask` 构建推进到 `453/454`、即最终大型静态链接前；该次链接因耗时过长被人工终止。
因此目前能够证明绝大多数 Rust crate 可编译，但尚未证明最终链接完成、
`/opt/starryos-selfbuilt` 已发布或自编译 ELF 能通过 OVMF 启动。本次清理后的提交只运行定向
回归测试，不重复数小时端到端构建。最终链接性能优化和启动烟测留待后续完成。

该命令使用项目的 Starry app runner：构建种子内核、创建或复用 app 专用 rootfs、执行
`prebuild.sh`、注入 overlay，并通过 `shell_init_cmd` 启动来宾 runner。它不依赖
`scripts/self-compile.sh`、`expect`、loop mount 或 host sudo。

### 来宾流程

首次运行时，app runner 从默认 Alpine rootfs 创建受管理的
`rootfs-x86_64-selfhost.img`，并由 prebuild 扩容到 32 GiB。prebuild 会把当前 checkout
（包含未提交修改）和 source metadata 打包注入 rootfs。它还会从 Rust 官方发布站点下载并
校验固定 nightly 的六个组件，在 host 侧预解压为单一非压缩 tar 后注入；不复制 host 已安装
的 Rust 或 GNU 工具链。

QEMU 通过 user-mode networking 联网。来宾 runner 会：

1. 用 `apk` 安装构建依赖、`libudev-zero-dev`、git 和 curl；
2. 将预处理的 `nightly-2026-07-15-x86_64-unknown-linux-musl` Rust toolchain 解包到 rootfs，
   并在 MemoryFs 中安装 rustup；
3. 在线安装固定版本的 `cargo-binutils 0.4.0` 和 `ksym 0.6.0`；
4. 将源码解包到 `/tmp`，但把 canonical `target/` 链接到持久化的
   `/opt/starry-selfhost-target`，使用两个 Cargo jobs 编译 musl-host `tg-xtask`，然后执行
   `tg-xtask starry build -c apps/starry/selfhost/build-x86_64-unknown-none.toml --arch x86_64`；
5. 将生成的 ELF 持久化为 `/opt/starryos-selfbuilt`，并输出 `SELF_COMPILE_SUCCESS`。

失败会输出 `SELF_COMPILE_FAILED` 并让 app 命令以非零状态退出。成功前会先 `sync`，因此
app runner 检测到成功标记后可安全结束 QEMU。来宾还会把当前构建阶段写入 rootfs；若内核
在编译中途异常重启，下一次登录会输出包含中断阶段的失败标记，而不会等待到全局超时。

### 前置条件

- Linux x86_64 host，并可读写 `/dev/kvm`；app runner 检测到后会自动加入 `-accel kvm`，
  上述长时间构建实际启用了 KVM；
- `qemu-system-x86_64`、OVMF、`debugfs` 和 ext4 resize 工具；
- host 和 QEMU guest 都可访问网络。

正常流程不需要 sudo。第一次运行需要下载 Rust 官方组件、Alpine 包和 Cargo crates，后续运行
复用 app rootfs 中的系统包、Rust 和 Cargo 缓存。

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

脚本会重新从 rootfs 提取 ELF，转换为 EFI payload，并通过 OVMF 启动。该脚本当前保留为
后续烟测工具；本 PR 尚未完成产物生成和启动验证，不能据此宣称已经到达 Starry shell。

## riscv64 旧流程

riscv64 的离线 Debian selfhost 流程和 `scripts/self-compile.sh` 保持不变。它是与 x86_64
app runner 独立的兼容路径；不要用该脚本准备或运行 x86_64 自编译。
