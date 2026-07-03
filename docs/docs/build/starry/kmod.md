---
sidebar_position: 7
sidebar_label: "内核模块"
---

# StarryOS 内核模块

`cargo xtask starry kmod build` 编译 StarryOS 可加载内核模块（`.ko`）。这是 StarryOS 独有的命令，[ArceOS](../arceos/overview) 和 [Axvisor](../axvisor/overview) 没有。

## 子命令

```text
cargo xtask starry kmod build [options]
```

## 命令

```bash
cargo xtask starry kmod build [--arch <ARCH>] [--target <TARGET>] [--config <PATH>] [--smp <N>] [--debug] \
                              [-m/--module <PATH>... | --all] [--rootfs <IMAGE>]
```

| 参数 | 说明 |
|------|------|
| `--arch <ARCH>` | 目标架构（默认 `riscv64`） |
| `--target <TRIPLE>` | target triple |
| `--config <PATH>` | 显式 Build Info 路径 |
| `--smp <N>` | CPU 核数 |
| `--debug` | debug 构建 |
| `-m/--module <PATH>`（可重复） | 模块 crate 路径（或含模块的目录，自动查找深度 ≤ 10） |
| `--all` | 构建 `os/StarryOS/lkm/` 下所有模块（与 `--module` 互斥） |
| `--rootfs <IMAGE>` | 把构建产物注入到此 rootfs 镜像的 `/modules/` 目录 |

`--all` 与 `--module` 互斥；两者都未提供时默认扫描 `os/StarryOS/lkm/`。

## 模块发现

`collect_modules(workspace_root, args)` 从两个来源发现模块：

1. `--module <PATH>` 显式指定的路径（可重复）。路径可以是 crate 目录（含 `Cargo.toml`）或直接指向 `Cargo.toml`，自动查找深度 ≤ 10。
2. `--all`（或默认）扫描 `os/StarryOS/lkm/` 目录下的所有模块 crate。

未找到任何模块时报错 `no module crates found`。

## Rust 模块构建

Rust 模块复用 StarryOS 内核构建配置的 Cargo 环境（target、features、axconfig、Cargo target directory），确保模块与内核 ABI 一致。关键差异在于：

- **独立链接脚本**：使用 `os/StarryOS/scripts/kmod-linker.ld`（不存在时报错）
- **部分链接为 ET_REL**：模块的 rlib 被部分链接（partial-link）为可重定位的 ET_REL `.ko` 文件，而非完全链接的可执行文件

构建流程：派生内核的 Cargo 配置 → 切换 package 和输出处理 → `cargo build` 编译模块 rlib → 用 kmod-linker.ld 部分链接为 `.ko`。

## C 模块构建（Linux Kbuild）

C 模块使用 Linux Kbuild Makefile 构建。axbuild **仅在所选架构与 host 架构相同时**才调用模块目录自带的 Makefile（host-only 限制）。这与 Rust 模块的全架构支持不同。

## rootfs 注入

`--rootfs <IMAGE>` 指定时，所有构建产物（`.ko` 文件）通过 `debugfs` 注入到镜像的 `/modules/` 目录下，使得 StarryOS 启动后可用 `insmod` 加载这些模块。

## 用法示例

```bash
# 构建所有模块（默认 riscv64）
cargo starry kmod build

# 构建指定模块
cargo starry kmod build -m os/StarryOS/lkm/my-module

# 构建并注入 rootfs
cargo starry kmod build --all --rootfs alpine

# aarch64 模块
cargo starry kmod build --arch aarch64 --all
```
