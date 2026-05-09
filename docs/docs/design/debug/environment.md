---
sidebar_position: 2
sidebar_label: "准备环境"
---

# 调试环境准备

本文档说明当前调试方案依赖哪些宿主机条件，以及这些依赖在实现里分别承担什么角色。

## VS Code 扩展

建议至少安装以下扩展：

- `vadimcn.vscode-lldb`
- `rust-lang.rust-analyzer`

其中：

- `CodeLLDB` 负责对接 `launch.json` 中的 `lldb` 类型配置
- `rust-analyzer` 不直接参与调试链路，但负责代码导航、符号感知和断点上下文体验

如果缺少 `CodeLLDB`，即使 QEMU 和 GDB stub 正常启动，VS Code 也无法附加调试器。

## Rust 目标

当前预置调试配置默认围绕 AArch64 QEMU 路径，因此至少需要：

```bash
rustup target add aarch64-unknown-none-softfloat
```

如果你同时开发其他架构，建议一并安装：

```bash
rustup target add riscv64gc-unknown-none-elf
rustup target add x86_64-unknown-none
rustup target add loongarch64-unknown-none-softfloat
```

这些 target 不是“为了能编译就多装几个”，而是和当前调试配置中 `target create ${workspaceFolder}/target/.../debug/...` 的产物路径直接对应。也就是说，调试链路默认假设对应 target 的 debug 二进制能够被正常构建出来。

## 宿主机依赖

进入调试前，宿主机至少需要具备：

- 可用的 Rust 工具链
- 对应架构的 QEMU system 模拟器
- 可被 VS Code 调用的 `python`

这些依赖在实现中的角色分别是：

- QEMU：提供被调试目标与 GDB stub
- Python：执行 `.vscode/session.py`
- Rust 工具链：生成 `launch.json` 中引用的 debug 二进制

因此 `python` 不是附属脚本依赖，而是当前调试链路的一部分。

### 可选环境变量

除上述必需依赖外，`session.py` 还支持以下可选环境变量（当前 `tasks.json` 中均未显式设置，使用默认值）：

| 环境变量 | 默认值 | 说明 |
|---------|--------|------|
| `TGOS_DEBUG_TEE_OUTPUT` | `"1"` | 设为 `"1"` 时将 QEMU 输出同时镜像到 VS Code 终端；设为 `"0"` 时仅写入日志文件。预留用于 CI/自动化静默模式 |

## 系统运行资源

### StarryOS

StarryOS 首次运行可能需要准备 rootfs：

```bash
cargo xtask starry rootfs --arch aarch64
```

### Axvisor

Axvisor 除了构建本体，还通常依赖 Guest 镜像、VM 配置和 rootfs。当前这些运行资源已经由 `scripts/axbuild` 路径统一接管，因此不再要求开发者在调试前手工执行 `setup_qemu.sh` 一类准备脚本。

这里的核心设计点是：调试链路虽然仍然依赖这些运行时资源，但它们的准备逻辑已经被并入 `cargo xtask axvisor ...` 的实现路径，而不是要求开发者额外维护一套独立的预处理步骤。

## 失败检查顺序

如果第一次按 `F5` 就失败，优先检查：

1. 对应 target 是否已安装
2. `python` 是否可用
3. QEMU 是否在 `PATH` 中
4. StarryOS / Axvisor 的运行资源准备逻辑是否在当前 `xtask` 路径中执行成功
5. `target/qemu-debug/*.log` 中是否已经出现 `QEMU_DEBUG_FAILED`
