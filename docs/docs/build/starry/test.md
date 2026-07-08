---
sidebar_position: 4
sidebar_label: "测试"
---

# StarryOS 测试

StarryOS 测试直接从 `test-suit/starryos/` 根目录发现用例，通过 **build wrapper**（含 `build-{target}.toml` 的目录）划分构建组。同一 wrapper 下的 case 共享一次内核构建，避免重复编译。

测试编排（用例发现、分组构建、资产准备、结果判定）由 `scripts/axbuild/src/test/` 提供统一框架，核心原则是 **OS 只构建一次，逐 case 运行**——具有相同构建配置的用例归入同一 build wrapper，组内共享一次内核编译。本文描述 StarryOS 特有的测试目录结构和用例组织方式。

## 命令

```text
cargo xtask starry test qemu --arch <arch> [--test-case <case>] [--list]
cargo xtask starry test board --board <type> --server <host> --port <port> [--test-case <case>] [--list]
```

`--arch` 与 `--target`/`--list` 三选一（`test qemu`）。

## 目录结构

StarryOS 的测试目录组织为**平铺 + build wrapper**：

```text
test-suit/starryos/
├── qemu-smp1/                    ← build wrapper（含 build-riscv64gc-unknown-none-elf.toml）
│   ├── build-riscv64gc-unknown-none-elf.toml
│   └── system/
│       └── qemu-riscv64.toml     ← 子 case（继承 wrapper 的 build config）
├── qemu-smp4/                    ← 另一个 build wrapper（不同 SMP 配置）
│   ├── build-riscv64gc-unknown-none-elf.toml
│   └── system/
│       └── qemu-riscv64.toml
└── board-*/                      ← 板级 build wrapper
    └── <case>/board-{board}.toml
```

与 [ArceOS 测试](../arceos/test)（区分 `rust/` 和 `c/` 子目录）不同，StarryOS 从 `test-suit/starryos/` 根目录直接发现 QEMU/board 用例，不按语言分子目录。

### Build Wrapper 的意义

`qemu-smp1` 和 `qemu-smp4` 分别测试单核和多核场景，它们的构建配置不同（SMP 核数不同），因此必须分别编译；每个 wrapper 下的 `system` 聚合用例使用完全相同的内核，只需编译一次并启动一次。发现算法通过识别 `build-{target}.toml` 文件来自动划分构建边界，build wrapper 是含 `build-{target}.toml` 的目录，定义一组共享相同构建配置的用例。

## QEMU 聚合 case

StarryOS 的 QEMU 用例通常是**聚合 case**（Grouped pipeline）——一次内核启动中通过 `test_commands` 顺序执行多条 shell 命令，每条命令独立判定结果。这与 ArceOS 的"feature 即用例"模式不同。

聚合 case 的资产准备（runner 脚本生成、ELF 依赖补全、rootfs 注入）逻辑由 `scripts/axbuild/src/test/case/` 提供。StarryOS 在 `prepare_staging_root` 钩子中还额外完成：

- **DNS 注入**（`starry/resolver.rs`）：读取宿主 DNS 配置写入 staging `/etc/resolv.conf`，过滤 loopback 和 QEMU slirp 地址
- **APK 区域配置**（`starry/apk.rs`）：根据 `STARRY_APK_REGION` 重写 `/etc/apk/repositories` 镜像源（`china`/`cn` 用 `mirrors.cernet.edu.cn/alpine`，`us`/`usa` 用 `dl-cdn.alpinelinux.org`）

`STARRY_APK_REGION` 的运行时取值会纳入 rootfs 缓存键，因此切换区域会使缓存失效。

## Board 测试

板级用例通过 `board-{board_name}.toml` 配置文件定义，发现算法递归扫描目录匹配 `board-*.toml`，每个 board case 通过 `nearest_build_wrapper()` 向上查找最近的 build wrapper 确定构建配置。`--test-case` 和 `--board` 支持按用例名和板卡名过滤。board 配置按板卡名命名（`board-{name}.toml`），通过 `nearest_build_wrapper()` 向上查找最近的构建配置。

## GroupedCaseRunnerConfig

StarryOS 的 grouped runner 标记前缀由 `GroupedCaseRunnerConfig` 定义（如 `STARRY`），生成的日志形如：

```text
STARRY_GROUPED_TEST_BEGIN: /usr/bin/test-a
STARRY_GROUPED_TEST_PASSED: /usr/bin/test-a
STARRY_GROUPED_TESTS_PASSED
```

axbuild 通过这些结构化标记精确统计每条命令的通过/失败状态。
