---
sidebar_position: 2
sidebar_label: "构建"
---

# Axvisor 构建

`cargo xtask axvisor build` 把用户友好的高层参数转换为 Cargo 能理解的底层编译参数，最终调用 ostool 的 `cargo_build()` 完成 Axvisor（Hypervisor）编译。本节描述 Axvisor 构建的完整流程及其特有行为；通用的参数解析、Snapshot、Build Info 和动态平台构建约定详见 [参数与配置](../configuration)，运行详见 [Axvisor 运行](./runtime)。

构建过程与 [ArceOS](../arceos/build)、[StarryOS](../starry/build) 共享参数解析、arch/target 解析和 Build Info 加载逻辑。Axvisor 在 Build Info 默认值、旧平台选择项过滤和 VM 配置注入上有独有的行为。

## Axvisor 特有行为

### 编译 Hypervisor + 多 Guest VM 配置

与 [ArceOS](../arceos/build)（app 模块化）和 [StarryOS](../starry/build)（单内核）不同，Axvisor 编译的是**虚拟机监控器**本身，同时需要管理一个或多个 Guest VM 的配置（`--vmconfigs`）。`--vmconfigs <PATH>...` 指定 VM 配置文件列表，每个 VM 配置描述一个 Guest（如 Linux、StarryOS guest）的内存、CPU、设备和启动来源。

### 默认架构 `aarch64`

Axvisor 默认架构为 `aarch64`（`aarch64-unknown-none-softfloat`）。详见 [参数与配置 §默认值](../configuration#默认值)。

### Build Info 默认值：优先复制板卡配置

Axvisor 首次构建时（无 Build Info）会**优先从 `os/axvisor/configs/board/` 查找与 target 匹配的默认板卡配置并复制**到 Build Info 路径（`tmp/axbuild/config/<pkg>/build-<target>.toml`），找不到时才写入清空 features 的默认 BuildInfo。这与 ArceOS（`ArceosBuildConfig::default_config()`）和 StarryOS（`default_starry_build_info_for_target()`）直接写入代码默认值不同。

### 旧平台选择项过滤

Axvisor 的旧 board 配置中可能声明 `defplat`、`myplat`、`plat-dyn`、`ax-std/plat-dyn`、`axvm/plat-dyn`、`ax-driver/plat-dyn` 或 `axplat-dyn/*` 等历史平台选择项。当前构建固定走动态平台路径，`axbuild` 在 Build Info 读取和最终 Cargo 配置组装时过滤这些 feature，避免旧平台选择项泄漏到当前构建。

## 注入的环境变量

Axvisor 在 Cargo 配置组装阶段额外注入环境变量（与 ArceOS/StarryOS 不同）：

| 环境变量 | 值 | 用途 |
|----------|-----|------|
| `AX_ARCH` | 当前 arch | 编译期读取 |
| `AX_TARGET` | 当前 target triple | 编译期读取 |
| `AXVISOR_VM_CONFIGS` | `--vmconfigs` 列表 | 编译期读取 VM 配置 |

构建使用动态平台链接脚本 `Taxplat.x`。硬件信息来自启动时的固件表、FDT/ACPI 和 `somehal`/`axplat-dyn` 运行时发现结果；`axbuild` 不再生成 `.axconfig.toml`，也不再向 Cargo 注入 `AX_CONFIG_PATH`。

## 用法示例

```bash
# 构建 Axvisor（默认 aarch64）
cargo axvisor build

# 指定 Guest VM 配置
cargo axvisor build --vmconfigs os/axvisor/configs/vm/aarch64-linux.toml

# 多个 Guest
cargo axvisor build \
    --vmconfigs configs/vm/aarch64-linux.toml \
    --vmconfigs configs/vm/aarch64-starry.toml

# loongarch64
cargo axvisor build --arch loongarch64

# 板卡配置流程
cargo axvisor config ls
cargo axvisor defconfig <board>
cargo axvisor build
```
