---
sidebar_position: 2
sidebar_label: "构建"
---

# StarryOS 构建

`cargo xtask starry build` 把用户友好的高层参数（`--arch`、`--smp`）转换为 Cargo 能理解的底层编译参数，最终调用 ostool 的 `cargo_build()` 完成 StarryOS 内核编译。本节描述 StarryOS 构建的完整流程及其特有行为；通用的参数解析、Snapshot、Build Info 和动态平台构建约定详见 [参数与配置](../configuration)，运行详见 [StarryOS 运行](./runtime)。

构建过程与 [ArceOS](../arceos/build)、[Axvisor](../axvisor/build) 共享参数解析、arch/target 解析和 Build Info 加载逻辑，在 Feature 解析和 Build Info 默认值上分化。

## StarryOS 特有行为

### 编译整个内核，无需 `--package`

StarryOS 编译完整的 Linux 兼容内核镜像（单一 ELF），不存在 [ArceOS](../arceos/overview) 那样的"多 app 选择"问题。`--package` 对 StarryOS 不适用——构建目标始终是 `os/StarryOS/` 下的内核 crate。

### 默认架构 `riscv64`

StarryOS 的默认架构是 `riscv64`（`riscv64gc-unknown-none-elf`），与 ArceOS 和 Axvisor 的 `aarch64` 不同，反映其最常用的开发和测试目标。详见 [参数与配置 §默认值](../configuration#默认值)。

### Build Info 默认值

初次构建时 StarryOS 写入 `default_starry_build_info_for_target()`，会清空默认 features 并走动态平台路径。这与 ArceOS（`ArceosBuildConfig::default_config()`）和 Axvisor（优先从 `configs/board/` 复制）不同。

### 动态平台固定启用

StarryOS 当前构建固定走 `axplat-dyn` 路径。Build Info 中不再提供平台选择开关，旧 `plat_dyn` 字段会被拒绝；旧平台选择 feature 会在最终 Cargo 配置中被过滤。

## 注入的环境变量

StarryOS 在 Cargo 配置组装阶段额外注入三个环境变量（与 ArceOS/Axvisor 不同）：

| 环境变量 | 值 | 用途 |
|----------|-----|------|
| `AX_ARCH` | 当前 arch（如 `riscv64`） | 内核编译期读取 |
| `AX_TARGET` | 当前 target triple | 内核编译期读取 |
| `AX_PLATFORM` | 平台名 | 内核编译期读取 |

## 用法示例

```bash
# 默认 riscv64 构建
cargo starry build

# 切换架构
cargo starry build --arch aarch64

# 板卡配置流程（推荐）
cargo starry config ls
cargo starry defconfig orangepi-5-plus
cargo starry build

# 多核构建
cargo starry build --smp 4

```
