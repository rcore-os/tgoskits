# AxVisor Shell 模块详细介绍

## 概述

AxVisor Shell 模块是 AxVisor 虚拟化管理器中的一个重要组件，为用户提供了一个功能丰富的交互式命令行界面。该模块基于 Rust 语言实现，具有完整的命令解析、历史记录、终端控制和虚拟机管理功能。

```
┌─────────────────────────────────────────────┐
│            Shell Interface Layer            │
│      ┌─────────────┐  ┌─────────────┐       │
│      │ Interactive │  │ Command CLI │       │
│      │    Shell    │  │   Parser    │       │
│      └─────────────┘  └─────────────┘       │
├─────────────────────────────────────────────┤
│             VM Management Facade            │
│    ┌─────────────┐  ┌──────────────────┐    │
│    │ Controller  │  │ Query & Monitor  │    │
│    └─────────────┘  └──────────────────┘    │
├─────────────────────────────────────────────┤
│          Existing VMM Components            │
│   VMList │ VCpu │ IVC │ Timer │ Config │    │
└─────────────────────────────────────────────┘
```

## 模块架构

### 目录结构
```
src/shell/
├── mod.rs                  # 主模块，实现交互式shell界面
└── command/
    ├── mod.rs              # 命令框架和解析器
    ├── base.rs             # 基础Unix命令实现
    ├── vm.rs               # 虚拟机管理命令
    └── history.rs          # 命令历史记录管理
```

## 核心组件

### 1. 交互式Shell界面 ([shell/mod.rs](/src/shell/mod.rs))

#### 主要功能
- **实时字符输入处理**: 支持逐字符读取和处理用户输入
- **光标控制**: 支持左右箭头键移动光标位置
- **行编辑功能**: 支持删除、插入字符等基本编辑操作
- **历史记录导航**: 通过上下箭头键浏览命令历史
- **转义序列处理**: 支持部分ANSI转义序列和特殊键处理

#### 关键特性
```rust
const MAX_LINE_LEN: usize = 256;   // 最大命令行长度

enum InputState {
    Normal,      // 正常输入状态
    Escape,      // ESC键按下状态
    EscapeSeq,   // 转义序列处理状态
}
```

#### 支持的按键操作
- **回车键 (CR/LF)**: 执行当前命令
- **退格键 (BS/DEL)**: 删除光标前的字符
- **ESC序列**: 处理箭头键和功能键
- **上/下箭头**: 浏览命令历史
- **左/右箭头**: 移动光标位置

### 2. 命令框架和解析器 ([command/mod.rs](/src/shell/command/mod.rs))

#### 命令树结构
采用基于树状结构的命令系统，支持主命令和子命令的层次化组织：

```rust
#[derive(Debug, Clone)]
pub struct CommandNode {
    handler: Option<fn(&ParsedCommand)>,    // 命令处理函数
    subcommands: BTreeMap<String, CommandNode>, // 子命令映射
    description: &'static str,               // 命令描述
    usage: Option<&'static str>,            // 使用说明
    log_level: log::LevelFilter,            // 日志级别
    options: Vec<OptionDef>,                // 命令选项
    flags: Vec<FlagDef>,                    // 命令标志
}
```

#### 命令解析功能
- **智能分词**: 支持引号包围的参数和转义字符
- **选项解析**: 支持短选项(-x)和长选项(--option)
- **参数验证**: 自动验证必需选项和参数格式
- **错误处理**: 详细的错误信息和使用提示
- **灵活格式**: 支持 `--option=value` 和 `--option value` 两种格式

#### 分词示例

```rust
// src/shell/command/mod.rs:186-215
fn tokenize(input: &str) -> Vec<String> {
    // 支持引号包围的参数
    // 例: echo "hello world" -> ["echo", "hello world"]

    // 支持转义字符
    // 例: echo \"quoted\" -> ["echo", "\"quoted\""]

    // 自动处理空白符分隔
}
```

#### 解析错误类型
```rust
pub enum ParseError {
    UnknownCommand(String),           // 未知命令
    UnknownOption(String),            // 未知选项
    MissingValue(String),             // 缺少参数值
    MissingRequiredOption(String),    // 缺少必需选项
    NoHandler(String),                // 没有处理函数
}
```

### 3. 基础Unix命令 ([command/base.rs](/src/shell/command/base.rs))

实现了部分Unix风格命令，包括：

#### 文件系统操作命令
- **ls**: 列出目录内容，支持 `-l`(详细信息) 和 `-a`(显示隐藏文件) 选项
- **cat**: 显示文件内容，支持多文件连接输出
- **mkdir**: 创建目录，支持 `-p`(创建父目录) 选项
- **rm**: 删除文件和目录，支持 `-r`(递归)、`-f`(强制)、`-d`(删除空目录) 选项
- **cp**: 复制文件和目录，支持 `-r`(递归复制) 选项
- **mv**: 移动/重命名文件和目录
- **touch**: 创建空文件

#### 系统信息命令
- **pwd**: 显示当前工作目录
- **cd**: 切换目录
- **uname**: 显示系统信息，支持 `-a`(全部信息)、`-s`(内核名)、`-m`(架构) 选项
- **echo**: 输出文本，支持 `-n`(不换行) 选项和文件重定向

#### 系统控制命令
- **exit**: 退出shell，支持指定退出码
- **log**: 控制日志级别 (off/error/warn/info/debug/trace) **有计划实现**

#### 文件权限显示
实现了完整的Unix风格文件权限显示：
```rust
fn file_type_to_char(ty: FileType) -> char {
    match ty {
        is_dir() => 'd',
        is_file() => '-',
        is_symlink() => 'l',
        is_char_device() => 'c',
        is_block_device() => 'b',
        is_socket() => 's',
        is_fifo() => 'p',
        _ => '?'
    }
}
```

### 4. 虚拟机管理命令 ([command/vm.rs](/src/shell/command/vm.rs))

提供完整的虚拟机生命周期管理功能：

#### 主要子命令
- **vm create**: 从配置文件创建虚拟机，支持批量创建多个VM
- **vm start**: 启动虚拟机
  - 不带参数：启动所有虚拟机
  - 指定VM ID：启动特定虚拟机
  - 支持 `--detach` 后台模式运行
  - 支持 `--console` 连接到控制台(计划实现)
- **vm stop**: 停止虚拟机
  - 必须指定VM ID
  - 支持 `--force` 强制停止
  - 支持 `--graceful` 优雅关闭
- **vm suspend**: 暂停(挂起)运行中的虚拟机 (功能不完善)
  - 必须指定VM ID
  - 所有VCpu将在下次VMExit时进入等待队列
  - VM状态转换为Suspended
- **vm resume**: 恢复已暂停的虚拟机 (功能不完善)
  - 必须指定VM ID
  - 唤醒所有VCpu任务，恢复执行
  - VM状态从Suspended转换回Running
- **vm restart**: 重启虚拟机，必须指定VM ID (功能不完善)
  - 支持 `--force` 强制重启
  - 自动等待VM完全停止后再启动
- **vm delete**: 删除虚拟机
  - 必须指定VM ID
  - 需要 `--force` 确认删除
  - 支持 `--keep-data` 保留数据选项
- **vm list**: 列出虚拟机
  - 显示所有已创建的虚拟机
  - `--format json` 支持JSON格式输出
  - 表格模式显示：ID、名称、状态、VCPU列表、内存、VCPU状态汇总
- **vm show**: 显示虚拟机详细信息
  - 必须指定VM ID
  - 默认模式：显示基本信息和摘要
  - `--full` / `-f`: 显示完整详细信息(内存区域、设备、配置等)
  - `--config` / `-c`: 显示配置信息(入口点、中断模式、直通设备等)
  - `--stats` / `-s`: 显示统计信息(EPT、内存区域、设备数量等)

#### 功能特性
``` rust
// 虚拟机状态显示
let state = if vm.running() {
    "🟢 running"
} else if vm.shutting_down() {
    "🟡 stopping"
} else {
    "🔴 stopped"
};
```

#### 详细信息显示
- **配置信息** (`--config`):
  - BSP/AP入口点地址
  - 中断模式 (InterruptMode)
  - 直通设备列表 (PassThrough Devices)
    - 设备名称、GPA范围、HPA范围
  - 模拟设备列表 (Emulated Devices)
- **资源统计** (`--stats`):
  - EPT根页表地址
  - 内存区域详细信息 (GPA范围、大小)
  - VCPU数量和设备数量
- **运行状态**:
  - VCPU状态分布 (Free/Running/Blocked/Invalid/Created/Ready)
  - CPU亲和性设置 (Physical CPU affinity mask)
  - 虚拟机整体状态 (运行中/停止中/已停止)

#### 支持的选项和标志
- `--all` / `-a`: (vm list) 显示所有虚拟机(默认已包含所有VM)
- `--format json`: (vm list) JSON格式输出
- `--full` / `-f`: (vm show) 显示完整详细信息
- `--config` / `-c`: (vm show) 显示配置信息
- `--stats` / `-s`: (vm show) 显示统计信息
- `--force` / `-f`: (vm stop/delete/restart) 强制操作(无需确认)
- `--graceful` / `-g`: (vm stop) 优雅关闭
- `--console` / `-c`: (vm start) 连接到控制台(计划实现)
- `--watch` / `-w`: (vm status) 实时监控(已移除,功能未实现)
- `--keep-data`: (vm delete) 保留VM数据(功能未实现)

#### 输出格式示例

**Table格式** (默认):
```
VM ID  NAME            STATUS       VCPU            MEMORY     VCPU STATE
------ --------------- ------------ --------------- ---------- --------------------
0      linux-vm        Running      0,1             512MB      Run:2
1      test-vm         Stopped      0               256MB      Free:1
```

**简化表格** (vm list 输出):
```
ID    NAME           STATE      VCPU   MEMORY
----  -----------    -------    ----   ------
0     linux-vm       Running       2    512MB
1     test-vm        Stopped       1    256MB
```

**JSON格式** (`--format json`):
``` json
{
  "vms": [
    {
      "id": 0,
      "name": "linux-vm",
      "state": "running",
      "vcpu": 2,
      "memory": "512MB",
      "interrupt_mode": "Emulated"
    }
  ]
}
```

### 5. 命令历史管理 ([command/history.rs](/src/shell/command/history.rs))

#### 核心功能
```rust
pub struct CommandHistory {
    history: Vec<String>,       // 历史命令列表
    current_index: usize,       // 当前索引位置
    max_size: usize,           // 最大历史记录数
}
```

#### 关键特性
- **去重处理**: 避免连续重复命令
- **循环缓冲**: 超出最大容量时自动删除最旧记录
- **导航功能**: 支持前进/后退浏览
- **空命令过滤**: 自动忽略空白命令

#### 终端控制
```rust
pub fn clear_line_and_redraw(
    stdout: &mut dyn Write,
    prompt: &str,
    content: &str,
    cursor_pos: usize,
) {
    write!(stdout, "\r");              // 回到行首
    write!(stdout, "\x1b[2K");         // 清除整行
    write!(stdout, "{}{}", prompt, content); // 重绘内容
    // 调整光标位置
    if cursor_pos < content.len() {
        write!(stdout, "\x1b[{}D", content.len() - cursor_pos);
    }
}
```

## 内置命令

### 系统级内置命令
- **help**: 显示可用命令列表
  - 列出所有顶级命令及其子命令
  - 包含内置命令和系统命令
- **help `<command>`**: 显示特定命令的详细帮助
  - 显示命令描述
  - 显示用法 (Usage)
  - 列出所有选项 (Options)
  - 列出所有标志 (Flags)
  - 列出所有子命令 (Subcommands)
- **clear**: 清屏 (发送ANSI清屏序列 `\x1b[2J\x1b[H`)
- **exit/quit**: 退出shell

### VM 管理命令列表

执行 `help vm` 可以看到完整的 VM 命令列表：

```
VM - virtual machine management

Most commonly used vm commands:
  create    Create a new virtual machine
  start     Start a virtual machine
  stop      Stop a virtual machine
  suspend   Suspend (pause) a running virtual machine
  resume    Resume a suspended virtual machine
  restart   Restart a virtual machine
  delete    Delete a virtual machine

Information commands:
  list      Show table of all VMs
  show      Show VM details (requires VM_ID)
            - Default: basic information
            - --full: complete detailed information
            - --config: show configuration
            - --stats: show statistics

Use 'vm <command> --help' for more information on a specific command.
```

### 错误处理
Shell会对命令解析和执行错误提供友好的提示信息：
```bash
# 未知命令
$ unknown_cmd
Error: Unknown command 'unknown_cmd'
Type 'help' to see available commands

# 未知选项
$ ls --invalid
Error: Unknown option '--invalid'

# 缺少参数值
$ vm create
Error: No VM configuration file specified
Usage: vm create [CONFIG_FILE]

# 缺少必需选项
$ vm stop
Error: No VM specified
Usage: vm stop [OPTIONS] <VM_ID>
```

## VM 生命周期和状态管理

### VM 状态机

AxVisor 的 VM 状态遵循严格的状态机模型：

```
                   ┌──────────┐
                   │ Loading  │ (VM 正在创建/加载)
                   └────┬─────┘
                        │ create complete
                        ▼
                   ┌──────────┐
            ┌─────▶│  Loaded  │ (VM 已加载，未启动)
            │      └────┬─────┘
            │           │ start
            │           ▼
            │      ┌──────────┐
            │  ┌───┤ Running  │ (VM 正在运行)
            │  │   └────┬─────┘
            │  │        │
            │  │        ├─── suspend ────▶ ┌───────────┐
            │  │        │                  │ Suspended │ (VM 已暂停)
            │  │        │                  └─────┬─────┘
            │  │        │                        │ resume
            │  │        │ ◀──────────────────────┘
            │  │        │
            │  │        │ shutdown/stop
            │  │        ▼
            │  │   ┌──────────┐
            │  │   │ Stopping │ (VM 正在关闭)
            │  │   └────┬─────┘
            │  │        │ all vcpus exited
            │  │        ▼
            │  │   ┌──────────┐
            │  └──▶│ Stopped  │ (VM 已停止)
            │      └────┬─────┘
            │           │ delete
            │           ▼
            │      [Resources Freed]
            │           │
            └───────────┘ restart
```

### VM 状态定义

```rust
pub enum VMStatus {
    Loading,    // VM 正在创建/加载
    Loaded,     // VM 已加载但未启动
    Running,    // VM 正在运行
    Suspended,  // VM 已暂停（可恢复）
    Stopping,   // VM 正在关闭中
    Stopped,    // VM 已完全停止
}
```

#### 状态转换规则

| 当前状态 | 可执行操作 | 目标状态 | 说明 |
|---------|-----------|---------|------|
| Loading | - | Loaded | 创建完成后自动转换 |
| Loaded | `vm start` | Running | 启动 VCpu 任务开始执行 |
| Loaded | `vm delete` | Stopped | 直接删除未启动的 VM |
| Running | `vm stop` | Stopping | 发送关闭信号给所有 VCpu |
| Running | `vm suspend` | Suspended | 暂停所有 VCpu 执行 |
| Suspended | `vm resume` | Running | 恢复 VCpu 执行 |
| Suspended | `vm stop` | Stopping | 从暂停状态直接关闭 |
| Stopping | - | Stopped | 所有 VCpu 退出后自动转换 |
| Stopped | `vm delete` | [释放资源] | 清理并释放 VM 资源 |
| Stopped | `vm start` | Running | 重新启动已停止的 VM |

### VCpu 生命周期

每个 VM 包含一个或多个 VCpu（虚拟 CPU），它们的生命周期与 VM 状态紧密关联：

```
VM Start
   │
   ├─▶ 创建 VCpu 任务 (alloc_vcpu_task)
   │     │
   │     ├─ 设置 CPU 亲和性
   │     ├─ 初始化 TaskExt (Weak 引用 VM)
   │     └─ spawn_task 到调度器
   │
   ├─▶ VCpu 任务运行 (vcpu_run)
   │     │
   │     ├─ 等待 VM Running 状态
   │     ├─ mark_vcpu_running()
   │     └─ 进入运行循环
   │           │
   │           ├─ vm.run_vcpu() - 执行 Guest 代码
   │           ├─ 处理 VM Exit (hypercall, interrupt, halt...)
   │           ├─ 检查 VM 暂停状态
   │           └─ 检查 VM 关闭状态 ──┐
   │                                 │
   │                                 ▼ vm.stopping() == true
   ├─▶ VCpu 任务退出                │
   │     │◀───────────────────────┘
   │     ├─ mark_vcpu_exiting() - 递减运行计数
   │     ├─ 最后一个 VCpu 设置 VM 为 Stopped
   │     └─ 任务函数返回，进入 Exited 状态
   │
   └─▶ VCpu 清理 (cleanup_vm_vcpus)
         │
         ├─ 遍历所有 VCpu 任务
         ├─ 调用 task.join() 等待退出
         ├─ 释放 VM 的 Arc 引用
         └─ 清理等待队列资源
```

#### VCpu 任务特性

1. **Weak 引用**：VCpu 任务通过 `TaskExt` 持有 VM 的 `Weak` 引用，避免循环引用
2. **CPU 亲和性**：可配置 VCpu 绑定到特定物理 CPU
3. **协作式退出**：VCpu 检测到 `vm.stopping()` 后主动退出
4. **引用计数管理**：退出前释放所有对 VM 的引用

#### VCpu 任务生命周期扩展

```
VM Running
   │
   ├─▶ VCpu 任务运行循环
   │     │
   │     ├─ vm.run_vcpu() - 执行 Guest 代码
   │     ├─ 处理 VM Exit
   │     ├─ 检查 VM 状态
   │     │    │
   │     │    ├─ vm.stopping() == true ──▶ 退出循环
   │     │    │
   │     │    └─ vm.vm_status() == Suspended ──▶ 进入等待队列
   │     │                                          │
   │     │                                          │ wait for notify
   │     │                                          │
   │     │                                          ▼
   │     │                                     被唤醒 (resume)
   │     │                                          │
   │     │    ◀────────────────────────────────────┘
   │     │
   │     └─ 继续执行
```

### VM 删除流程详解

`vm delete` 命令执行完整的资源清理流程，确保没有资源泄漏：

#### 删除流程步骤

```
1. 状态检查和关闭信号
   ├─ 检查 VM 当前状态
   ├─ 如果 Running/Suspended/Stopping
   │    ├─ 设置状态为 Stopping
   │    └─ 调用 vm.shutdown() 通知 Guest
   └─ 如果 Loaded
        └─ 直接设置为 Stopped

2. 从全局列表移除
   ├─ 调用 vm_list::remove_vm(vm_id)
   ├─ 获得 VM 的 Arc<AxVM> 引用
   └─ 打印当前 Arc 引用计数 (调试信息)

3. VCpu 任务清理 ⭐ (核心步骤)
   ├─ 调用 cleanup_vm_vcpus(vm_id)
   │    ├─ 从全局队列移除 VM 的 VCpu 列表
   │    ├─ 遍历所有 VCpu 任务
   │    │    ├─ task.join() - 阻塞等待任务退出
   │    │    └─ 释放 VCpu 持有的 VM Arc 引用
   │    └─ 清理等待队列资源
   └─ 打印清理后的 Arc 引用计数

4. 验证引用计数
   ├─ 期望：Arc count == 1 (仅剩当前函数持有)
   ├─ 实际：检查并打印 Arc::strong_count(&vm)
   └─ 如果 count > 1：警告可能的引用泄漏

5. 资源释放
   ├─ 函数返回时 vm (Arc) 被 drop
   ├─ 如果 count == 1，触发 AxVM::drop()
   │    ├─ 释放 EPT 页表
   │    ├─ 释放内存区域
   │    └─ 释放设备资源
   └─ VM 对象完全销毁
```

#### 关键实现代码片段

```rust
// src/vmm/vcpus.rs:241-260
pub(crate) fn cleanup_vm_vcpus(vm_id: usize) {
    if let Some(vm_vcpus) = VM_VCPU_TASK_WAIT_QUEUE.remove(&vm_id) {
        let task_count = vm_vcpus.vcpu_task_list.len();

        info!("VM[{}] Joining {} VCpu tasks...", vm_id, task_count);

        // ⭐ 关键：真正 join 所有 VCpu 任务
        for (idx, task) in vm_vcpus.vcpu_task_list.iter().enumerate() {
            debug!("VM[{}] Joining VCpu task[{}]: {}", vm_id, idx, task.id_name());
            if let Some(exit_code) = task.join() {
                debug!("VM[{}] VCpu task[{}] exited with code: {}", vm_id, idx, exit_code);
            }
        }

        info!("VM[{}] VCpu resources cleaned up, {} VCpu tasks joined successfully",
              vm_id, task_count);
    }
}
```

#### 删除示例输出

```bash
$ vm delete 2
Deleting stopped VM[2]...
  [Debug] VM Arc strong_count: 2
✓ VM[2] removed from VM list
  Waiting for vCPU threads to exit...
  [Debug] VM Arc count before cleanup: 1
  Cleaning up VCpu resources...
[ 67.812092 0:2 axvisor::vmm::vcpus:243] VM[2] Joining 1 VCpu tasks...
[ 67.819730 0:2 axvisor::vmm::vcpus:253] VM[2] VCpu resources cleaned up, 1 VCpu tasks joined successfully
  [Debug] VM Arc count after final wait: 1
✓ VM[2] deleted completely
  [Debug] VM Arc strong_count: 1
  ✓ Perfect! VM will be freed immediately when function returns
  VM[2] will be freed now
[ 67.848026 0:2 axvm::vm:884] Dropping VM[2]
[ 67.853407 0:2 axvm::vm:775] Cleaning up VM[2] resources...
[ 67.860698 0:2 axvm::vm:878] VM[2] resources cleanup completed
[ 67.867209 0:2 axvm::vm:889] VM[2] dropped
✓ VM[2] deletion completed
```

### 命令提示符
```rust
pub fn print_prompt() {
    #[cfg(feature = "fs")]
    print!("axvisor:{}$ ", std::env::current_dir().unwrap());
    #[cfg(not(feature = "fs"))]
    print!("axvisor:$ ");
    std::io::stdout().flush().unwrap();
}
```

## 扩展性

### 添加新命令

1. 在对应的模块中实现命令处理函数
2. 定义命令节点和选项/标志
3. 在 `build_command_tree()` 中注册命令

### 命令定义示例

```rust
tree.insert(
    "mycommand".to_string(),
    CommandNode::new("My custom command")
        .with_handler(my_command_handler)
        .with_usage("mycommand [OPTIONS] <ARGS>")
        .with_option(
            OptionDef::new("config", "Config file path")
                .with_short('c')
                .with_long("config")
                .required()
        )
        .with_flag(
            FlagDef::new("verbose", "Verbose output")
                .with_short('v')
                .with_long("verbose")
        ),
);
```

# 使用说明

## Shell功能特性

AxVisor Shell模块**默认启用**，但不同功能对features有不同要求：

### 功能分层

#### 🟢 基础功能（无需额外feature）
- 交互式命令行界面
- 命令历史记录（上下箭头导航）
- 光标移动和行编辑
- 内置命令：`help`, `clear`, `exit`
- 系统命令：`uname`, `log`
- VM管理命令：`vm list`, `vm show`, `vm status`, `vm stop` 等

#### 🟡 文件系统功能（需要 `fs` feature）
- 文件操作命令：`ls`, `cat`, `mkdir`, `rm`, `cp`, `mv`, `touch`, `cd`, `pwd`, `echo`
- `vm create` - 从配置文件创建VM
- `vm /` - 从文件系统加载VM镜像启动

## vmconfigs 配置说明

`vmconfigs` 参数决定了 AxVisor 启动时是否自动创建和启动虚拟机：

### 📌 配置行为

| vmconfigs 配置 | 启动行为 | 使用场景 |
|---------------|---------|---------|
| **有值**（指定配置文件）| ✅ 自动创建并启动VM | 预加载VM，启动后VM已运行 |
| **无值**（不指定）| ❌ 不创建VM，进入空Shell | 手动管理VM，通过Shell创建 |

### 配置示例

#### 场景1：自动启动VM
```bash
# VM会在启动时自动创建并运行
./axvisor.sh run \
  --plat aarch64-generic \
  --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml
```

**启动后**：
```
Welcome to AxVisor Shell!
...
VMM starting, booting VMs...
VM[0] boot success

axvisor:/$ vm list
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     nimbos-vm      🟢 running       1    512MB
```

#### 场景2：不自动启动VM（空Shell）
```bash
# 不指定 vmconfigs 参数
./axvisor.sh run --plat aarch64-generic --features fs,ept-level-4
```

**启动后**（需要手动创建VM）：
```
Welcome to AxVisor Shell!
...

axvisor:/$ vm list
No virtual machines found.

axvisor:/$ vm create /path/to/vm.toml
✓ Successfully created VM from config: /path/to/vm.toml

axvisor:/$ vm start 0
✓ VM[0] started successfully
```

### 配置方式

#### 命令行指定
```bash
./axvisor.sh run --vmconfigs configs/vms/vm1.toml,configs/vms/vm2.toml
```

#### 配置文件指定
在 `.hvconfig.toml` 中：
```toml
vmconfigs = [
    "configs/vms/nimbos-aarch64-qemu-smp1.toml",
    "configs/vms/linux-aarch64-qemu.toml"
]
```

### 💡 使用建议

| 使用场景 | 推荐配置 |
|---------|---------|
| **生产环境** - 固定的VM配置 | 指定 `vmconfigs`，自动启动 |
| **开发调试** - 频繁修改VM配置 | 不指定 `vmconfigs`，Shell中手动创建 |
| **演示测试** - 需要快速启动 | 指定 `vmconfigs`，自动启动 |
| **交互式管理** - 动态创建多个VM | 不指定或只指定部分，其余手动创建 |

## 启用方式

### 方式一：自动启动VM（指定 vmconfigs）

指定 `--vmconfigs` 参数，AxVisor 会在启动时自动创建并启动虚拟机：

```bash
# VM会自动启动
./axvisor.sh run \
  --plat aarch64-generic \
  --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml
```

**启动后状态**：
- ✅ VM已创建并运行
- ✅ Shell可直接管理VM
- ✅ 可执行 `vm list`, `vm status` 等命令

**可用功能**：
- VM状态查询和管理
- 系统信息查看
- 命令历史和行编辑
- 日志级别控制

**不可用功能**（无 `fs` feature时）：
- 文件操作命令
- 从文件系统动态创建新VM

### 方式二：空Shell模式（不指定 vmconfigs）

不指定 `--vmconfigs`，AxVisor 启动后不会创建VM，提供纯净的Shell环境：

```bash
# 启动时不创建VM，需要启用fs以便手动创建
./axvisor.sh run --plat aarch64-generic --features fs,ept-level-4
```

**启动后状态**：
- ❌ 无VM运行
- ✅ Shell就绪，等待用户操作
- ✅ 可通过 `vm create` 手动创建VM

**使用场景**：
- 需要在Shell中动态创建多个VM
- 测试不同的VM配置
- 交互式VM管理

### 方式三：完整Shell功能（带文件系统 + vmconfigs）

结合文件系统和 vmconfigs，既可以自动启动预定义的VM，又可以使用文件操作和动态创建VM：

#### 步骤1：准备磁盘镜像

```bash
# 创建磁盘镜像（以FAT32为例）
dd if=/dev/zero of=disk.img bs=1M count=64
mkfs.vfat disk.img

# 挂载并放入VM配置文件
mkdir -p mnt
sudo mount disk.img mnt
sudo cp configs/vms/*.toml mnt/
sudo umount mnt
```

#### 步骤2：运行AxVisor（完整功能）

```bash
# 同时启用文件系统和自动启动VM
./axvisor.sh run \
  --plat aarch64-generic \
  --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml \
  --features fs,ept-level-4 \
  --arceos-args "BUS=mmio,BLK=y,DISK_IMG=disk.img,MEM=8g,LOG=info"
```

**启动后状态**：
- ✅ VM已自动创建并运行
- ✅ 文件系统已挂载
- ✅ 可执行所有Shell命令
- ✅ 可从文件系统创建更多VM

**完整功能**：
``` bash
axvisor:/$ vm list           # 查看已启动的VM
axvisor:/$ ls -la            # 浏览文件系统
axvisor:/$ cat /vm2.toml     # 查看其他配置文件
axvisor:/$ vm create /vm2.toml  # 创建更多VM
```

#### 文件系统类型选择

ArceOS 默认使用 **FAT32** 文件系统。如需使用其他文件系统，可通过 ArceOS 的构建参数指定：

```bash
# 使用EXT4文件系统（需要创建ext4格式的磁盘镜像）
./axvisor.sh run \
  --plat aarch64-generic \
  --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml \
  --features fs,ept-level-4 \
  --arceos-features ext4fs \
  --arceos-args "BUS=mmio,BLK=y,DISK_IMG=disk-ext4.img,MEM=8g"
```

## 实际使用示例

### 示例1：NimbOS客户机（自动启动）

使用 `--vmconfigs` 让 NimbOS 在启动时自动运行：

```bash
# 1. 准备NimbOS镜像
./scripts/nimbos.sh --arch aarch64

# 2. 启动AxVisor（VM会自动启动）
./axvisor.sh run \
  --plat aarch64-generic \
  --features fs,ept-level-4 \
  --vmconfigs configs/vms/nimbos-aarch64-qemu-smp1.toml \
  --arceos-args "BUS=mmio,BLK=y,DISK_IMG=tmp/nimbos-aarch64.img,LOG=info"

# 3. 在Shell中操作（VM已运行）
# 查看VM状态
axvisor:/$ vm list
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     nimbos-vm      🟢 running       1    512MB

axvisor:/$ vm status 0        # 查看详细状态
axvisor:/$ log debug          # 调整日志级别
```

### 示例2：交互式创建VM（手动管理）

不使用 `--vmconfigs`，在Shell中手动创建和管理VM：

```bash
# 1. 准备镜像和配置文件
./scripts/nimbos.sh --arch aarch64

# 2. 启动AxVisor（不指定vmconfigs，不自动启动VM）
./axvisor.sh run \
  --plat aarch64-generic \
  --features fs,ept-level-4 \
  --arceos-args "BUS=mmio,BLK=y,DISK_IMG=tmp/nimbos-aarch64.img,LOG=info"

# 3. 在Shell中手动创建和启动VM
axvisor:/$ vm list
No virtual machines found.

axvisor:/$ ls /              # 浏览文件系统
nimbos-aarch64-qemu-smp1.toml
...

axvisor:/$ vm create /nimbos-aarch64-qemu-smp1.toml
✓ Successfully created VM from config

axvisor:/$ vm list -a
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     nimbos-vm      🔴 stopped       1    512MB

axvisor:/$ vm start 0
✓ VM[0] started successfully

axvisor:/$ vm list
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     nimbos-vm      🟢 running       1    512MB
```

### 示例3：混合模式（部分自动，部分手动）

自动启动一个VM，再手动创建更多：

```bash
# 启动AxVisor，自动启动第一个VM
./axvisor.sh run \
  --plat aarch64-generic \
  --features fs,ept-level-4 \
  --vmconfigs configs/vms/vm1.toml \
  --arceos-args "BUS=mmio,BLK=y,DISK_IMG=disk.img,LOG=info"

# Shell中查看和创建更多VM
axvisor:/$ vm list
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     vm1            🟢 running       2    1024MB

axvisor:/$ vm create /configs/vm2.toml
✓ Successfully created VM from config

axvisor:/$ vm start 1
✓ VM[1] started successfully

axvisor:/$ vm list
ID    NAME           STATE         VCPU   MEMORY
----  -----------    -------       ----   ------
0     vm1            🟢 running       2    1024MB
1     vm2            🟢 running       1    512MB
```

### 代码层面说明

Shell模块在代码中的启用方式：

```rust
// src/main.rs
fn main() {
    // ... 初始化代码 ...

    // Shell总是被调用，无条件编译
    shell::console_init();
}
```

```rust
// src/shell/command/base.rs
// 文件系统相关命令通过条件编译控制
#[cfg(feature = "fs")]
fn do_ls(cmd: &ParsedCommand) { /* ... */ }

#[cfg(feature = "fs")]
fn do_cat(cmd: &ParsedCommand) { /* ... */ }

// 这些命令在构建命令树时也受条件编译控制
pub fn build_base_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    #[cfg(feature = "fs")]
    tree.insert("ls".to_string(), /* ... */);

    #[cfg(feature = "fs")]
    tree.insert("cat".to_string(), /* ... */);

    // 非文件系统命令始终可用
    tree.insert("uname".to_string(), /* ... */);
    tree.insert("log".to_string(), /* ... */);
}
```

这种设计使得：
1. **Shell界面始终可用** - 提供基本的交互和VM管理能力
2. **文件系统功能可选** - 仅在需要时启用，减少依赖
3. **灵活的部署方式** - 支持从内存或文件系统加载VM

## 快速开始

启动AxVisor后会自动进入Shell界面：
```
Welcome to AxVisor Shell!
Type 'help' to see available commands
Use UP/DOWN arrows to navigate command history

axvisor:/$
```

### 基本操作
- `help` - 查看所有命令
- `help <command>` - 查看特定命令帮助
- `clear` - 清屏
- `exit` - 退出

### 键盘快捷键
- **上/下箭头**: 浏览命令历史
- **左/右箭头**: 移动光标
- **退格键**: 删除字符

## 常用命令

### 文件操作
```bash
ls -la                     # 列出文件（详细信息+隐藏文件）
cat file.txt               # 查看文件内容
mkdir -p dir/subdir        # 创建目录
cp -r source dest          # 复制文件/目录
mv old new                 # 移动/重命名
rm -rf path                # 删除文件/目录
touch file.txt             # 创建空文件
```

### 虚拟机管理
```bash
vm list                    # 列出所有虚拟机
vm list --format json      # JSON格式输出
vm create config.toml      # 创建虚拟机
vm create vm1.toml vm2.toml # 批量创建虚拟机
vm start                   # 启动所有虚拟机
vm start 1                 # 启动VM（ID=1）
vm start -d 1              # 后台启动VM
vm stop -f 1               # 强制停止VM
vm suspend 1               # 暂停VM（ID=1）
vm resume 1                # 恢复暂停的VM
vm restart 1               # 重启VM
vm restart -f 1            # 强制重启VM
vm delete -f 1             # 删除VM(需要确认)
vm status                  # 显示所有VM状态概览（已移除）
vm status 1                # 查看特定VM状态（已移除）
vm show 1                  # 查看VM基本信息
vm show -f 1               # 查看VM完整详细信息
vm show -c 1               # 查看VM配置
vm show -s 1               # 查看VM统计信息
vm show -c -s 1            # 查看VM配置和统计信息
```

### 系统信息
```bash
pwd                        # 当前目录
uname -a                   # 系统信息
```

## 典型工作流

### 单虚拟机场景
```bash
# 1. 检查环境
ls -la
pwd

# 2. 创建虚拟机
vm create linux.toml

# 3. 启动虚拟机
vm start 1

# 4. 监控状态
vm status 1
vm show -c -s 1            # 查看详细配置和统计

# 5. 停止虚拟机
vm stop 1
```

### 多虚拟机场景
```bash
# 1. 批量创建虚拟机
vm create vm1.toml vm2.toml vm3.toml

# 2. 查看所有虚拟机
vm list -a

# 3. 启动所有虚拟机
vm start

# 4. 查看整体状态
vm status                  # 显示所有VM的状态概览

# 5. 停止特定虚拟机
vm stop 2

# 6. 重启虚拟机
vm restart 1

# 7. 删除虚拟机
vm delete -f 3
```

更多详细信息请使用 `help <command>` 查看具体命令的使用方法。
