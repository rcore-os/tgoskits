---
sidebar_position: 4
sidebar_label: "内核测试"
---

# 内核测试（ktest）

`cargo xtask ktest` 为 StarryOS 和 Axvisor 中声明为 `harness = false` 的 `[[test]]` target 提供统一的 QEMU 和板卡执行路径。它不是 host 端的 `cargo test`：测试目标会作为内核镜像构建并在目标运行环境输出 axtest 标记。

## 1. 运行接口

`qemu` 和 `board` 共享 package、test target 与 Build Config 选择，但分别在虚拟机和远程板卡上完成最终验证。两种模式都只接收明确的 `harness = false` target，避免误把 host 测试当作内核测试执行。

### 1.1 QEMU 参数

QEMU 模式以 arch 或 target 选择运行时配置；未给出 `--test` 时，package 中必须只有一个符合约束的目标。`--coverage` 会启用 axtest 覆盖率捕获，不改变测试 target 的 feature 选择。

```text
cargo xtask ktest qemu -p <PACKAGE> [--test <TARGET>] [--arch <ARCH> | -t <TRIPLE>]
                       [--config <BUILD_TOML>] [--qemu-config <QEMU_TOML>] [--coverage]
```

### 1.2 板卡参数

板卡模式必须同时给出 test target 与 board 名称，因为 build/run TOML 都由 board 名称派生。服务器地址和板型作为 `RunBoardOptions` 传给 ostool 的运行阶段，Build Config 保持由 `--config` 或 board 默认路径选择。

```text
cargo xtask ktest board -p <PACKAGE> --test <TARGET> -b <BOARD>
                        [--config <BUILD_TOML>] [--board-config <BOARD_TOML>]
                        [--board-type <TYPE>] [--server <HOST>] [--port <PORT>]
```

`--package` 必须是 workspace package。运行时由 package 所在位置决定：`axvisor` 使用 Axvisor 流程，`os/StarryOS/` 下的 package 使用 StarryOS 流程；其他 package 当前会被拒绝。

## 2. 测试目标

axbuild 从 `cargo metadata` 和 package `Cargo.toml` 读取 target。可选 target 必须是 `[[test]]` 且 `harness = false`：

- 指定 `--test` 时，名称必须存在并满足上述条件；
- 不指定时，package 必须恰好有一个符合条件的 target；
- 没有或存在多个候选 target 时会给出错误，要求补充 `--test`。

## 3. 构建装配

`prepare_ktest_cargo()` 在运行时 Cargo 配置上进行最小、显式的改写：

- 清除普通 binary/test selector，选择当前 `--test` target；
- 追加 `axtest` 和该 target 的 `required-features`；
- 追加 `--cfg axtest --check-cfg cfg(axtest)`；
- `--coverage` 时设置 `AXTEST_COVERAGE=y` 并配置覆盖率捕获。

平台、虚拟化和应用 feature 由所选 Build Config 与 test target 的 `required-features` 共同确定。StarryOS target 构建完成后执行 `postprocess_starry_artifact()`，生成 kallsyms 并按 ITS 配置处理启动镜像。

## 4. QEMU 验证

默认配置路径来自运行时：

| 运行时 | Build Config | QEMU Config |
| --- | --- | --- |
| StarryOS | `os/StarryOS/configs/board/qemu-<arch>.toml` | `os/StarryOS/configs/qemu/qemu-<arch>.toml` |
| Axvisor | `os/axvisor/configs/board/qemu-<arch>.toml` | `os/axvisor/configs/qemu/qemu-<arch>.toml` |

StarryOS 会准备其 managed rootfs；Axvisor 会准备当前 arch 的 managed rootfs。随后 QEMU drive 被替换为该镜像，并追加以下判定标记：

- 成功：`AXTEST_SUITE_OK`
- 失败：`panicked at`、`AXTEST_SUITE_FAIL`、`AXTEST_CASE .* status=fail`

因此 test target 应在完成时输出相应 axtest 成功标记。

## 5. 板卡验证

板卡模式要求 `--test` 和 `--board`。默认 Build Config 是 `configs/board/<board>.toml`；StarryOS 优先采用存在的 `configs/board/<board>-board.toml` 作为 run config，Axvisor 采用 `<board>.toml`。完成构建后，ktest 使用当前 Cargo 的 `to_bin` 调用 `board_prepared_elf()`。

## 6. 命令示例

这些示例覆盖 runtime 选择、显式配置/覆盖率和远程板卡三类常用验证方式。

```bash
# QEMU，target 由 --arch 解析
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --arch x86_64

# 显式 TOML 与覆盖率捕获
cargo xtask ktest qemu -p axvisor --test axtest_kernel \
  --target aarch64-unknown-none-softfloat --config path/to/build.toml \
  --qemu-config path/to/qemu.toml --coverage

# 板卡
cargo xtask ktest board -p starry-kernel --test axtest_kernel -b orangepi-5-plus
```
