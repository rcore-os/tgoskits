---
sidebar_position: 7
sidebar_label: "内核模块"
---

# StarryOS 内核模块

`cargo xtask starry kmod build` 编译 StarryOS 可加载内核模块（`.ko`）。这是 StarryOS 独有的命令，[ArceOS](../arceos/overview) 和 [Axvisor](../axvisor/overview) 没有。

## 1. 命令接口

### 1.1 构建入口

`kmod build` 只处理可加载模块的发现、编译、部分链接和可选 rootfs 注入；内核本体仍由 Starry Build Config 决定。命令入口如下。

```text
cargo xtask starry kmod build [options]
```

### 1.2 参数接口

模块选择、目标配置与 rootfs 注入均通过命令参数明确表达，`--all` 和 `--module` 不能同时使用。完整调用形式如下。

```bash
cargo xtask starry kmod build [--arch <ARCH>] [--target <TARGET>] [--config <PATH>] [--smp <N>] [--debug] \
                              [-m/--module <PATH>... | --all] [--rootfs <IMAGE>]
```

参数表区分目标选择、模块选择和部署目标，便于将构建能力与 rootfs 注入意图分开维护。

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

## 2. 模块发现

`collect_modules(workspace_root, args)` 从两个来源发现模块：

1. `--module <PATH>` 显式指定的路径（可重复）。路径可以是 crate 目录（含 `Cargo.toml`）或直接指向 `Cargo.toml`，自动查找深度 ≤ 10。
2. `--all`（或默认）扫描 `os/StarryOS/lkm/` 目录下的所有模块 crate。

未找到任何模块时报错 `no module crates found`。

## 3. 构建路径

模块按语言和链接方式分为 Rust 与 C 两条路径；两者共享目标选择，但依赖的工具链和可支持的宿主条件不同。

### 3.1 Rust 构建

Rust 模块复用 StarryOS 内核构建配置的 Cargo 环境（target、features、Cargo target directory），确保模块与内核 ABI 一致。模块链接使用专用 linker script，并将 rlib 处理为内核可加载对象：

- **独立链接脚本**：使用 `os/StarryOS/scripts/kmod-linker.ld`（不存在时报错）
- **部分链接为 ET_REL**：模块的 rlib 被部分链接（partial-link）为可重定位的 ET_REL `.ko` 文件，而非完全链接的可执行文件

构建流程：派生内核的 Cargo 配置 → 切换 package 和输出处理 → `cargo build` 编译模块 rlib → 用 kmod-linker.ld 部分链接为 `.ko`。

### 3.2 C 构建

C 模块使用 Linux Kbuild Makefile 构建。axbuild **仅在所选架构与 host 架构相同时**才调用模块目录自带的 Makefile（host-only 限制）。这与 Rust 模块的全架构支持不同。

## 4. 根文件系统注入

`--rootfs <IMAGE>` 指定时，所有构建产物（`.ko` 文件）通过 `debugfs` 注入到镜像的 `/modules/` 目录下，使得 StarryOS 启动后可用 `insmod` 加载这些模块。

## 5. 命令示例

这些示例分别覆盖默认发现、指定模块、注入 rootfs 和切换架构四种常用路径。

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
