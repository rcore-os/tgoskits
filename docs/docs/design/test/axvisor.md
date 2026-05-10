---
sidebar_position: 6
sidebar_label: "Axvisor 测试套件"
---

# Axvisor 测试套件设计

Axvisor 的 QEMU 与 board 测试用例放在 `test-suit/axvisor/normal/<case>/` 下，
由 `scripts/axbuild/src/axvisor/test.rs` 发现。系统构建配置和 VM 配置仍复用
`os/axvisor/configs/`。

从当前 `scripts/axbuild` 实现看，Axvisor QEMU 测试与 StarryOS QEMU 测试已经采用基本一致的执行模型：先发现本轮全部 case，汇总构建配置和 VM 配置，hypervisor 只构建一次，然后逐 case 加载 QEMU 运行配置并执行。rootfs 也走统一的 managed rootfs/VM config 推导路径，不再按 case 复制多份。

## 1. 入口

当前 Axvisor 测试的权威实现入口主要在：

- `scripts/axbuild/src/axvisor/mod.rs`
- `scripts/axbuild/src/axvisor/test.rs`
- `scripts/axbuild/src/test/`

其中：

- `mod.rs` 负责 `test qemu`、`test uboot`、`test board` 三类命令的调度
- `test.rs` 负责发现 QEMU/board 测例，以及维护 U-Boot 白名单
- `test/` 目录保存共享的 QEMU case assets、board 汇总、target 解析等公共能力

## 2. 类型

| 类型 | 说明 | 运行命令 |
|------|------|----------|
| QEMU 测试 | 在 QEMU 中启动 hypervisor 并运行 Guest | `cargo xtask axvisor test qemu --target <arch>` |
| U-Boot 测试 | 通过 U-Boot 引导 hypervisor | `cargo xtask axvisor test uboot --board <board> --guest <guest>` |
| 板级测试 | 在物理开发板上运行 | `cargo xtask axvisor test board [--board <board>] [--test-group <group>] [--test-case <case>]` |

## 3. QEMU

QEMU 测试从 `test-suit/axvisor/normal/<case>/qemu-<arch>.toml` 发现。每个
`qemu-<arch>.toml` 保存运行配置，并可声明：

- `build_config`
- `vmconfigs`
- `test_commands`
- `shell_init_cmd`
- `success_regex` / `fail_regex`

**命令行参数：**

```text
cargo xtask axvisor test qemu --arch <arch>
```

| 参数 | 说明 |
|------|------|
| `--arch` | 目标架构（如 `aarch64`、`x86_64`、`riscv64`、`loongarch64`） |
| `--target` | 目标 target triple |
| `--test-group` / `-g` | 指定测试组名，默认 `normal` |
| `-c, --test-case` | 只运行指定 case |

### 3.1 执行链路

`cargo xtask axvisor test qemu --target <arch>` 的实现主流程为：

1. 解析目标架构或 target triple
2. 从 `test-suit/axvisor/<group>` 发现当前架构的 QEMU case
3. 汇总 case 的 `build_config` 与 `vmconfigs`
   - 如果多个 case 显式声明了不同的 `build_config`，会报错；这是 build-once 的约束
   - `vmconfigs` 会去重后合并到一次构建请求中
4. 调用 `rootfs::ensure_qemu_rootfs_ready(...)` 准备本轮共享 rootfs
5. 调用 `AppContext::build(...)` 构建一次 Axvisor
6. 对每个 case 读取对应 `qemu-<arch>.toml`
7. 调用共享 `test::case::prepare_case_assets(...)` 准备 case 资产
8. 将 case QEMU 配置中的 rootfs drive 改写为本轮选定的 rootfs 路径
9. 运行 QEMU，最后统一汇总失败 case

### 3.2 Rootfs 选择

Axvisor QEMU 测试的 rootfs 选择逻辑位于 `scripts/axbuild/src/axvisor/rootfs.rs`，顺序为：

1. 如果普通 `axvisor qemu` 命令传入显式 `--rootfs`，优先使用该路径；`test qemu` 当前不暴露该参数。
2. 如果 VM config 的 `kernel.kernel_path` 同目录存在 `rootfs.img`，使用该 VM config 推导出的 rootfs。
3. 否则使用 workspace managed rootfs：`target/rootfs/rootfs-{arch}-alpine.img`。

对于 managed rootfs，xtask 会通过 `scripts/axbuild/src/rootfs/store.rs` 按需下载 `rcore-os/tgosimages` release 中的归档并解压。case 运行时只改写 QEMU `disk0` drive 路径，不会为每个 case 复制 rootfs。

### 3.3 Case 资产

Axvisor 和 StarryOS 共用 `scripts/axbuild/src/test/case.rs` 的 case 资产流水线。当前可识别：

| 资产类型 | 发现方式 | 处理方式 |
|----------|----------|----------|
| C | case 目录下存在 `c/` | CMake 交叉编译并通过 overlay 注入 rootfs |
| Shell | case 目录下存在 `sh/` | 复制脚本到 `/usr/bin/` 并注入 rootfs |
| Python | case 目录下存在 `python/` | 在 staging rootfs 安装 python3，复制 `.py` 到 `/usr/bin/` 并注入 rootfs |
| grouped | `qemu-{arch}.toml` 中存在 `test_commands` | 生成 `/usr/bin/starry-run-case-tests` runner，按命令顺序执行 |

当前 Axvisor 预置的 `smoke` case 不需要额外 case 资产，但执行路径已经与 StarryOS 共用这套准备逻辑。`shell_init_cmd` 和 `test_commands` 不能同时出现在同一个 QEMU 配置中；加载配置时会检查并报错。

## 4. U-Boot

U-Boot 测试通过硬编码的板型/客户机映射表定义：

| 板型 | 客户机 | 构建配置 | VM 配置 |
|------|--------|----------|---------|
| `orangepi-5-plus` | `linux` | `os/axvisor/configs/board/orangepi-5-plus.toml` | `os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml` |
| `phytiumpi` | `linux` | `os/axvisor/configs/board/phytiumpi.toml` | `os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml` |
| `roc-rk3568-pc` | `linux` | `os/axvisor/configs/board/roc-rk3568-pc.toml` | `os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml` |

**命令行参数：**

```text
cargo xtask axvisor test uboot --board <board> --guest <guest>
```

| 参数 | 说明 |
|------|------|
| `--board` / `-b` | 板型名称 |
| `--guest` | 客户机类型 |
| `--uboot-config` | 自定义 U-Boot 配置文件路径 |

### 4.1 执行链路

`cargo xtask axvisor test uboot --board <board> --guest <guest>` 的实现主流程为：

1. 在 `scripts/axbuild/src/axvisor/test.rs` 中按 `(board, guest)` 查找硬编码映射
2. 得到对应的 build config 与 VM config
3. 若用户传入 `--uboot-config`，优先使用显式配置；否则走默认 U-Boot 配置搜索
4. 组装 build request，并交给 `AppContext::uboot(...)`

当前 Axvisor 的 U-Boot 测试不采用目录扫描发现，而是**只支持硬编码白名单中的板型/guest 组合**。

## 5. Board

板级测试从 `test-suit/axvisor/normal/<case>/board-*.toml` 发现。每个
`board-*.toml` 是自包含测试用例，既包含 axbuild 需要的构建配置和 VM 配置，也包含
ostool 需要的板级运行配置。

预置 case 包括：

| case | 板测配置 |
|--------|----------|
| `smoke` | `test-suit/axvisor/normal/smoke/board-*.toml` |

执行日志会按 `<case>/<board>` 展示单个板测项，例如
`smoke/roc-rk3568-pc-linux`。

**命令行参数：**

```text
cargo xtask axvisor test board [--board <board>] [--test-group <group>] [--test-case <case>] [--board-type <type>] [--server <addr>] [--port <port>]
```

| 参数 | 说明 |
|------|------|
| `--board` | 指定开发板名，运行该开发板下所有匹配的 board 测例 |
| `--test-group` / `-g` | 指定测试组名，默认 `normal` |
| `--test-case` / `-c` | 指定 case 目录名（如 `smoke`），作为额外 case 过滤 |
| `--board-type` / `-b` | 指定板型 |
| `--server` | 串口服务器地址 |
| `--port` | 串口服务器端口 |

### 5.1 执行链路

`cargo xtask axvisor test board ...` 的实现主流程为：

1. 扫描 `test-suit/axvisor/<group>/*/board-*.toml`，按 `--board` 与 `--test-case` 过滤后展开一个或多个 board test group
2. 对每个 group：
   - 准备对应 VM config
   - 组装 build request
   - 读取该 `board-*.toml` 作为 board run config
   - 调用 `AppContext::board(...)`
3. 汇总失败组并统一报错

Axvisor board test 由**构建配置、VM 配置和板测配置三者共同驱动**，并非单独的板级串口运行步骤。

与 QEMU suite 不同，board 测试按 board test group 逐项准备和构建；每个 `board-*.toml` 都是自包含的板测配置，并显式声明对应的 `build_config` 与 `vmconfigs`。

## 6. 限制

- `test qemu` 和 `test board` 已经从 `test-suit/axvisor/<group>` 发现用例，当前仓库预置 `normal` 组。
- `test qemu` 的一次运行要求所有显式 `build_config` 一致，以保证 Axvisor 本体只构建一次；需要不同 build config 的 case 应拆成不同命令运行。
- `test qemu` 共享本轮 rootfs 路径，不会为每个 case 复制 rootfs。
- `test uboot` 仅支持硬编码白名单中的 `(board, guest)` 组合。

## 7. 新增用例

新增 Axvisor board 测试用例需要：

1. 在 `os/axvisor/configs/board/` 下准备构建配置
2. 在 `os/axvisor/configs/vms/` 下准备 VM 配置
3. 在 `test-suit/axvisor/normal/<case>/board-<name>.toml` 中声明
   `build_config`、`vmconfigs` 和板级运行配置

新增 Axvisor QEMU 测试用例需要：

1. 在 `test-suit/axvisor/<group>/<case>/` 下创建 case 目录
2. 为目标架构创建 `qemu-{arch}.toml`
3. 如需指定构建配置，声明 `build_config`
4. 如需 guest，声明一个或多个 `vmconfigs`
5. 通过 `shell_init_cmd` 或 `test_commands` 定义 guest 内测试入口，并配置 `success_regex` / `fail_regex`
