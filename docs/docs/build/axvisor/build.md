---
sidebar_position: 2
sidebar_label: "构建"
---

# Axvisor 构建

`cargo xtask axvisor build` 把用户友好的高层参数转换为 Cargo 能理解的底层编译参数，最终调用 ostool 的 `cargo_build()` 完成 Axvisor（Hypervisor）编译。本节描述 Axvisor 构建的完整流程及其特有行为；通用的参数解析、Snapshot、Build Info、axconfig 机制详见 [参数与配置](../configuration)，运行详见 [Axvisor 运行](./runtime)。

构建过程分八个阶段，与 [ArceOS](../arceos/build)、[StarryOS](../starry/build) 共享前四个阶段。Axvisor 在 Build Info 默认值、feature 归一化和 VM 配置注入上有独有的行为。

## Axvisor 特有行为

### 编译 Hypervisor + 多 Guest VM 配置

与 [ArceOS](../arceos/build)（app 模块化）和 [StarryOS](../starry/build)（单内核）不同，Axvisor 编译的是**虚拟机监控器**本身，同时需要管理一个或多个 Guest VM 的配置（`--vmconfigs`）。`--vmconfigs <PATH>...` 指定 VM 配置文件列表，每个 VM 配置描述一个 Guest（如 Linux、StarryOS guest）的内存、CPU、设备和启动来源。

### 默认架构 `aarch64`

Axvisor 默认架构为 `aarch64`（`aarch64-unknown-none-softfloat`）。详见 [参数与配置 §默认值](../configuration#默认值)。

### Build Info 默认值：优先复制板卡配置

Axvisor 首次构建时（无 Build Info）会**优先从 `os/axvisor/configs/board/` 查找与 target 匹配的默认板卡配置并复制**到 Build Info 路径（`tmp/axbuild/config/<pkg>/build-<target>.toml`），找不到时才写入清空 features 的默认 BuildInfo。这与 ArceOS（`ArceosBuildConfig::default_config()`）和 StarryOS（`default_starry_build_info_for_target()`）直接写入代码默认值不同。

### `defplat → myplat` feature 归一化

Axvisor 的 board 配置通常声明 `ax-std/defplat`（"使用默认平台"），但 Cargo 编译需要 `ax-std/myplat`（"使用自定义平台"）才能正确启用静态平台绑定。`axbuild` 通过 `normalize_axvisor_platform_features()` 在两处执行归一化：

1. **`BuildInfo` 解析后**：把 `defplat` 替换为 `myplat`
2. **`patch_axvisor_cargo_config()` 最终组装时**：再次归一化，并在既非动态平台又无任何平台 feature 时自动注入 `myplat`

这确保 Axvisor 的静态平台编译始终正确。

## 注入的环境变量

Axvisor 在 Cargo 配置组装阶段额外注入环境变量（与 ArceOS/StarryOS 不同）：

| 环境变量 | 值 | 用途 |
|----------|-----|------|
| `AX_ARCH` | 当前 arch | 编译期读取 |
| `AX_TARGET` | 当前 target triple | 编译期读取 |
| `AXVISOR_VM_CONFIGS` | `--vmconfigs` 列表 | 编译期读取 VM 配置 |

链接器参数：`plat_dyn=true` 用 `-Clink-arg=-Taxplat.x`，静态平台用 `-Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc`。

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
