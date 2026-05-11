---
sidebar_position: 4
sidebar_label: "运行配置文件"
---

# 测试配置文件

测试配置文件定义了每个用例在 QEMU 或板卡上的运行参数，包括 QEMU 命令行参数、Shell 交互配置、超时设置和结果判定规则。每个测试用例通过 `qemu-{arch}.toml` 或 `board-{board_name}.toml` 描述自己的运行需求，这些配置文件是测试发现算法的识别标记，也是运行时行为的声明式定义。

## QEMU 运行配置 (`qemu-{arch}.toml`)

每个 QEMU 用例通过一个 TOML 文件声明运行环境参数，主要字段如下：

| 字段 | 类型 | 说明 |
|------|------|------|
| `args` | `[String]` | QEMU 命令行参数，支持 `${workspace}` 占位符 |
| `uefi` | `bool` | 是否使用 UEFI 启动 |
| `to_bin` | `bool` | 是否将 ELF 转为 raw binary |
| `shell_prefix` | `String` | Shell 提示符前缀 |
| `shell_init_cmd` | `String` | Shell 就绪后执行的命令（与 `test_commands` 互斥） |
| `test_commands` | `[String]` | 分组测试命令列表（与 `shell_init_cmd` 互斥） |
| `success_regex` | `[String]` | 成功判定正则列表 |
| `fail_regex` | `[String]` | 失败判定正则列表 |
| `timeout` | `u64` | 超时秒数（0 = 禁用） |

`args` 是最核心的字段，直接传递给 QEMU 命令行，可以指定内存大小、CPU 数量、设备挂载等。`${workspace}` 占位符在运行时替换为 workspace 根目录的绝对路径，使得配置文件可以引用项目中的文件（如 rootfs 镜像、磁盘映像）。

`shell_prefix` 和 `shell_init_cmd` 共同定义了 Shell 交互模式：axbuild 等待 QEMU 输出中出现匹配 `shell_prefix` 的字符串（表示 shell 就绪），然后发送 `shell_init_cmd` 中定义的命令。`success_regex` 和 `fail_regex` 扫描 QEMU 的全部输出来判定测试结果——如果任何 `fail_regex` 匹配，则判定为失败；如果所有 `success_regex` 都匹配且无 `fail_regex` 匹配，则判定为成功。

`test_commands` 与 `shell_init_cmd` 的互斥由 `validate_grouped_qemu_commands()` 校验。

互斥校验确保用户不会同时指定两种运行模式（单命令模式和分组命令模式），避免运行时行为不确定。

## 板级配置 (`board-{board_name}.toml`)

板级运行配置文件与 QEMU 配置字段相似，但不需要 `args`（硬件决定）和 `test_commands`：

| 字段 | 类型 | 说明 |
|------|------|------|
| `board_type` | `String` | 板型标识 |
| `shell_prefix` | `String` | Shell 提示符前缀 |
| `shell_init_cmd` | `String` | Shell 就绪后执行的命令 |
| `success_regex` | `[String]` | 成功判定正则列表 |
| `fail_regex` | `[String]` | 失败判定正则列表 |
| `timeout` | `u64` | 超时秒数 |

板级配置与 QEMU 配置类似，但不需要 `args`（板卡的 QEMU 参数由硬件决定）和 `test_commands`（板级测试目前只支持单命令模式）。`board_type` 对应 ostool-server 中注册的板卡类型标识。

## SMP 参数注入

`apply_smp_qemu_arg()` 和 `smp_from_qemu_arg()` 共同实现构建配置与 QEMU 配置之间的 SMP 双向同步。`apply_smp_qemu_arg()` 确保 QEMU 的 `-smp` 参数与传入的 CPU 数量一致，`smp_from_qemu_arg()` 从 QEMU 配置中提取 SMP 值供 reverse check 使用。

## 超时缩放

环境变量 `AXBUILD_TEST_TIMEOUT_SCALE` 可线性放大所有 case 超时（用于 CI 慢环境）。

`apply_timeout_scale()` 从 QEMU 配置中读取 `timeout` 字段，乘以缩放因子后写回。CI 环境的执行速度通常比本地开发环境慢（尤其是在共享 runner 上），直接使用本地调试时的超时值可能导致用例因超时而误报失败。`AXBUILD_TEST_TIMEOUT_SCALE` 允许 CI 脚本按比例放大所有用例的超时值（如设置为 `2.0` 将超时翻倍），而不需要逐个修改配置文件。
