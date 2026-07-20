---
sidebar_position: 6
sidebar_label: "Backtrace Host 符号化"
---

# Backtrace Host 符号化

本文档说明 Issue [#146](https://github.com/rcore-os/tgoskits/issues/146) 中 **host 端** backtrace 日志符号化工具的用途、前置条件、命令参数与典型工作流。

它与 target 侧（QEMU / 板载）输出的 **raw backtrace 块** 配合使用，由 PR [#635](https://github.com/rcore-os/tgoskits/pull/635) 提供实现；target 侧 raw 格式与 E2E 测例由 PR [#646](https://github.com/rcore-os/tgoskits/pull/646) 等提供。

## 设计分工

| 阶段 | 运行环境 | 职责 |
|------|----------|------|
| **Unwind + raw 日志** | Target（ArceOS / 应用） | 沿帧指针链展开栈，串口输出固定格式块（`BACKTRACE_BEGIN` / `BT` / `BACKTRACE_END`），仅包含 `ip`、`fp` 等原始信息 |
| **Symbolize** | Host（开发机 / 容器） | 读取日志与**同一次构建**的 ELF，用 `llvm-addr2line` / `addr2line` 解析函数名与源文件行号 |

这样做的原因包括：

- Target 在 panic / oops 路径上不宜依赖完整在线 DWARF 解析（体积、稳定性）。
- Raw 块便于 CI 用正则校验 unwind 是否成功。
- Host 可使用完整 debuginfo 与成熟的 addr2line 工具链。

本工具 **不使用** `components/axbacktrace/src/dwarf.rs` 的 target 在线符号化路径。

## 前置条件

1. **Host 工具**：`PATH` 中可执行 `llvm-addr2line` 或 `addr2line`。
2. **ELF**：与产生日志的那次构建一致（例如 `target/x86_64-unknown-none/release/arceos-test-suit`），且含 debug 信息（测例通常通过 `[env] DWARF=y` 等打开帧指针与调试构建）。
3. **日志**：串口或 QEMU 输出中已包含 target 打印的 raw 块（例如 `Backtrace::capture().kind("raw")`、panic 路径的 `.kind("panic")`、trap 的 `.kind("trap")`）。

## 命令

在仓库根目录执行：

```bash
cargo xtask backtrace symbolize \
  --elf <ELF> \
  [--log <LOG>] \
  [--kind <KIND>] \
  [--adjust-ip true|false] \
  [--ip-bias <I64>]
```

### 参数说明

| 参数 | 必填 | 说明 |
|------|------|------|
| `--elf PATH` | 是 | 用于符号化的内核/应用 ELF 路径 |
| `--log PATH` | 否 | 捕获的文本日志；省略时从 **stdin** 读取 |
| `--kind KIND` | 否 | 仅处理 `BACKTRACE_BEGIN` 中 `kind=` 匹配的块（如 `raw`、`panic`、`trap`） |
| `--adjust-ip` | 否 | 默认 `true`：符号化前对 `ip` 减 1，对齐典型 call-site；可用 `--adjust-ip false` 关闭 |
| `--ip-bias I64` | 否 | 符号化前对 `ip` 施加有符号偏移，用于运行时地址与 ELF 布局不一致时（默认 `0`） |

实现位于 `scripts/axbuild/src/backtrace.rs`，由 `cargo xtask backtrace` 子命令调用。

## Target 日志格式（输入）

工具解析如下块（与 `axbacktrace` 在设置 `kind` 后的 [`Display`](https://doc.rust-lang.org/std/fmt/trait.Display.html) 输出一致）：

```text
BACKTRACE_BEGIN kind=raw arch=x86_64 alloc=true dwarf=true
BT 0 ip=0x... fp=0x...
BT 1 ip=0x... fp=0x...
BT_ERROR invalid_fp          # 可选，展开失败时
BACKTRACE_END
```

- 日志前可有任意前缀噪声；解析器从第一个 `BACKTRACE_BEGIN` 起进入块内状态。
- 缺少 `BACKTRACE_END` 时仍会尝试符号化已捕获的帧。
- 重复的 `BACKTRACE_BEGIN` 会拆成多个逻辑块输出。

## 输出说明

符号化成功后，stdout 大致为：

```text
BACKTRACE_BLOCK 0 kind=raw arch=x86_64
BT 0 ip=0x... fp=0x... <function> (<file>:<line>)
...
```

若某 `ip` 无法在 ELF 中解析，可能仍只显示 `ip` / `fp`，或解析失败；请确认 ELF 与日志来自同一次构建，必要时尝试 `--ip-bias`。

## 典型工作流（当前：两步）

在 PR **#635**（本工具）与 **#646**（raw 测例）均合入 `dev` 之前，请使用对应分支构建。

### 1. 在 target 上产生 raw 日志

示例：ArceOS Rust QEMU 测例 `debug-backtrace`：

```bash
cargo xtask arceos test qemu \
  --arch x86_64 \
  --test-group rust \
  --test-case debug-backtrace \
  2>&1 | tee /tmp/arceos-backtrace.log
```

确认日志中有 `BACKTRACE_BEGIN`、`BT n ip=... fp=...`、`ARCEOS_TEST_END feature=debug-backtrace ... status=pass` 等。

### 2. 在 host 上符号化

```bash
cargo xtask backtrace symbolize \
  --elf target/x86_64-unknown-none/release/arceos-test-suit \
  --log /tmp/arceos-backtrace.log \
  --kind arceos-test-suit
```

Panic 路径若使用 `kind=panic` 的 raw 块，将 `--kind` 改为 `panic`，并确保 `--elf` 与产生该日志的构建一致。会故意 panic 或 trap 的 raw 专项用例不再放在 ArceOS Rust 全测入口中；当前全测入口只保留能返回并继续执行后续用例的 backtrace smoke。

## ArceOS QEMU 测试：跑完自动 symbolize

在 **Unix** 主机上，`cargo xtask arceos test qemu` 运行 **rust** 用例时，默认仅当该用例的 **`build-<target>.toml` 启用了 backtrace 构建**（`[env]` 中 `BACKTRACE=y` 或 `DWARF=y`，与 axbuild 为帧指针 / 调试信息开门条件一致）时，在 QEMU 结束后：

1. 将本次串口输出 tee 到 `.axbuild/tmp/qemu-logs/<case>-<target>.log`；
2. 用同次构建的 ELF 调用 host `backtrace symbolize`，在终端打印 `=== host backtrace symbolize ===` 段。

当前统一 Rust suite 的四个 `build-<target>.toml` 均启用了 `BACKTRACE=y`
和 `DWARF=y`，因此 Rust QEMU 用例默认都会 tee 串口日志并尝试自动
symbolize。backtrace smoke（如 `debug-backtrace`）通常 **一条命令** 即可看到符号化栈。

关闭自动符号化：

```bash
cargo xtask arceos test qemu --arch x86_64 --test-group rust --test-case debug-backtrace --no-symbolize
```

仍可用上文「手动两步」对任意保存的日志做 symbolize。

## 验证

在 host 上可运行：

```bash
cargo fmt --check
cargo clippy -p axbuild -- -D warnings
cargo test -p axbuild backtrace::
```

## 相关文档与 PR

- Target 回溯组件：[`axbacktrace`](../components/crates/axbacktrace.md)
- Panic 路径与 backtrace 门控：[Panic 递归保护](./panic-recursion-guards.md)
- Tracking issue：[#146](https://github.com/rcore-os/tgoskits/issues/146)
- Host symbolize PR：[#635](https://github.com/rcore-os/tgoskits/pull/635)
- ArceOS raw E2E PR：[#646](https://github.com/rcore-os/tgoskits/pull/646)
