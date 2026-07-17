---
sidebar_position: 1
sidebar_label: "概述"
---

# StarryOS

StarryOS 始终构建 workspace package `starryos`，不提供 ArceOS 式的 `--package` 选择。它在共享构建/运行/测试命令之上增加 rootfs、应用用例、性能剖析和内核模块工作流。

## 1. 命令边界

StarryOS 的固定内核构建单元使其命令围绕运行资产和扩展工作流展开。下表将顶层入口与其负责的工程对象对应，避免把 app、perf 或 kmod 当作普通内核构建的参数。

| 命令 | 职责 |
| --- | --- |
| `build` / `qemu` / `uboot` / `board` | 构建内核并在相应运行目标启动 |
| `test qemu` / `test board` | 运行 Starry 测试套件 |
| `rootfs` | 拉取并准备默认 managed rootfs |
| `app list/qemu/board` | 发现并运行 `apps/starry/` 的声明式应用用例 |
| `perf` | qperf 性能采集和报告 |
| `kmod build` | 构建 Rust 或 C 可加载模块，并可注入 rootfs |
| `defconfig <board>` / `config ls` | 选择或列出 checked-in board build config |
| `quick-start` | 常用平台的便捷构建和运行入口 |

默认 arch 是 `riscv64`。共享参数为 `--config`、`--arch`、`--target`、`--smp`、`--debug`；QEMU 额外接受 `--qemu-config` 与 `--rootfs`。

## 2. 配置选择

### 2.1 配置布局

Starry 的 board 文件为 build/qemu/kmod 提供 target 与能力基线，QEMU 文件单独描述启动。该布局也解释了为什么缺失默认配置时必须能找到对应 `qemu-*` board 文件。

```text
os/StarryOS/configs/
├── board/                 # target + BuildInfo；部分 board 配有同名 .its
│   ├── qemu-riscv64.toml
│   └── orangepi-5-plus.toml
└── qemu/                  # QEMU boot contract
    └── qemu-riscv64.toml
```

### 2.2 默认配置

`build`、`qemu` 和 `kmod build` 调用 `ensure_default_build_config_for_target()`。默认路径缺失时，必须找到 target 对应的 `qemu-*` board 文件，复制到：

```text
tmp/axbuild/config/starryos/build-<target>.toml
```

若 board 配置有同名 `.its`，复制时也会复制到构建配置旁，供 Starry 的 uImage 生成流程使用。`defconfig <board>` 完成同样的复制并更新 Snapshot；隐式补齐默认配置不会修改 Snapshot。

## 3. 构建后处理

Starry 先用共享 std-aware Cargo 逻辑构建 ELF，之后 `postprocess_starry_artifact()` 进行两项处理：

1. 用 `rust-nm -n` 收集符号、调用 `gen_ksym`，再用 `rust-objcopy --update-section` 写回保留的 `.kallsyms` section；生成内容超过该 section 时明确失败，避免静默截断。
2. 当 Build Config 旁存在 `.its` 时，以 `mkimage` 生成 uImage；ITS 文件提供镜像的启动描述。

最终的构建产物依然是 ELF；运行配置的 `to_bin` 决定需要启动时是否额外准备 BIN。

## 4. 根文件系统

Starry QEMU 运行以 managed rootfs 为常规路径。image storage 的默认 rootfs 名称由 arch 决定，例如 `rootfs-riscv64-alpine.img`；`--rootfs` 可以选择显式路径或 managed 镜像别名。QEMU TOML 中的 `-drive` 合同会被替换为选中的 rootfs 路径。

`cargo xtask starry rootfs` 是预拉取和准备入口：它调用 `ensure_rootfs_for_arch()` 后在受锁保护的镜像上更新 APK 镜像区域。其余 QEMU、app 和测试路径会按各自的资产和 rootfs 规则调用统一 image/rootfs 基础设施。详见 [rootfs 准备](./rootfs)。

## 5. 命令示例

这些命令覆盖默认构建、显式 defconfig、rootfs 预取和两个扩展工作流，可用于验证配置和资产路径。

```bash
# 默认 riscv64
cargo xtask starry build
cargo xtask starry qemu

# 选择 QEMU defconfig
cargo xtask starry defconfig qemu-x86_64
cargo xtask starry qemu

# 准备 rootfs，运行 app 和模块
cargo xtask starry rootfs --arch aarch64
cargo xtask starry app qemu --all
cargo xtask starry kmod build --all
```

深入内容见 [构建](./build)、[运行](./runtime)、[测试](./test)、[应用运行](./app)、[性能剖析](./perf) 和 [内核模块](./kmod)。
