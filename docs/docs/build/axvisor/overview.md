---
sidebar_position: 1
sidebar_label: "概述"
---

# Axvisor

Axvisor 构建固定 package `axvisor`，并可在 Build Config 或 CLI 中选择一个或多个 VM 配置。它的核心职责是构建 hypervisor 本体；guest 的内存、设备、启动协议、firmware 路径等由 `vm_configs` 及 QEMU TOML 显式描述。

## 1. 命令边界

Axvisor 命令在固定 hypervisor package 上叠加 VM 描述选择；Build Config、VM TOML 和 host QEMU TOML 由不同入口消费。下表概括各命令的责任。

| 命令 | 职责 |
| --- | --- |
| `build` | 构建 Axvisor ELF |
| `qemu` | 准备 rootfs、读取 QEMU TOML 并启动 |
| `uboot` / `board` | 通过 U-Boot 或远程板卡启动 |
| `test qemu` / `test board` | QEMU 与板卡回归 |
| `test uboot` | Axvisor 专属的 U-Boot 真实板卡测试 |
| `defconfig <board>` / `config ls` | 选择或列出 `os/axvisor/configs/board/` 模板 |

通用参数是 `--config`、`--arch`、`--target`、`--smp`、`--debug`；`--vmconfigs <PATH>` 可以重复传入。QEMU 另有 `--qemu-config` 和 `--rootfs`。

## 2. 配置布局

### 2.1 文件组织

host board/QEMU 配置与 guest VM 配置分目录存放，这让 target 能力、host 启动和 guest 引导协议可独立演进。下列路径与 `axvisor/board.rs` 的发现规则一致。

```text
os/axvisor/configs/
├── board/                 # target + BuildInfo + 可选 vm_configs
│   └── qemu-x86_64.toml
├── qemu/                  # host QEMU boot contract
│   └── qemu-x86_64.toml
└── vms/                   # guest VM 描述，按 qemu/或具体板卡组织
    └── qemu/x86_64/linux-vmx-smp1.toml
```

请使用 `configs/vms/`（复数）路径。`configs/vm/` 是过时的文档路径，当前源码树不存在该目录。

Axvisor board Build Config 的额外字段为：

```toml
target = "x86_64-unknown-none"
features = ["ax-driver/virtio-blk", "vmx"]
vm_configs = ["os/axvisor/configs/vms/qemu/x86_64/linux-vmx-smp1.toml"]
```

### 2.2 VM 选择

CLI 传入的 `--vmconfigs` 非空时覆盖该配置中的 `vm_configs`；否则使用 Build Config 中的列表。相对 VM 路径相对于 workspace 根解析后写入 `AXVISOR_VM_CONFIGS`，以平台路径分隔符连接。

## 3. 虚拟化后端

`vmx` 和 `svm` 是 Axvisor x86 Build Config 的显式 capability。Intel 配置选择 `vmx`，AMD 配置选择 `svm`；`BuildInfo` 将选中的 feature 传入 Axvisor Cargo 构建，测试 QEMU TOML 则定义对应 CPU 扩展。

相应 QEMU CPU flags 也属于启动配置。例如仓库的 VMX 和 SVM 测试 build 配置分别位于：

```text
test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-vmx.toml
test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-svm.toml
```

## 4. 默认配置

`cargo xtask axvisor defconfig <board>` 将 board TOML 写到：

```text
tmp/axbuild/config/axvisor/build-<target>.toml
```

并更新 Snapshot 的 arch、target、config，清除旧 QEMU/U-Boot 路径。它保留已有 Snapshot 中的 `vmconfigs`，因此 CLI 选择的 guest 不会因切换 defconfig 被无意丢弃。

首次读取一个缺失的默认 Build Config 时，Axvisor 也会查找同 target 且名称以 `qemu-` 开头的 board 配置；找到即复制，找不到才写入空的默认 BuildInfo。

## 5. 命令示例

以下命令分别覆盖 QEMU guest 选择、x86 后端测试与 defconfig 流程。

```bash
# 默认 aarch64，指定 QEMU guest 描述
cargo xtask axvisor qemu \
  --vmconfigs os/axvisor/configs/vms/qemu/aarch64/linux-smp1.toml

# x86 VMX 和 SVM 是不同的显式构建契约
cargo xtask axvisor test qemu --arch x86_64 --test-case smoke-vmx
cargo xtask axvisor test qemu --arch x86_64 --test-case smoke-svm

# 板卡配置
cargo xtask axvisor config ls
cargo xtask axvisor defconfig qemu-x86_64
cargo xtask axvisor build
```

详见 [构建](./build)、[运行](./runtime) 和 [测试](./test)。
