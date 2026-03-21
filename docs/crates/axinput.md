# `axinput` 技术文档

> 路径：`os/arceos/modules/axinput`
> 类型：库 crate
> 分层：ArceOS 层 / 输入设备接线层
> 版本：`0.3.0-preview.3`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`os/arceos/modules/axruntime/src/lib.rs`、`os/StarryOS/kernel/src/pseudofs/dev/event.rs`、`os/arceos/api/axfeat/Cargo.toml`

`axinput` 的作用是把 `axdriver` 聚合出来的输入设备收集起来，并在合适的时候一次性交给更上层模块。它不是输入驱动本身，也不是完整的 evdev 子系统；它更像是 ArceOS 驱动聚合层和实际输入服务之间的一道“句柄交接层”。

## 1. 架构设计分析
### 1.1 真实定位
当前实现只有一个全局对象：

- `DEVICES: LazyInit<Mutex<Vec<AxInputDevice>>>`

这说明 `axinput` 的设计目标非常明确：

- 不重新定义输入事件格式。
- 不保存额外的设备元数据索引结构。
- 只负责在初始化阶段把 `AxInputDevice` 句柄收集进来。
- 然后在上层真正准备好时把这些句柄整体移交出去。

### 1.2 初始化主线
当前仓库中的调用链如下：

1. `axruntime` 调用 `axdriver::init_drivers()`。
2. `axdriver` 把输入设备收集进 `AllDevices.input`。
3. `axruntime` 在 `feature = "input"` 下调用 `axinput::init_input(all_devices.input)`。
4. `axinput` 循环 `take_one()`，把所有输入设备压入内部 `Vec`。
5. StarryOS 等上层模块再调用 `take_inputs()` 取走整个设备列表。

与 `axdisplay` 最大的区别在于：`axinput` 不只保留一个主设备，而是会收集容器中的所有输入设备。

### 1.3 关键 API
| API | 作用 |
| --- | --- |
| `init_input()` | 从 `AxDeviceContainer<AxInputDevice>` 收集所有输入设备 |
| `take_inputs()` | 一次性取走当前已收集的所有输入设备 |

### 1.4 所有权模型
`take_inputs()` 的实现是 `mem::take(&mut DEVICES.lock())`，这意味着：

- 调用方会直接获得内部 `Vec<AxInputDevice>` 的所有权。
- 全局容器随后被清空。
- 这不是“只读查询”接口，而是“转移设备句柄”的接口。

因此它很适合 StarryOS 这类在更上层重新组织设备节点的场景，但不适合被设计成可反复窥视的全局设备表。

### 1.5 与上下层的边界
| 层次 | 负责内容 | 不负责内容 |
| --- | --- | --- |
| `axdriver_input` / `VirtIoInputDev` | 输入事件、能力位图、设备 ID 的驱动语义 | 全局设备收集与移交 |
| `axinput` | 收集并转移输入设备句柄 | evdev、poll、ioctl、按键状态缓存 |
| StarryOS `pseudofs/dev/event.rs` | 把输入设备包装成事件设备文件 | 底层驱动初始化 |

### 1.6 边界澄清
最关键的边界是：**`axinput` 不是输入子系统本身，而是“把已经探测好的输入设备句柄收好并交给上层”的桥接层。**

## 2. 核心功能说明
### 2.1 主要能力
- 收集所有已探测输入设备。
- 提供一次性移交接口 `take_inputs()`。
- 在日志中记录注册到的输入设备名称和类型。

### 2.2 与 StarryOS 的真实接线关系
StarryOS 的 `pseudofs/dev/event.rs` 会：

- 调用 `axinput::take_inputs()` 获取设备列表；
- 使用 `InputDriverOps::get_event_bits()` 判断其支持按键还是鼠标；
- 再把它们包装为 `eventN` 或 `mice` 设备；
- 后续通过 `read_event()`、`poll()`、`ioctl` 等形成更像 Linux evdev 的行为。

这说明 `axinput` 还处在 evdev 之下，是一次更早的桥接层。

### 2.3 与 ArceOS 顶层 feature 的关系
在 `os/arceos/api/axfeat/Cargo.toml` 中，`input` feature 会同时打开：

- `axdriver/virtio-input`
- `dep:axinput`
- `axruntime/input`

因此它的整机定位是“输入功能 feature 的中间模块”，而不是一个独立发行层 API。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `axdriver` | 提供 `AxInputDevice`、`AxDeviceContainer` 及 `prelude` 类型 |
| `axsync` | 保护全局设备列表 |
| `lazyinit` | 延迟初始化全局容器 |
| `log` | 初始化日志 |

### 3.2 主要消费者
- `os/arceos/modules/axruntime`
- `os/StarryOS/kernel/src/pseudofs/dev/event.rs`
- 启用 `axfeat/input` 的整机构建路径

### 3.3 分层关系总结
- 向下消费已经探测好的输入设备。
- 向上把输入设备交给真正的事件服务层。
- 不承担设备节点或系统调用接口语义。

## 4. 开发指南
### 4.1 适合修改这里的场景
应修改 `axinput` 的情况主要包括：

- 需要调整输入设备全局保存方式。
- 需要改变输入设备从初始化阶段到上层服务阶段的交接策略。
- 需要为更上层输入模块提供新的统一入口。

如果只是修改事件格式、能力位图或底层读取逻辑，应改 `axdriver_input` 或具体驱动实现。

### 4.2 维护时需要特别注意
1. `take_inputs()` 是一次性转移接口，调用顺序非常重要。
2. `init_input()` 使用 `while let Some(...) = take_one()`，因此会消费掉整个容器。
3. 如果未来要支持“多次查询设备列表”，当前 API 需要整体调整，不能只在内部偷偷 clone。

### 4.3 常见坑
- 不要把 `axinput` 写成用户 API；当前仓库里没有 `arceos_api::input` 这样的封装层。
- 不要假设调用 `take_inputs()` 后全局设备仍保留。
- 不要在这里引入按键缓冲、poll 唤醒或 ioctl 语义；这些属于更上层事件设备实现。

## 5. 测试策略
### 5.1 当前有效验证面
当前主要验证路径包括：

- `virtio-input` 设备被 `axdriver` 正确探测。
- `init_input()` 能收集全部输入设备。
- StarryOS `event.rs` 能成功 `take_inputs()` 并生成事件设备。

### 5.2 建议补充的测试
- `init_input()` 在空容器和多设备容器上的行为。
- `take_inputs()` 之后再次调用返回空列表的语义。
- 与 StarryOS 事件设备组装流程的最小集成测试。

### 5.3 风险点
- 由于 `take_inputs()` 会清空全局列表，若初始化顺序不当，设备可能被过早取走。
- 设备列表一旦被错误地多次消费，问题会表现为“输入设备神秘消失”，而不是明显报错。

## 6. 跨项目定位分析
### 6.1 ArceOS
`axinput` 是 ArceOS 本体中的输入桥接模块，服务于启用 `input` feature 的系统构建路径。

### 6.2 StarryOS
这是当前仓库里最明确的上层消费者。StarryOS 直接拿走 `AxInputDevice` 列表，在伪文件系统中组织成接近 Linux 的输入事件设备。

### 6.3 Axvisor
当前仓库中没有看到 Axvisor 直接依赖 `axinput`。它不是 hypervisor 侧输入管理框架。
