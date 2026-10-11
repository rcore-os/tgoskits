# Backtrace Showcase

这些示例验证 ArceOS 和 StarryOS 在 target 内完成回溯、函数名及源码行号解析。
构建流程从最终 ELF 生成 AXBT map，并把 map 放进 initramfs；QEMU 输出就是
最终诊断结果，不需要宿主机 ELF 或 `addr2line`。

| Demo | 内容 |
| --- | --- |
| 1 | ArceOS 基本帧指针回溯 |
| 2 | ArceOS map 函数名回溯 |
| 3 | ArceOS 函数名与 `file:line` 回溯 |
| 4 | StarryOS `/dev/memtrack` 分配回溯 |

四个 QEMU 目标都覆盖 `x86_64`、`aarch64`、`riscv64` 和 `loongarch64`：

```bash
bash apps/backtrace/run_demo.sh demo3 riscv64
bash apps/backtrace/run_demo.sh demo4 aarch64
bash apps/backtrace/run_demo.sh all-arch
```

## 在程序中启用

```toml
[dependencies]
ax-std = { workspace = true, features = ["backtrace"] }
axbacktrace = { workspace = true }

[features]
backtrace = ["ax-std/backtrace"]
```

```rust
use axbacktrace::Backtrace;

println!("{}", Backtrace::capture().kind("panic"));
```

`BACKTRACE=y` 保留帧指针，`DWARF=y` 在构建机生成函数、文件和行号 map。
map 安装完成后，输出包含 `symbol=<function> at <file>:<line>`；极早期启动
尚未解包 initramfs 时会安全降级为地址级帧。

## 运行

```bash
bash apps/backtrace/run_demo.sh demo1 x86_64
bash apps/backtrace/run_demo.sh demo3 x86_64
bash apps/backtrace/run_demo.sh demo4 x86_64
```

StarryOS 示例仍检查 `BACKTRACE_BEGIN`、函数名和 `file:line`，配置位于
`apps/starry/qemu/memtrack-backtrace`。可写客户机数据保持外部路径，启动
配置和只读镜像由统一 initramfs/bundle 迁移流程管理。
