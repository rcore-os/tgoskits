# ArceOS 配置

本目录只保存运行工具配置。编译输入由 `ax_config::CommonConfig` 和 `ArceosConfig` 表达，内置配方位于 `scripts/axbuild/src/arceos/config/presets/`；构建不扫描或复制旧板卡编译 TOML。

## 1. 构建输入

`prepare_arceos_request()` 将来源、用户覆盖、本次环境及 CLI 解析为完整请求，Cargo 和运行入口消费同一结果。

### 1.1 内置配方

`config ls` 列出名称、板卡和 target；`config show` 展示实际配置但不写快照。可以直接构建，不必先导出文件。

```bash
cargo xtask arceos config ls
cargo xtask arceos config show --preset qemu-aarch64
cargo xtask arceos build --preset qemu-aarch64
```

只有裸 `arceos qemu` 在没有应用来源时默认选取 Hello 配方；显式 `--package` 不自动套用该板卡策略。

### 1.2 用户文件

`--config local.toml` 可提供完整自定义输入；同时传 `--preset`，或在文件中声明 `preset`，则应用稀疏覆盖。显式文件缺失、未知键、类型错误和选择器冲突均报错，不生成默认文件。

```bash
cargo xtask arceos build --preset qemu-aarch64 --config local.toml
```

数组整体替换，feature 增删与有限 unset 的使用方法见[配置说明](../../../docs/docs/build/configuration.md)。用户显式 `app-c` 相对文件目录解析，内置路径相对工作区根，resolver 将其绝对化。

## 2. 运行资源

QEMU、U-Boot 与远程板卡 TOML 描述启动和部署，不参与编译字段合并；DTB、ITS 与 VM 等资源保留各自格式及位置。

### 2.1 QEMU 配置

`qemu/` 保存可用 `--qemu-config` 显式选择的架构运行模板，不声明应用成功标记。应用目录自己的 `qemu-<arch>.toml` 可以描述专用磁盘、宿主服务、超时和成功规则，仍按应用发现流程使用。

### 2.2 显式导出

`defconfig <preset>` 使用 `export_config()` 原子写出展开 TOML 到 `tmp/axbuild/config/<package>/build-<target>.toml`，不保留控制键，也不把有效 CPU 默认容量改写为显式上限。写出成功后才更新文件选择并清空旧 SMP 和运行配置；失败保留原文件与快照。
