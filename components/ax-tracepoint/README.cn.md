# ax-tracepoint

面向内核场景的 Rust tracepoint 库，设计目标类似 Linux tracepoint：

- 用宏定义事件与字段
- 运行时按唯一事件 ID 管理
- 支持开关、过滤表达式、回调
- 支持原始事件缓冲区与可读文本输出
- no_std 可用

## 核心能力

- 事件定义：通过 define_event_trace! 一次性生成事件元数据、调用函数、注册函数
- 事件管理：TracePointMap 按 tracepoint ID 索引
- 事件控制：enable/disable、format/id/filter
- 过滤表达式：基于 tp-lexer 按 schema 编译并匹配
- 输出链路：TracePipeRaw + TraceEntryParser

## 快速接入

### 1. 添加依赖

```toml
[dependencies]
ax-tracepoint = "*"
```

### 2. 链接脚本中保留 .tracepoint 段

该库通过 __start_tracepoint / __stop_tracepoint 扫描所有事件元数据。
请将 my_section.ld 的内容并入你的链接脚本，确保 .tracepoint 段被 KEEP。
StarryOS 的内核 linker script 已包含该契约；其它宿主需要自行加入等价段定义。

### 3. 实现 KernelTraceOps

你需要提供：

- current_pid
- trace_pipe_push_raw_record
- trace_cmdline_push
- tracepoint state registry 钩子：read_tracepoint_state、write_tracepoint_state

state registry 钩子用于让你的 OS 自行选择 callbacks 和 filters 的同步策略。
tracepoint 调用使用运行时原子 callback gate，动态注册不会在其它 CPU
运行时修改可执行代码。
writer 负责发布顺序：先发布非空 callback 状态再打开 gate；移除最后一个
callback 时先关闭 gate，再退役旧状态。

### Callback 限制

`read_tracepoint_state` 可以在 tracing 快路径执行 callbacks 时持有读侧锁。如果你的实现使用 `RwLock` 这类不可重入锁，callback 中不得：

- 注册或注销 tracepoint callback
- 更新 tracepoint filter
- 调用其它需要 `write_tracepoint_state` 的 API
- 递归触发由同一个 state registry 支撑的 tracepoint

违反这些规则可能导致死锁。如果宿主使用 RCU、snapshot 或其它非阻塞读侧机制实现 `read_tracepoint_state`，可以自行放宽这些限制。

### 4. 定义并调用事件

```rust
use ax_tracepoint::{define_event_trace, KernelTraceOps};

define_event_trace!(
    TEST,
    TP_kops(Kops),
    TP_system(tracepoint_test),
    TP_PROTO(a: u32, b: u32),
    TP_STRUCT__entry { a: u32, b: u32 },
    TP_fast_assign { a: a, b: b },
    TP_ident(__entry),
    TP_printk(format_args!("a={}, b={}", __entry.a, __entry.b))
);

// 生成函数: trace_TEST / register_trace_TEST / unregister_trace_TEST
trace_TEST(1, 2);
```

提示：TP_STRUCT__entry 会参与字节布局。字段必须实现 TraceField；内置实现覆盖整数
原语及 trace field 数组。

### 5. 初始化管理器

```rust
use ax_tracepoint::global_init_events;

let (tracepoints, ext_tracepoints) = global_init_events::<Kops>()?;

// 将 ext_tracepoints 安装到 Kops::read_tracepoint_state 和
// Kops::write_tracepoint_state 使用的 registry 中。
```

### 6. 启用、过滤、消费输出

```rust
use ax_tracepoint::{TraceFilterFile, TracePointEnableFile, TracePointFormatFile, TracePointIdFile};

let event_id = 0;
Kops::write_tracepoint_state(event_id, |event| {
    TracePointEnableFile::new().write(event, '1');
    let mut filter = TraceFilterFile::new();
    filter.write(event, "a > 8 && b > 5").unwrap();
});

// 读取格式描述
let tracepoint = tracepoints.get(&event_id).unwrap();
let fmt = TracePointFormatFile::new(tracepoint).read();
let id = TracePointIdFile::new(tracepoint).read();
```

## 运行示例

```bash
cargo run -p ax-tracepoint --example usage
```

示例代码位于 examples/usage.rs，覆盖了：

- 事件定义与触发
- 事件启用与过滤
- 注册 event/raw 回调
- TracePipeRaw 快照读取与文本解析

## 主要公开类型

- KernelTraceOps
- TracePoint / ExtTracePoint / TracePointMap
- TracePipeRaw / TracePipeSnapshot / TracePipeOps
- TraceCmdLineCache / TraceEntryParser
- TraceFilterError / TraceParseError / TraceInitError

损坏的 trace record 和被拒绝的 filter 更新会返回 typed error。filter 编译失败会保留
上一个已生效的 compiled filter；只有精确写入 `0`（允许外围空白）才会清除 filter。

## 参考项目

- DragonOS: <https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/debug/tracing/mod.rs>
- TGOSKits StarryOS: <https://github.com/rcore-os/tgoskits/tree/dev/os/StarryOS>
