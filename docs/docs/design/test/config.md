---
sidebar_position: 8
sidebar_label: "配置与命名规范"
---

# 配置文件规范与命名约定

测试用例通过 TOML 配置文件控制构建和运行行为。按用途分为三类：QEMU 运行配置、板级测试配置和构建配置。

## 1. QEMU 运行配置

**文件名格式**：`qemu-{arch}.toml`

**适用范围**：StarryOS、ArceOS

**完整字段参考：**

| 字段 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `args` | `[String]` | 是 | — | QEMU 命令行参数，支持 `${workspace}` 占位符 |
| `uefi` | `bool` | 否 | `false` | 是否使用 UEFI 启动 |
| `to_bin` | `bool` | 否 | `false` | 是否将 ELF 转换为 raw binary |
| `shell_prefix` | `String` | 否 | — | Shell 提示符前缀，用于检测 shell 就绪 |
| `shell_init_cmd` | `String` | 否 | — | Shell 就绪后执行的命令，支持多行 `'''` 语法 |
| `success_regex` | `[String]` | 是 | — | 成功判定正则列表，任一匹配即判定成功 |
| `fail_regex` | `[String]` | 否 | `[]` | 失败判定正则列表，任一匹配即判定失败 |
| `timeout` | `u64` | 否 | — | 超时秒数 |

**判定逻辑：**

1. QEMU 启动后开始捕获串口输出
2. 每收到新输出时，先检查 `fail_regex`：任一匹配 → 判定**失败**
3. 再检查 `success_regex`：任一匹配 → 判定**成功**
4. 若设置了 `shell_prefix`，先等待该前缀出现在输出中，然后发送 `shell_init_cmd`
5. 超时未判定 → 判定**失败**

## 2. 板级测试配置

**文件名格式**：`board-{board_name}.toml`

**适用范围**：StarryOS

**完整字段参考：**

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `board_type` | `String` | 是 | 板型标识，需对应 `os/<OS>/configs/board/{board_name}.toml` |
| `shell_prefix` | `String` | 是 | Shell 提示符前缀 |
| `shell_init_cmd` | `String` | 是 | Shell 就绪后执行的命令 |
| `success_regex` | `[String]` | 是 | 成功判定正则列表 |
| `fail_regex` | `[String]` | 否 | 失败判定正则列表 |
| `timeout` | `u64` | 是 | 超时秒数 |

## 3. 构建配置

**文件名格式**：`build-{target}.toml`

**适用范围**：ArceOS Rust 测试

**完整字段参考：**

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `features` | `[String]` | 否 | 启用的 Cargo features |
| `log` | `String` | 否 | 日志级别 |
| `max_cpu_num` | `u32` | 否 | 最大 CPU 数量 |
| `[env]` | Table | 否 | 构建时环境变量 |

## 4. 命名约定

统一目录和配置文件的命名规则，确保跨 OS 一致性。

### 4.1 目录命名规则

- 使用小写字母、数字、连字符和下划线
- 测试用例目录名应简短且有描述性：`smoke`、`stress-ng-0`、`helloworld`

### 4.2 配置文件命名规则

| 文件 | 格式 | 示例 |
|------|------|------|
| QEMU 配置 | `qemu-{arch}.toml` | `qemu-aarch64.toml`、`qemu-x86_64.toml` |
| 板级配置 | `board-{board_name}.toml` | `board-orangepi-5-plus.toml` |
| 构建配置 | `build-{target}.toml` | `build-x86_64-unknown-none.toml`、`build-aarch64-unknown-none-softfloat.toml` |

### 4.3 支持的目标架构

| 架构缩写 | 完整 Target | QEMU 参数 | 说明 |
|----------|-------------|-----------|------|
| `x86_64` | `x86_64-unknown-none` | `-machine q35 -cpu max` | x86_64 Q35 平台 |
| `aarch64` | `aarch64-unknown-none-softfloat` | `-cpu cortex-a53` | ARM Cortex-A53 |
| `riscv64` | `riscv64gc-unknown-none-elf` | `-cpu rv64` | RISC-V 64 位 |
| `loongarch64` | `loongarch64-unknown-none-softfloat` | `-machine virt -cpu la464` | LoongArch LA464 |
