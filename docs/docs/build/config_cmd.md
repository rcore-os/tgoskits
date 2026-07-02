---
sidebar_position: 10
sidebar_label: "Config 辅助命令"
---

# Config 辅助命令

`cargo xtask config` 是 axbuild 暴露的底层配置工具，围绕 ArceOS 的 **axconfig** 体系提供平台包定位、配置项读取、配置文件生成和 Makefile 适配字段检查四项能力。它本质上是把 `axbuild` 内部使用的配置引擎库（`ax_config_gen`）和平台包解析逻辑（`scripts/axbuild/src/build.rs`）直接暴露给用户、Makefile 和调试场景使用，避免重复实现一套平台配置查找逻辑。

> 本命令面向**手动调试和旧 Makefile 兼容**。日常 `cargo xtask <os> build` 流程会自动完成平台配置生成（见 [参数与配置 §axconfig](./configuration#axconfig)），用户通常不需要直接调用 `cargo xtask config`。

## 子命令

| 子命令 | 说明 |
|--------|------|
| `config platform-path --package <PKG>` | 定位平台包的 `axconfig.toml`，输出绝对路径 |
| `config read -r <ITEM> <SPEC>...` | 合并多个配置规范文件，读取并打印单个配置项 |
| `config generate -o <OUT> <SPEC>...` | 合并配置规范并生成完整的 TOML 配置文件 |
| `config inspect --package <PKG> [...]` | 输出平台包的关键字段（package/platform/arch/SMP/内存），供 Makefile 解析 |

## platform-path

```bash
cargo xtask config platform-path --package ax-plat-riscv64-custom
# 输出：/path/to/platforms/ax-plat-riscv64-custom/axconfig.toml
```

复用 `build.rs::resolve_platform_config_by_package`：先加载 workspace metadata，在依赖中查找匹配的平台包，按三级回退（workspace metadata → deps metadata → 包名↔目录映射）定位 `axconfig.toml`。找不到时报错。定位算法详见 [参数与配置](./configuration#平台配置文件查找)。

## read

```bash
cargo xtask config read -r plat.max-cpu-num \
    os/arceos/configs/defconfig.toml \
    platforms/ax-plat-riscv64-custom/axconfig.toml
```

- `<SPEC>...` 是一个或多个 TOML 配置规范文件，按顺序合并（后者覆盖前者）。
- `-r/--read <ITEM>` 是要读取的键，支持 `key` 或 `table.key` 形式。
- 输出该配置项的值（单行）。

底层调用 `ax_config_gen::read_config_value`。

## generate

```bash
cargo xtask config generate \
    -o tmp/axbuild/axconfig/myapp/aarch64/.axconfig.toml \
    -c oldconfig.toml \
    -w plat.max-cpu-num=4 \
    os/arceos/configs/defconfig.toml \
    platforms/ax-plat-aarch64-custom/axconfig.toml
```

| 参数 | 作用 |
|------|------|
| `<SPEC>...` | 按顺序合并的配置规范文件 |
| `-o/--output <PATH>` | 输出文件路径 |
| `-c/--oldconfig <PATH>` | 旧的配置文件，用于保留用户已有自定义值 |
| `-w/--write <table.key=value>` | 直接覆盖某个配置项（可多次） |

生成后会对两类异常项发出警告（`[WARN]`，非致命）：

- **untouched**：在 oldconfig 中未设置、使用默认值的项
- **extra**：在规范中找不到、被忽略的项

`keep_backup: true` 表示会保留旧文件备份。这是 `cargo xtask <os> build` 在 [参数与配置 §axconfig](./configuration#axconfig) 内部调用的同一个函数。

## inspect

```bash
# 人类可读形式（默认）
cargo xtask config inspect --package ax-plat-riscv64-custom

# Makefile 单行 key=value 形式
cargo xtask config inspect --package ax-plat-riscv64-custom --makefile
```

输出平台包的关键字段：

| 字段 | 配置键 | 用途 |
|------|--------|------|
| `PLAT_CONFIG` | 平台 `axconfig.toml` 路径 | Makefile 知道去哪读 |
| `PLAT_PACKAGE` | `package` | 平台包名 |
| `PLAT_NAME` | `platform` | 平台名 |
| `PLAT_ARCH` | `arch` | 架构 |
| `PLAT_SMP` | `plat.max-cpu-num` | SMP 核数 |
| `PHYS_MEMORY_SIZE` | `plat.phys-memory-size` | 物理内存大小 |

| 参数 | 作用 |
|------|------|
| `--package <PKG>` | 平台包名（必需） |
| `--manifest-dir <DIR>` | 用指定目录的 `Cargo.toml` 做依赖查找（而非默认 workspace） |
| `--config <PATH>` | 直接指定 `axconfig.toml` 路径，跳过包解析 |
| `--makefile` | 输出单行 `PLAT_CONFIG=... PLAT_PACKAGE=... ...`，便于 Makefile `$(shell ...)` 解析；默认对值做 shell 转义 |

`--manifest-dir` 用于 Makefile 在 app 源码目录内调用时，让 `axbuild` 以该 app 的依赖视角解析平台包（等价于 `cargo metadata --manifest-path <DIR>/Cargo.toml`）。

## 模块组成

| 代码位置 | 作用 |
|----------|------|
| `scripts/axbuild/src/config.rs` | 全部子命令实现、CLI 参数定义 |
| `os/arceos/configs/defconfig.toml` | 默认配置规范（所有项的默认值） |
| `<platform>/axconfig.toml` | 平台特定配置规范 |

底层依赖：`ax_config_gen`（配置合并引擎）与 `scripts/axbuild/src/build.rs`（平台包解析）。
