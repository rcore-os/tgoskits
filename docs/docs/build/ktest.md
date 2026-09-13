---
sidebar_position: 4
sidebar_label: "内核测试"
---

# 内核测试（ktest）

`cargo xtask ktest` 为 workspace 中声明为 `harness = false` 的 Cargo `[[test]]` target 提供统一的 QEMU 和板卡执行路径。它不是 host 端的 `cargo test`：测试目标会作为 ArceOS、StarryOS 或 Axvisor 内核镜像构建，并在目标运行环境输出 axtest 标记。只有依赖目标架构、内核运行时、真实设备或板卡的测例才使用 axtest；可在宿主确定性运行的算法、状态机、格式化和参数校验测试统一使用普通 `#[test]`。完整契约见仓库文档 `docs/design/axtest-cargo-integration.md`。

axtest target 由拥有完整运行时的上层 consumer（例如 `starry-kernel`、Axvisor）或专用板卡
测试包持有。`ax-runtime`、`ax-hal`、`ax-task`、`axbacktrace`、`ax-fs-ng` 等启动依赖库不
单独创建 target：其私有纯逻辑使用 std 单元测试，需要真实 ArceOS 的行为放入
`test-suit/arceos/rust`，并且只调用生产公开 API。

## 1. 运行接口

`qemu` 从 Cargo metadata 建立 workspace 执行计划；`board` 显式选择一个 package 和 test target。两种模式都只接收明确的 `harness = false` target，避免误把 host 测试当作内核测试执行。

### 1.1 QEMU 参数

无参数 QEMU 命令等价于 workspace 全量选择。`-p/--package`、`--exclude`、`--test` 均可重复；未给出 `--test` 时会展开所有符合约束的 target。`--arch` 只保留支持该架构的执行单元，`--coverage` 会启用 axtest 覆盖率捕获。

```text
cargo xtask ktest qemu [--workspace | -p <PACKAGE>...] [--exclude <PACKAGE>...]
                       [--test <TARGET>...] [--arch <ARCH> | -t <TRIPLE>]
                       [--features <FEATURES>] [--all-features | --no-default-features]
                       [--profile <PROFILE>] [--target-dir <DIR>]
                       [--locked] [--offline] [--frozen]
                       [--config <BUILD_TOML>] [--qemu-config <QEMU_TOML>]
                       [--coverage [--out-fmt html]] [--no-fail-fast]
```

### 1.2 板卡参数

板卡模式必须同时给出 test target 与 board 名称，因为 build/run TOML 都由 board 名称派生。服务器地址和板型作为 `RunBoardOptions` 传给 ostool 的运行阶段，Build Config 保持由 `--config` 或 board 默认路径选择。

```text
cargo xtask ktest board -p <PACKAGE> --test <TARGET> -b <BOARD>
                        [--config <BUILD_TOML>] [--board-config <BOARD_TOML>]
                        [--board-type <TYPE>] [--server <HOST>] [--port <PORT>]
```

`--package` 必须是 workspace package。`--config` 和 `--qemu-config` 只允许用于唯一执行单元。目标使用自定义 harness，因此不支持 Cargo `TESTNAME` 或 `--` 后的 libtest 参数。

## 2. 测试目标

创建 target 前必须先确认标准 Rust harness 或项目已有的 host-test adapter 无法表达被测
语义。同一 crate 的纯逻辑部分应留在 unit/integration `#[test]` 中，仅把实际需要 QEMU 或
板卡的部分放入以下 target。源码单元测试放在实现文件末尾；Cargo `tests/` 下的集成测试
只能验证公开 API，不得为它们公开内部表示或增加测试状态注入接口。

axbuild 从 `cargo metadata` 和 package `Cargo.toml` 读取 target。package 必须在
`[dev-dependencies]` 中通过相对 path 直接依赖 workspace `axtest`，可选 target 必须是
`[[test]]` 且 `harness = false`。仓库内 dev-dependency 不得声明 `version`，也不得通过
`workspace = true` 继承版本；path-only 依赖会在发布时被 Cargo 剥离，避免测试依赖环进入
registry 发布图。外部 registry dev-dependency 可以继续声明版本。

发现规则如下：

- workspace 选择对没有直接依赖的 package 静默跳过，显式 `-p` 选择则报错；
- 声明了依赖却没有符合条件 target 的 package 被视为 manifest 错误；
- 指定 `--test` 时只保留同名 target，不指定时展开 package 的全部符合条件 target；
- 计划按 package、test target、target triple 稳定排序，一个单元对应一次构建和一次 QEMU。

默认遇到首个失败立即停止；`--no-fail-fast` 会运行剩余单元并最终返回聚合失败。

## 3. 架构与运行时元数据

QEMU 架构由 `[package.metadata.docs.rs].targets` 声明。支持以下映射：

| Arch | Bare-metal target |
| --- | --- |
| `x86_64` | `x86_64-unknown-none` |
| `aarch64` | `aarch64-unknown-none-softfloat` |
| `riscv64` | `riscv64gc-unknown-none-elf` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` |

未声明 targets 的通用 crate 只运行 x86_64；声明了 targets 却没有可识别 bare-metal target 时会报错。workspace 使用 `--arch` 时，不支持该架构的 package 被过滤；显式 `-p` 选择不支持者会报错。

运行时通过最小 metadata 声明，省略时默认 ArceOS：

```toml
[package.metadata.axtest]
runtime = "arceos" # starry | axvisor | board
```

`board` runtime 不进入 workspace QEMU 计划，只能用 `ktest board`。StarryOS/Axvisor 保留各自 rootfs、镜像和后处理流程。

## 4. 构建装配

`prepare_ktest_cargo()` 在运行时 Cargo 配置上进行最小、显式的改写：

- 清除普通 binary/test selector，选择当前 `--test` target；
- 追加 `axtest` 和该 target 的 `required-features`；
- 追加 `--cfg axtest --check-cfg cfg(axtest)`；
- `--coverage` 时设置 `AXTEST_COVERAGE=y` 并配置覆盖率捕获。

平台、虚拟化和应用 feature 由所选 Build Config 与 test target 的 `required-features` 共同确定。runner 不会按 arch 改写 `uefi`、`to_bin`、CPU、设备或 rootfs 契约。StarryOS target 构建完成后执行 `postprocess_starry_artifact()`，生成 kallsyms 并按 ITS 配置处理启动镜像。

## 5. QEMU 验证

默认配置路径来自运行时：

| 运行时 | Build Config | QEMU Config |
| --- | --- | --- |
| ArceOS | package `build-<target>.toml`，否则 `os/arceos/configs/board/qemu-<arch>.toml` | package `qemu-<arch>.toml`，否则 `os/arceos/configs/qemu/qemu-<arch>.toml` |
| StarryOS | `os/StarryOS/configs/board/qemu-<arch>.toml` | `os/StarryOS/configs/qemu/qemu-<arch>.toml` |
| Axvisor | `os/axvisor/configs/board/qemu-<arch>.toml` | `os/axvisor/configs/qemu/qemu-<arch>.toml` |

StarryOS 会准备其 managed rootfs；Axvisor 会准备当前 arch 的 managed rootfs。随后 QEMU drive 被替换为该镜像，并追加以下判定标记：

- 成功：`AXTEST_SUITE_OK`
- 失败：`panicked at`、`AXTEST_SUITE_FAIL`、`AXTEST_CASE .* status=fail`

因此 test target 应在完成时输出相应 axtest 成功标记。Starry kernel 的 target 只负责
启动共享内部 root 中的源码内 `#[axtest]` case；它不提供集中测试 façade。

覆盖率产物按 `<package>-<test>-<target>` 隔离；`--out-fmt html` 会为每个执行单元生成并打印独立报告路径。覆盖率运行还必须到达 `AXTEST_COVERAGE_DONE`。

## 6. 板卡验证

板卡模式要求 `--test` 和 `--board`。Build Config 优先使用 package 内的 `build-<board>.toml`；若不存在且 package 内恰好只有一个 `build-*.toml`，则使用该文件，否则回退到运行时的 `configs/board/<board>.toml`。Run Config 优先使用 package 内的 `board-<board>.toml`；否则 StarryOS 优先采用存在的 `configs/board/<board>-board.toml`，其他情况使用 `configs/board/<board>.toml`。完成构建后，ktest 使用当前 Cargo 的 `to_bin` 调用 `board_prepared_elf()`。

## 7. 命令示例

这些示例覆盖 runtime 选择、显式配置/覆盖率和远程板卡三类常用验证方式。

```bash
# workspace 全量（按各 package metadata 展开架构）
cargo xtask ktest qemu

# 只运行支持 x86_64 的执行单元
cargo xtask ktest qemu --workspace --arch x86_64

# 显式 package/target
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64

# 显式 TOML 与覆盖率捕获
cargo xtask ktest qemu -p axvisor --test axtest \
  --target aarch64-unknown-none-softfloat --config path/to/build.toml \
  --qemu-config path/to/qemu.toml --coverage

# SG2002 板卡专用测试
cargo xtask ktest board -p arceos-axtest-sg2002-usb-msc \
  --test axtest -b aka-00-sg2002
```
