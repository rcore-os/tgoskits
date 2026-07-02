---
sidebar_position: 2
sidebar_label: "测试"
---

# Axvisor 测试

Axvisor 复用了与 [StarryOS 测试](../starry/test) 相同的测试基础设施（用例发现、资产准备、结果判定），因为两者都是完整 OS/Hypervisor 级别的测试，需要在 rootfs 用户空间中执行测试命令。五种 pipeline 类型（plain/grouped/C/sh/python）的处理逻辑完全相同。

测试编排框架详见 测试框架(../test_framework)。本文只描述 Axvisor 特有的测试目录结构、三种测试模式（QEMU / U-Boot / Board）的差异，以及 Axvisor 独有的 `test uboot` 模式。

## 命令

```text
cargo xtask axvisor test qemu  [--test-group <g>] [--test-case <c>] [--list]
cargo xtask axvisor test uboot --board <type> [--guest <image>] [--uboot-config <cfg>]   # Axvisor 独有
cargo xtask axvisor test board --board <type> --server <h> --port <p> [--test-case <c>] [--list]
```

## 测试目录结构

Axvisor 测试资产位于：

```text
test-suit/axvisor/
└── normal/
    └── <case>/
        └── qemu-{arch}.toml
```

与 StarryOS 的平铺结构不同，Axvisor 用 `normal` 测试组目录组织用例。发现算法统一通过 `build-{target}.toml` 定位构建组、`qemu-{arch}.toml` 定位用例，详见 [测试框架 §测试目录结构](../test_framework#测试目录结构)。

## 三种测试模式

| 模式 | 命令 | 运行环境 | 适用场景 |
|------|------|----------|----------|
| `test qemu` | `cargo axvisor test qemu` | QEMU 虚拟机 | 常规功能验证（CI 主力） |
| `test uboot` | `cargo axvisor test uboot`（**Axvisor 独有**） | 远程板卡 + U-Boot 引导 | 验证 hypervisor 在真实硬件 + U-Boot 链路上的行为 |
| `test board` | `cargo axvisor test board` | 远程板卡 | 板级回归 |

### `test qemu`

最常用的测试模式，在 QEMU 中启动 Axvisor 和配置的 Guest VM，通过 `qemu-{arch}.toml` 中的 `success_regex`/`fail_regex` 判定结果。资产准备（rootfs、guest 镜像、VM 配置）在测试前完成，详见 运行时环境(../runtime)。

### `test uboot`（Axvisor 独有）

Axvisor 是唯一支持 U-Boot 测试模式的子系统。`cargo axvisor test uboot --board <TYPE>` 在远程板卡上通过 U-Boot 引导 Axvisor 和 Guest：

| 参数 | 说明 |
|------|------|
| `--board <TYPE>`（必需） | ostool-server 上的板卡类型 |
| `--guest <IMAGE>` | 指定 guest 镜像 |
| `--uboot-config <CFG>` | U-Boot 配置文件 |

该模式验证完整的"U-Boot → Axvisor → Guest"引导链路，覆盖真实硬件上 U-Boot 加载 Axvisor ELF、Axvisor 初始化硬件虚拟化扩展、再启动 Guest 的全流程。

### `test board`

板级测试通过 `board-{board_name}.toml` 配置文件定义，发现算法与 QEMU 测试一致，通过 `nearest_build_wrapper()` 确定构建配置。`--test-case` 和 `--board` 支持过滤。详见 [测试框架 §Board 用例发现](../test_framework#board-用例发现)。

## Pipeline 复用

Axvisor 测试的五种 pipeline 类型与 StarryOS 完全一致，因为两者都需要在 rootfs 用户空间中执行测试命令：

| Pipeline | 触发条件 | Axvisor 使用情况 |
|----------|----------|-----------------|
| Plain | 无资产子目录、无 `test_commands` | 最常见，纯 QEMU 启动验证 |
| Grouped | `test_commands` 非空 | 多命令聚合 case |
| C | 含 `c/` 子目录 | C 测试程序 |
| Shell | 含 `sh/` 子目录 | shell 脚本测试 |
| Python | 含 `python/` 子目录 | Python 测试 |

各 pipeline 的详细处理流程见 [测试框架 §资产准备](../test_framework#资产准备)。Axvisor 的 `prepare_staging_root` 钩子为空操作（`|_| Ok(())`），不做 StarryOS 那样的 DNS 注入和 APK 区域配置。
