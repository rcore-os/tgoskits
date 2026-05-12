---
sidebar_position: 3
sidebar_label: "平台实现"
---

# `.vscode` 平台实现

本文档按文件说明当前调试方案在 `.vscode` 目录中的实现方式。  
重点不是介绍“如何点击调试”，而是解释这几个文件各自负责什么、为什么要这样拆，以及它们如何共同完成一次完整的本地调试。

当前实现主要由三个文件组成：

- `.vscode/launch.json`
- `.vscode/tasks.json`
- `.vscode/session.py`

## 设计分层

这三个文件的职责边界是刻意分开的：

- `launch.json` 负责“调试器视角”的配置
- `tasks.json` 负责“任务编排视角”的配置
- `session.py` 负责“会话管理视角”的实现

这样分层的原因是：

- VS Code 的调试配置更适合表达“附加到哪里、用什么方式附加、起始断点在哪里”
- VS Code 的任务系统更适合表达“构建和启动的顺序关系”
- QEMU debug 会话的等待、输出接管、状态管理、退出清理更适合落在脚本里实现

如果把这些逻辑全部塞进单一层里，调试入口会更难维护，也更难处理 Linux / Windows 的差异。

## `launch.json`

`launch.json` 是 VS Code 调试入口的最上层描述。当前每个系统都提供 `Main` 和 `Boot` 两类配置。

### 核心字段

当前预置配置统一采用：

```json
"type": "lldb",
"request": "custom"
```

这意味着当前调试器使用的是 CodeLLDB，并且通过自定义命令流完成目标创建和远程附加。

### `initCommands`：信号处理与步进控制

每个配置都包含 `initCommands`，在调试器附加之前执行。所有配置共享的基础设置是：

```json
"initCommands": [
  "process handle SIGINT -p false -s false -n false"
]
```

这条命令告诉 LLDB **不捕获、不停止、不通知** `SIGINT` 信号。原因是：在 QEMU + GDB stub 场景下，`SIGINT` 需要透传到被调试目标（例如让内核正确响应 Ctrl+C 中断），而不是被 LLDB 拦截后暂停目标进程。

**Axvisor 配置额外包含一条步进过滤规则**：

```json
"settings set target.process.thread.step-avoid-regexp ^(core::|alloc::|bitflags::|ax_page_table_entry::|page_table_multiarch::)"
```

这条规则让 LLDB 在单步执行（step over / step into）时**自动跳过**匹配的 crate 路径。Axvisor 作为 hypervisor，其执行路径会频繁穿过 `core::`（Rust 核心库）、`alloc::`（全局分配器）、`bitflags::`（位标志宏展开）以及页表操作 crate。如果不做步进过滤，开发者按一次 F10 可能会陷入数十个无关帧才回到业务代码。ArceOS 和 StarryOS 当前未启用此规则——它们的调用深度和 crate 依赖模式使得默认步进行为已经可用。

### `sourceLanguages` 字段

所有配置均声明：

```json
"sourceLanguages": ["rust"]
```

此字段帮助 CodeLLDB 优先使用 Rust 源码级别符号进行断点解析和堆栈展示。如果省略此项，LLDB 在某些混合二进制场景下可能退化为纯地址/反汇编视图，降低调试效率。

### 调试前后任务

`launch.json` 不直接负责构建或启动 QEMU，而是通过任务名把这件事交给 `tasks.json`：

```json
"preLaunchTask": "TGOS: Prepare Axvisor QEMU debug",
"postDebugTask": "TGOS: Stop Axvisor QEMU debug"
```

这里体现了第一层职责划分：

- `launch.json` 只声明“调试前必须准备好什么”
- 真正的准备流程在 `tasks.json`

### 调试目标与附加方式

每个配置都会显式指定调试目标路径，例如：

```json
"targetCreateCommands": [
  "target create ${workspaceFolder}/target/aarch64-unknown-none-softfloat/debug/axvisor",
  "target modules load --file ${workspaceFolder}/target/aarch64-unknown-none-softfloat/debug/axvisor --slide 0"
]
```

随后通过：

```json
"processCreateCommands": [
  "gdb-remote 127.0.0.1:1234"
]
```

附加到 QEMU 暴露出来的 GDB stub。

这里说明 `launch.json` 只假设两件事已经成立：

1. 对应的 debug 二进制已经构建出来
2. `127.0.0.1:1234` 已经可连接

而这两件事都不是 `launch.json` 自己保证的，而是由 `tasks.json` 和 `session.py` 提前完成。

### `Main` 与 `Boot` 的区别

`launch.json` 中 `Main` / `Boot` 的差异主要体现在 `postRunCommands` 上。

例如：

- `Main` 更偏向应用或主路径断点
- `Boot` 更偏向平台入口、runtime 初始化、早期引导断点

也就是说，`launch.json` 的价值不只是“能附加”，还负责把不同问题类型映射到不同的断点入口。
#### 各系统具体断点位置

| 配置 | 断点策略 | 典型命中位置 |
|------|---------|-------------|
| ArceOS Main | 单个软件断点 + `continue` | `os/arceos/examples/helloworld/src/main.rs:8` |
| ArceOS Boot | 多个符号/行号断点（不自动 continue） | `ax_plat::call_main`、`axruntime/src/lib.rs:141`、`main.rs:8` |
| Axvisor Main | 单个软件断点 + `continue` | `os/axvisor/src/main.rs:42` |
| Axvisor Boot | 多个行号断点（不自动 continue） | `platform/axplat-dyn/src/boot.rs:8`、`axvisor/src/main.rs:42` |
| StarryOS Main | **单个硬件断点** + `continue` | `os/StarryOS/starryos/src/main.rs:12` |
| StarryOS Boot | 混合符号/行号断点（不自动 continue） | `ax_plat::call_main`、`axruntime/src/lib.rs:141`、`starry_kernel::entry::init`、`starryos/src/main.rs:12` |

#### StarryOS 硬件断点

StarryOS Main 配置使用 `--hardware true`：

```json
"breakpoint set --hardware true --file ... --line 12"
```

这是因为 StarryOS 在早期引导阶段可能运行在内存权限受限的页面布局上，软件断点（通过写入 `0xCC` / `0xE7FFFFFF` trap 指令实现）不一定能成功写入目标代码页。硬件断点使用 CPU 的调试寄存器（DR0-DR3 on x86, HWBP on AArch64），不需要修改代码内存，因此在任何内存布局下都能可靠命中。ArceOS 和 Axvisor 当前未启用硬件断点——它们的引导阶段内存布局允许软件断点正常工作。
## `tasks.json`

`tasks.json` 负责把一次完整调试拆成可维护的任务链，而不是依赖单个巨大命令。

### 三段式任务链

每个系统当前都拆成三层任务：

1. `Build ... debug image`
2. `Start ... QEMU debug`
3. `Prepare ... QEMU debug`

例如 ArceOS：

```json
{
  "label": "TGOS: Build ArceOS debug image",
  "command": "cargo",
  "args": ["xtask", "arceos", "build", "--debug", "--package", "ax-helloworld", "--arch", "aarch64"]
}
```

```json
{
  "label": "TGOS: Start ArceOS QEMU debug",
  "command": "python",
  "args": ["${workspaceFolder}/.vscode/session.py", "start"]
}
```

```json
{
  "label": "TGOS: Prepare ArceOS QEMU debug",
  "dependsOrder": "sequence",
  "dependsOn": [
    "TGOS: Build ArceOS debug image",
    "TGOS: Start ArceOS QEMU debug"
  ]
}
```

### 为什么要显式拆成 `Build -> Start -> Prepare`

核心原因是首次冷编译可能非常慢。

如果把“构建 + 启动 QEMU + 等待 GDB stub”揉在同一个后台任务里，VS Code 在某些情况下会过早进入调试附加阶段，导致：

- 目标二进制还没准备好
- GDB stub 还没打开
- `target create` 或 `gdb-remote` 阶段失败

现在把构建显式拆开后，`Prepare` 任务通过顺序依赖保证：

1. 先构建二进制
2. 再启动 QEMU debug 会话
3. 最后才允许 `launch.json` 进入附加阶段

### 背景任务与完成信号

`Start ... QEMU debug` 当前被标记为后台任务：

```json
"isBackground": true
```

是否可以结束等待，由 `problemMatcher.background` 决定：

```json
"beginsPattern": "^QEMU_DEBUG_STARTING session=axvisor\\b.*$",
"endsPattern": "^QEMU_GDB_READY session=axvisor\\b.*$"
```

这意味着 `tasks.json` 本身不直接理解 QEMU 是否就绪，而是依赖 `session.py` 输出的状态信号。

这也是 `tasks.json` 与 `session.py` 的关键接口。

### 失败匹配与问题报告

每个 `Start ... QEMU debug` 任务还配置了前台 `problemMatcher`：

```json
"problemMatcher": {
  "owner": "axvisor-qemu",
  "pattern": {
    "regexp": "^(QEMU_DEBUG_FAILED).*$",
    "message": 1
  },
  "background": { ... }
}
```

当 `session.py` 输出包含 `QEMU_DEBUG_FAILED` 时，VS Code 会将其作为任务错误捕获并在 Problems 面板中展示。注意：此正则**只匹配失败状态**——`QEMU_DEBUG_STARTING` 和 `QEMU_GDB_READY` 的识别完全由 `background` 子段处理，两者职责分离。

如果 `endsPattern`（即 `QEMU_GDB_READY`）在超时内始终未出现（默认 20 秒，由 `session.py` 控制），VS Code 会将后台任务标记为超时完成，但 `launch.json` 的附加步骤仍会尝试执行并很可能因连接失败而报错。此时应检查 `target/qemu-debug/*.log` 确认 QEMU 是否正常启动。

### 任务可见性与面板行为

所有调试相关任务均设置 `"hide": true`，这意味着它们**不会出现在命令面板的任务列表中**。开发者通过 `F5` 触发调试时，VS Code 按 `preLaunchTask` 引用自动执行这些隐藏任务，无需手动选择。

统一的 `presentation` 配置为：

```json
{
  "close": true,      // 任务完成后可关闭终端（不锁定面板）
  "echo": false,      // 不回显执行的命令本身（减少噪音）
  "focus": false,     // 不抢夺焦点（保持编辑器焦点）
  "panel": "dedicated", // 每个系统使用独立终端面板（避免输出混杂）
  "reveal": "always"   // 始终展示终端面板（确保构建/启动过程可见）
}
```

`"panel": "dedicated"` 是关键设计点：ArceOS、Axvisor、StarryOS 各自的构建和 QEMU 输出分别显示在不同终端标签页中，方便开发者并行对比或独立查看某个系统的输出流。Stop 任务使用 `"panel": "shared"` 并 `"reveal": "never"`，因为清理操作不需要抢占用户注意力。

## `session.py`

`session.py` 是当前调试方案里最接近“运行时控制器”的一层。

### 它解决的问题

相较于 `launch.json` 和 `tasks.json`，`session.py` 主要负责：

- 启动前清理旧会话
- 启动 cargo / QEMU 命令
- 接管标准输入输出
- 轮询 GDB stub 是否就绪
- 输出统一的状态信号
- 调试结束后清理进程

因此它不是一个简单的“命令转发脚本”，而是整条调试链路的会话管理器。

### 环境变量接口

`session.py` 通过环境变量接收任务层传入的参数，包括：

- `TGOS_DEBUG_COMMAND`
- `TGOS_DEBUG_PORT`
- `TGOS_DEBUG_SESSION`
- `TGOS_DEBUG_STATE_DIR`
- `TGOS_DEBUG_TEE_OUTPUT`

例如 `tasks.json` 中的：

```json
"env": {
  "TGOS_DEBUG_COMMAND": "cargo xtask axvisor qemu --debug --config os/axvisor/.build.toml",
  "TGOS_DEBUG_PORT": "1234",
  "TGOS_DEBUG_SESSION": "axvisor",
  "TGOS_DEBUG_STATE_DIR": "${workspaceFolder}/target/qemu-debug"
}
```

会在 `session.py` 里变成当前会话的运行上下文。

**`TGOS_DEBUG_TEE_OUTPUT`**（默认值 `"1"`）控制是否将 QEMU 的标准输出同时镜像到 VS Code 终端。设为 `"0"` 时输出仅写入日志文件，不在终端显示。当前 `tasks.json` 中未显式设置此变量，因此始终启用终端镜像。此变量的存在是为了预留"静默运行"模式，适用于 CI 或自动化场景。

### 会话状态信号

`session.py` 统一输出以下状态：

- `QEMU_DEBUG_STARTING`
- `QEMU_GDB_READY`
- `QEMU_DEBUG_FAILED`
- `QEMU_DEBUG_STOPPED`

这些输出既用于终端显示，也作为 `tasks.json` 的后台任务匹配信号。

换句话说，`session.py` 不只是“记录日志”，它还直接决定了 VS Code 什么时候认为调试前置条件已经满足。

### 就绪判断

当前就绪判断不是简单地“睡几秒”，而是轮询检查。

核心逻辑在 `_wait_for_qemu_ready(...)` 中。

Linux 下优先使用更严格的组合判断：

- `_has_qemu_in_group(...)`
- `_port_owned_by_group(...)`
- `_tcp_connectable()`

Windows 或无 `/proc` 的环境下则退化为端口可连判断。

这解释了为什么平台差异最终会集中体现在 `session.py` 里，而不是 `launch.json` 或 `tasks.json` 中。
### 平台能力检测

`session.py` 在模块加载时一次性检测当前平台的各项能力，后续所有分支都基于这些检测结果做静态分派：

```python
_has_procfs            = _proc_root.is_dir()          # Linux /proc 文件系统可用
_has_pty               = pty is not None              # pty 模块可用（Unix）
_has_process_groups    = hasattr(os, "getpgid")        # 进程组 API 可用
_taskkill              = shutil.which("taskkill")      # Windows taskkill 命令
_new_process_group_flag = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)  # Windows 进程组标志
```

这种"启动时检测 + 后续条件分支"的模式避免了运行时反复探测开销，也使得新增平台支持时只需扩展检测列表而无需改动核心逻辑。
### 输出接管

`session.py` 同时承担“输出策略选择器”的角色。

Linux 下如果有 PTY，则优先走：

```python
if _tee and _has_pty:
```

通过 `pty.openpty()` 让 cargo / QEMU 把自己视为运行在真实终端中，再从 PTY 主端读取并转写到：

- VS Code 集成终端
- `target/qemu-debug/*.log`

如果没有 PTY，则退回 pipe 分支：

```python
stdout_target = subprocess.PIPE if _tee else log_fh
```

并通过 tee 线程把 `proc.stdout` 中的输出回写到终端和日志。

**PTY 模式的必要性**：当 stdout 被重定向到 pipe 时，大多数程序（包括 QEMU 和 cargo）会切换到**全缓冲模式**（缓冲区约 8 KB），导致输出长时间积压不显示。分配 PTY 后，程序认为自己连接到真实终端，自动使用**行缓冲模式**，输出可以实时逐行显示。这是 Linux 下调试体验"流畅"的关键技术原因之一。

因此，`session.py` 也是当前平台行为差异最集中的文件。

### Linux `/proc` 辅助函数

`session.py` 在 Linux 下使用一组 `/proc` 文件系统辅助函数来实现精确的进程识别和端口归属判断。这些函数在 Windows 上均安全降级（返回空列表或 `None`），不会导致错误。

| 函数 | 作用 | 使用场景 |
|------|------|---------|
| `_read_bytes(path)` | 读取 `/proc` 下任意文件的原始字节 | 所有其他 `/proc` 函数的基础设施 |
| `_all_pids()` | 枚举系统中所有数字 PID | 进程遍历的基础 |
| `_pgid_of(pid)` | 通过 `os.getpgid()` 获取进程组 ID | 判断进程是否属于目标进程组 |
| `_exe_basename(pid)` | 从 `/proc/<pid>/cmdline` 提取 `argv[0]` 的文件名 | 识别 QEMU 进程 |
| `_proc_env(pid)` | 解析 `/proc/<pid>/environ` 为键值对字典 | 会话归属判定 |

### 会话归属检测

`_belongs_to_session(pid)` 通过检查进程的环境变量来判断该进程是否属于当前调试会话：

```python
def _belongs_to_session(pid: int) -> bool:
    env = _proc_env(pid)
    return (
        env.get("TGOS_DEBUG_SESSION") == _session
        and env.get("TGOS_DEBUG_STATE_DIR") == _state_dir_str
    )
```

`session.py` 在启动子进程时通过 `shell=True` 执行命令，环境变量会自动继承到子进程及其所有后代。这意味着**整个 QEMU 进程树中的每个进程都携带会话标记**。此机制用于：

- **孤儿进程回收**：即使进程组的领导进程已退出，仍可通过环境标记找到属于本会话的残留进程
- **跨会话隔离**：防止不同调试会话（如同时存在的 arceos 和 axvisor）之间的进程互相干扰

### 端口归属验证

`_port_owned_by_group(pgid)` 是就绪判断中最复杂的环节。它不只检查"1234 端口是否可连"，而是验证**持有该端口的 socket 是否确实属于当前调试会话的进程组**。实现步骤如下：

1. **解析 `/proc/net/tcp` 和 `/proc/net/tcp6`**：提取所有处于 `TCP_LISTEN` 状态（内核状态码 `0A`）的套接字条目
2. **按端口号过滤**：将端口号转为十六进制（如 `1234` → `"04D2"`）与各条目的本地地址字段匹配
3. **收集 socket inode**：从匹配条目的第 10 个字段读取 socket inode 号
4. **遍历进程组内进程的 fd**：通过 `/proc/<pid>/fd/` 下的符号链接（格式 `socket:[<inode>]`）确认哪个进程持有这些 inode

这种三层验证（QEMU 进程存在 → 端口由该进程组的 socket 持有 → TCP 可连）有效避免了以下误判场景：
- 上一次调试残留的僵尸进程占用了 1234 端口
- 其他程序巧合监听了同一端口
- QEMU 已启动但 GDB stub 尚未完成绑定

### 超时参数

`session.py` 中各环节的超时参数经过调优以平衡"等待充分"和"快速失败"：

| 参数 | 默认值 | 位置 | 说明 |
|------|--------|------|------|
| QEMU 启动超时 | **20 秒** | `_wait_for_qemu_ready()` | 从 QEMU 启动到 GDB stub 可连接的最大等待时间 |
| 轮询间隔 | **0.1 秒** | `_wait_for_qemu_ready()` | 每次 TCP 探测之间的间隔，平衡响应速度与 CPU 开销 |
| Socket 连接超时 | **0.2 秒** | `_tcp_connectable()` | 单次 TCP connect 的阻塞上限 |
| 孤儿进程清理宽限 | **2 秒** | `_kill_orphans()` | SIGTERM 后等待 graceful exit 的时间 |
| tee 线程 join 超时 | **2 秒** | `_cmd_start()` | QEMU 退出后等待 tee 线程结束的时间 |
| stdin 线程 join 超时 | **0.2 秒** | `_cmd_start()` | 等待 stdin 转发线程退出的时间 |

如果 QEMU 在 20 秒内未能使 GDB stub 就绪，`session.py` 会输出 `QEMU_DEBUG_FAILED` 并打印日志最后 80 行作为诊断信息，然后以退出码 1 退出。

### 信号处理与 stdin 转发

`session.py` 在 `_cmd_start()` 中注册了信号处理器：

```python
signal.signal(signal.SIGINT, _on_signal)
signal.signal(signal.SIGTERM, _on_signal)
```

当用户按下 Ctrl+C 或 VS Code 发送终止请求时：
1. 调用 `_cleanup()` 清理所有跟踪的进程组和孤儿进程
2. 以退出码 1 退出

同时，`_start_stdin_forwarder()` 启动一个守护线程将 VS Code 终端的用户输入转发到 QEMU 子进程的 stdin。这使得开发者可以在调试运行期间直接在终端中与被调试系统交互（例如输入 shell 命令）。PTY 模式下输入写入 master fd，pipe 模式下写入 `proc.stdin`。

### 孤儿进程清理机制

`_kill_orphans()` 实现了"两阶段强制回收"策略：

1. **第一阶段**：对所有识别为"属于当前会话但不属于当前进程组"的进程组发送 `SIGTERM`
2. **等待期**：最多等待 2 秒让进程优雅退出
3. **第二阶段**：对仍未退出的进程组发送 `SIGKILL` 强制终止

孤儿进程的产生场景包括：
- QEMU 主进程异常退出但子进程（如 `-serial` 管道的管道进程）仍在运行
- 上一次调试会话的非正常终止（VS Code 崩溃、网络断开等）
- 进程组领导进程退出后，子进程被 init 收养但仍持有 GDB stub 端口

Windows 平台不使用孤儿扫描机制——Windows 下仅依赖 `taskkill /T /F` 按 PID 树执行强制终止。

### 退出码约定

`session.py` 使用以下退出码与调用方（`tasks.json` / VS Code）通信：

| 退出码 | 含义 | 触发条件 |
|--------|------|---------|
| **0** | 成功 | QEMU 正常退出，调试会话完整结束 |
| **1** | 失败 | QEMU 启动超时、GDB stub 未就绪、或收到 SIGINT/SIGTERM |
| **2** | 参数错误 | 缺少必需的环境变量（`TGOS_DEBUG_STATE_DIR`、`TGOS_DEBUG_SESSION`、`TGOS_DEBUG_COMMAND`） |

### 会话清理

`session.py` 会维护以下状态文件：

- `<session>.pid`
- `<session>.pgid`
- `<session>.log`

并在 `stop` 或异常退出时清理相关进程。

Linux 下主要依赖：

- 进程组信号（`kill(-pgid, SIGTERM)`）
- `/proc` 文件系统遍历
- 孤儿进程扫描与两阶段回收

Windows 下则主要依赖：

- `CREATE_NEW_PROCESS_GROUP` 创建独立进程组
- `taskkill /T /F` 递归强制终止进程树

所以，虽然两个平台对 VS Code 暴露的是同一套入口，但底层的会话回收手段是不同的。

**Linux 清理路径**：优先通过 PGID 向整个进程组发送 `SIGTERM`。如果 `.pgid` 文件丢失（异常删除），退回使用 `.pid` 文件向单个进程发信号。之后执行孤儿扫描（`_kill_orphans()`），清理所有携带本会话环境标记但不属于当前进程组的残留进程。

**Windows 清理路径**：由于没有进程组 API（`os.getpgid` 不可用），仅使用 `.pid` 文件定位主进程，然后调用 `_kill_pid_tree()` 执行 `taskkill /PID <pid> /T /F`。`/T` 标志递归终止子进程，`/F` 标志强制终止（跳过优雅关闭确认）。

**状态文件生命周期**：

| 文件 | 写入时机 | 删除时机 |
|------|---------|---------|
| `<session>.pid` | QEMU 启动后立即写入 | 正常退出或 stop 时删除 |
| `<session>.pgid` | QEMU 启动后立即写入 | 正常退出或 stop 时删除 |
| `<session>.log` | 启动后持续追加 | **保留不删除**（供事后诊断） |

日志文件不会被自动清理或轮转——它们在 `target/qemu-debug/` 中累积，用于跨会话对比和问题复现。如果日志占用过多磁盘空间，可手动清空该目录。

## 三个文件之间的调用关系

从调用关系上看，这三层可以概括为：

1. `launch.json` 决定“何时需要准备调试”
2. `tasks.json` 决定“准备调试的顺序是什么”
3. `session.py` 决定“调试会话如何真正启动与结束”

因此：

- `launch.json` 更接近“调试器入口描述”
- `tasks.json` 更接近“流程调度层”
- `session.py` 更接近“平台会话控制层”

这也是当前调试设计的核心实现结构。它既保证了 VS Code 入口稳定，又把复杂的平台差异压缩到了脚本层，而没有把它们散落到每个调试配置里。
