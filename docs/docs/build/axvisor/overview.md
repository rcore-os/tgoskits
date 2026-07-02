---
sidebar_position: 1
sidebar_label: "概述"
---

# Axvisor

Axvisor 是 TGOSKits 的 Hypervisor 子系统。与 [ArceOS](../arceos/overview)（app 模块化）和 [StarryOS](../starry/overview)（单内核）不同，Axvisor 编译的是**虚拟机监控器**本身，同时需要管理一个或多个 Guest VM 的配置（`--vmconfigs`）、Guest 镜像和 rootfs。Axvisor 也是**唯一支持 `test uboot` 测试模式**和**唯一需要 LVZ 扩展版 QEMU（loongarch64）**的子系统。

Axvisor 复用 axbuild 的全部公共能力——构建流水线、QEMU/U-Boot/板卡运行、测试框架——这些公共部分见：

- 构建过程(../build_process)：八阶段流水线、Feature 解析、axconfig 生成
- 运行时环境(../runtime)：QEMU/U-Boot/板卡三种运行目标、loongarch64 LVZ QEMU 定位
- 测试框架(../test_framework)：用例发现、分组构建、资产准备、结果判定
- [Axvisor 测试](./test)：QEMU / U-Boot / Board 测试模式

本文只描述 Axvisor 特有的命令结构、参数和行为。

## 子命令

```text
cargo xtask axvisor <subcommand> [options]
```

| 子命令 | 说明 |
|--------|------|
| `build` | 编译 Axvisor |
| `qemu` | 编译并在 QEMU 中运行（含 rootfs/guest 镜像准备） |
| `uboot` | 编译并通过 U-Boot 运行 |
| `board` | 编译并在远程板卡运行 |
| `test qemu` | QEMU 测试 |
| `test uboot` | U-Boot 测试（**Axvisor 独有**） |
| `test board` | 板级测试 |
| `defconfig <board>` | 生成默认板卡配置 |
| `config ls` | 列出可用板卡名称 |

## 参数

**通用参数**（`build` / `qemu` / `uboot` / `board`）：`--arch`、`--target`、`--config`、`--plat-dyn`/`--plat_dyn`、`--smp`、`--debug`、`--vmconfigs`。默认架构为 `aarch64`。

**QEMU 额外参数**：`--qemu-config`、`--rootfs`
**Board 额外参数**：`--board-config`、`-b/--board-type`、`--server`、`--port`
**测试参数**（`test qemu`）：`--test-group`、`--test-case`、`--list`
**测试参数**（`test board`）：`--test-group`、`--test-case`、`--board`、`--board-type`、`--server`、`--port`、`--list`
**U-Boot 测试参数**（`test uboot`）：`--board`（必需）、`--guest`、`--uboot-config`

## 特有行为

### `--vmconfigs`：多 Guest VM 配置

Axvisor 的核心特有参数 `--vmconfigs <PATH>...` 指定一个或多个 VM 配置文件列表。每个 VM 配置描述一个 Guest（如 Linux、StarryOS guest）的内存、CPU、设备和启动来源。Axvisor 在 QEMU 运行前会准备所有引用的 rootfs 和 guest 镜像。

### `defplat → myplat` feature 归一化

Axvisor 的 board 配置通常声明 `ax-std/defplat`（"使用默认平台"），但 Cargo 编译需要 `ax-std/myplat`（"使用自定义平台"）才能正确启用静态平台绑定。`axbuild` 通过 `normalize_axvisor_platform_features()` 在两处执行归一化——`BuildInfo` 解析后和 `patch_axvisor_cargo_config()` 最终组装时——把 `defplat` 替换为 `myplat`，并在既非动态平台又无任何平台 feature 时自动注入 `myplat`。详见 [构建过程 §7](../build_process#7-cargo-配置组装)。

### 默认板卡配置自动复制

Axvisor 首次构建时（无 Build Info）会**优先从 `os/axvisor/configs/board/` 查找与 target 匹配的默认板卡配置并复制**到 Build Info 路径，找不到时才写入清空 features 的默认 BuildInfo。这与 ArceOS/StarryOS 直接写入代码默认值不同。StarryOS 的板卡默认配置通过 `cargo starry defconfig <board>` 显式生成，不在普通首次构建时自动复制。详见 [构建过程 §4](../build_process#4-build-info-加载或创建)。

### LoongArch LVZ QEMU

Axvisor 的 loongarch64 target 需要带 **LVZ（Loongson Virtualization Extension）** 的定制 QEMU，标准发行版的 QEMU 不包含此扩展。`AppContext::scoped_qemu_path()` 按以下优先级定位 LVZ 版 QEMU：

1. `AXBUILD_QEMU_SYSTEM_LOONGARCH64`（指向可执行文件）
2. `AXBUILD_QEMU_DIR`（指向目录）
3. `$HOME/QEMU-LVZ/build`、`$HOME/qemu-lvz/build`
4. workspace 根及其祖先目录下的 `QEMU-LVZ/build`、`qemu-lvz/build`

找到后通过 `PathRestoreGuard`（RAII）临时把该目录注入 PATH 最前面，运行结束后恢复原始 PATH。详见 [运行时环境 §LoongArch 特殊处理](../runtime#loongarch-特殊处理)。

### LoongArch Linux Guest UEFI firmware

若 `--vmconfigs` 中的 Linux guest 使用 `/guest/linux/linux-qemu`，`axvisor/rootfs.rs` 会把 VM config 复制到 `tmp/axbuild/axvisor/loongarch64/` 并填入可找到的 LoongArch UEFI firmware 路径，搜索顺序：

1. `/tmp/ostool/ovmf/loongarch64/code.fd`
2. `tmp/ostool/ovmf/loongarch64/code.fd`
3. `tmp/loongarch-uefi-stage1/assets/qemu-binary/QEMU_EFI.fd`

### 独有的 `test uboot` 模式

Axvisor 是唯一支持 U-Boot 测试模式的子系统。`cargo axvisor test uboot --board <TYPE>` 在远程板卡上通过 U-Boot 引导 Axvisor 和 Guest，验证 hypervisor 在真实硬件 + U-Boot 链路上的行为。参数：`--board`（必需）、`--guest`（指定 guest 镜像）、`--uboot-config`。

### 测试结构

Axvisor 测试位于 `test-suit/axvisor/normal/<case>/qemu-{arch}.toml`，复用与 StarryOS 相同的测试基础设施（用例发现、资产准备、结果判定），因为两者都是完整 OS/Hypervisor 级别的测试，需要在 rootfs 用户空间中执行测试命令。五种 pipeline 类型（plain/grouped/C/sh/python）的处理逻辑完全相同。详见 [Axvisor 测试](./test)。

## 用法示例

```bash
# 构建 Axvisor（默认 aarch64）
cargo axvisor build

# 在 QEMU 中运行，指定 Guest VM 配置
cargo axvisor qemu --vmconfigs os/axvisor/configs/vm/aarch64-linux.toml

# 多个 Guest
cargo axvisor qemu \
    --vmconfigs configs/vm/aarch64-linux.toml \
    --vmconfigs configs/vm/aarch64-starry.toml

# 板卡流程
cargo axvisor config ls
cargo axvisor defconfig <board>
cargo axvisor board

# loongarch64（自动定位 LVZ QEMU）
cargo axvisor build --arch loongarch64
cargo axvisor qemu  --arch loongarch64 --vmconfigs configs/vm/loongarch64-linux.toml

# U-Boot 测试（Axvisor 独有）
cargo axvisor test uboot --board OrangePi-5-Plus --guest <image>
```

## 模块组成

| 代码位置 | 作用 |
|----------|------|
| `scripts/axbuild/src/axvisor/mod.rs` | CLI 入口、`Command` 枚举、参数定义 |
| `scripts/axbuild/src/axvisor/build.rs` | 构建配置加载、`patch_axvisor_cargo_config`（`defplat→myplat` 归一化） |
| `scripts/axbuild/src/axvisor/board.rs` | 板卡运行流程 |
| `scripts/axbuild/src/axvisor/rootfs.rs` | rootfs/guest 镜像准备、loongarch64 UEFI firmware 注入 |
| `scripts/axbuild/src/axvisor/config.rs` | `defconfig`、`config ls` |
| `scripts/axbuild/src/axvisor/test/` | QEMU/U-Boot/Board 测试流程 |
