---
sidebar_position: 1
sidebar_label: "概述"
---

# ArceOS

ArceOS 的构建单元是 workspace 中的一个 app package，例如 `arceos-helloworld` 或 `arceos-httpserver`。这与构建固定 `starryos` package 的 StarryOS，以及构建 `axvisor` 并加载 VM 描述的 Axvisor 不同。一次请求必须最终解析出 package；它可以来自 `--package`、Build Config 或 Snapshot。

## 1. 命令边界

ArceOS 的命令围绕 app 选择和运行目标展开，所有命令最终共享同一份解析后的 package、target 与 Build Config。下表说明每个入口消耗的主要契约。

| 命令 | 职责 |
| --- | --- |
| `build` | 构建 Rust app 或 `app-c` C 应用 |
| `qemu` | 构建并以 QEMU TOML 启动 |
| `uboot` | 构建并通过显式或自动发现的 U-Boot 配置启动 |
| `board` | 构建并部署到远程板卡 |
| `test qemu` / `test board` | 运行 Rust/C QEMU 测试或板级测试 |
| `defconfig <board>` | 将 checked-in board build config 写为默认配置并更新 Snapshot |
| `config ls` | 列出 `os/arceos/configs/board/` 下的 board 名称 |

共享参数为 `--config`、`--package`、`--arch`、`--target`、`--smp`、`--debug`；QEMU 额外接受 `--qemu-config`、`--rootfs`，U-Boot 接受 `--uboot-config`，板卡运行接受 `--board-config`、`--board-type`、`--server`、`--port`。

## 2. 应用选择

常规 `build`、`uboot`、`board` 需要通过 CLI、配置或 Snapshot 选定 package。ArceOS 有一个仅适用于 `qemu` 的便利规则：当调用方既没有 package，也没有 config 时，`ArceOS::qemu()` 会寻找 target 对应的 `os/arceos/configs/board/qemu-<arch>.toml`。该文件目前选择 `arceos-helloworld`，因此可直接执行：

```bash
cargo xtask arceos qemu
```

一旦传入 `--package` 或 `--config`，该默认值不参与解析；显式选择永远优先。

## 3. 配置布局

checked-in board 配置负责默认能力选择，QEMU 配置负责启动细节；两者不应混合维护。目录布局与 `arceos/config.rs` 和 `arceos/mod.rs` 中的默认路径一致。

```text
os/arceos/configs/
├── board/                 # package + target + BuildInfo；供 defconfig 和缺失配置初始化
│   └── qemu-aarch64.toml
└── qemu/                  # QEMU 启动契约
    └── qemu-aarch64.toml
```

`defconfig` 将指定 board 文件复制到：

```text
tmp/axbuild/config/<package>/build-<target>.toml
```

并把 package、arch、target、smp 和 config 写入 `tmp/axbuild/.arceos.toml`，同时清空旧 QEMU/U-Boot 路径。对于 `build` 和 `qemu`，若这个目标配置不存在，axbuild 会优先用同 target、同 package 的 `qemu-*` board 文件补齐它；隐式创建不会改变 Snapshot。

## 4. 构建路径

### 4.1 Rust 应用

普通 app 走 `arceos/build/cargo_config.rs`，使用共享的 `BuildInfo::into_prepared_base_cargo_config_with_metadata()`。它以 `ax-std` 的 Cargo metadata 为准拆分 feature，使用 musl PIE JSON target 编译 std，并默认保留 ELF。

### 4.2 C 应用

Build Config 中的 `app-c` 选择 C 应用路径。该字段相对路径按 Build Config 所在目录解析，目标目录中必须直接包含 `.c` 源文件。`prepare_arceos_request()` 将该请求解析为 `ax-libc` package，并验证 CLI、配置中的 package 选择与 C app 路径一致。

C app 使用 `arceos/cbuild/` 中的 CMake/musl 工具链构建 ELF。`resolve_c_app_features()` 在 `max_cpu_num > 1` 时加入 `ax-std/smp`，其余能力直接取自 Build Config 的 `features`。

## 5. 能力约束

ArceOS 的 Rust app 使用共享 std-aware 构建路径和动态平台链接配置。`BuildInfo::validate_features()` 与 `reject_removed_std_field()` 对 feature 和 TOML 根字段执行验证；具体字段与转发规则见 [参数与配置](../configuration)。

## 6. 命令示例

以下命令分别覆盖显式 app 选择、默认 QEMU、board-derived 配置和 C app 配置，便于验证请求解析是否符合预期。

```bash
# 显式构建 app
cargo xtask arceos build --package arceos-helloworld --arch aarch64

# 运行默认 QEMU app，或运行指定 app
cargo xtask arceos qemu
cargo xtask arceos qemu --package arceos-httpserver --smp 4

# 选择 checked-in board 配置
cargo xtask arceos config ls
cargo xtask arceos defconfig qemu-riscv64
cargo xtask arceos build

# C app 配置
cargo xtask arceos build --config path/to/build-c-app.toml
```
