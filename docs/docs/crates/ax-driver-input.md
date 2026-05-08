# `axdriver_input` 技术文档

> 路径：`components/axdriver_crates/axdriver_input`
> 类型：库 crate
> 分层：组件层 / 输入设备类别接口层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`components/axdriver_crates/axdriver_virtio/src/input.rs`、`os/arceos/modules/axinput/src/lib.rs`、`os/StarryOS/kernel/src/pseudofs/dev/event.rs`

`axdriver_input` 用来定义输入设备驱动的统一契约。它不是输入事件分发系统，也不是 `/dev/input/event*` 这样的用户可见设备层，而是把“输入设备如何报告支持的事件位图、如何吐出事件、如何暴露设备 ID”这一层接口固定下来，供 `virtio-input` 等具体驱动和 `ax-input` / StarryOS evdev 适配层共同使用。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 的设计明显借鉴了 Linux input 子系统的数据模型：

- `EventType` 定义同步、按键、相对坐标、绝对坐标、开关、LED、声音等事件类别。
- `Event` 使用 `(event_type, code, value)` 三元组描述单条输入事件。
- `InputDeviceId` 和 `AbsInfo` 分别表达设备身份和绝对坐标轴信息。
- `InputDriverOps` 给输入设备统一定义查询能力和取事件的方法。

需要特别指出：源码顶部和 trait 注释里还留有 copy-paste 造成的 “graphics device” 文案，但真实接口语义完全是输入设备，而不是显示设备。

### 1.2 关键对象
| 符号 | 作用 | 备注 |
| --- | --- | --- |
| `EventType` | 输入事件类别枚举 | 通过 `strum::FromRepr` 支持数值转枚举 |
| `Event` | 单条输入事件 | 与 Linux input 事件三元组一致 |
| `InputDeviceId` | 设备身份信息 | 包含 bus/vendor/product/version |
| `AbsInfo` | 绝对轴属性 | 当前更多是契约预留 |
| `InputDriverOps` | 输入设备统一 trait | 是上层读取事件的入口 |

### 1.3 能力模型
`InputDriverOps` 定义的核心方法有：

- `device_id()`：返回设备 ID。
- `physical_location()` / `unique_id()`：提供设备位置和唯一标识字符串。
- `get_event_bits()`：查询某类事件支持的 code 位图。
- `read_event()`：取出一条待处理事件；无事件时返回 `DevError::Again`。

这种接口设计说明它关注的是“驱动如何以轮询方式向上暴露事件”，而不是“系统如何把事件广播给多个消费者”。

### 1.4 与上下层的关系
当前仓库里的典型接线链路如下：

1. `ax_driver_virtio::VirtIoInputDev` 实现 `InputDriverOps`。
2. `ax-driver` 把它包装成 `AxInputDevice` 放进 `AllDevices.input`。
3. `ax-runtime` 调用 `ax_input::init_input(all_devices.input)`。
4. StarryOS 的 `pseudofs/dev/event.rs` 再用 `ax_input::take_inputs()` 取走这些设备，构造 evdev 风格的字符设备。

因此，本 crate 位于“驱动契约”和“系统输入服务”之间，而不是输入服务本身。

### 1.5 边界澄清
最重要的边界是：**`axdriver_input` 只定义输入设备驱动契约，不负责事件队列复用、焦点管理、终端输入分发或 `/dev/input` 节点。**

## 2. 核心功能说明
### 2.1 主要能力
- 统一输入事件和事件类型表示。
- 统一输入设备 ID 和能力位图查询接口。
- 统一“无事件时返回 `DevError::Again`”的轮询约定。
- 让不同输入设备能被 `ax-driver`、`ax-input` 和 StarryOS evdev 层共同消费。

### 2.2 当前 VirtIO 路径的实现方式
`ax_driver_virtio::VirtIoInputDev` 是当前仓库里的主要实现：

- `try_new()` 从底层设备读取名称和 `ids()`。
- `get_event_bits()` 通过 `query_config_select(InputConfigSelect::EvBits, ...)` 读取能力位图。
- `read_event()` 调用 `pop_pending_event()`，并在无事件时返回 `DevError::Again`。

这条实现主线也印证了本 crate 的设计：它要求驱动暴露事件能力和取事件接口，但不内建共享缓冲或广播语义。

### 2.3 当前实现特征
- crate 本身没有 feature 开关，保持纯接口层。
- `EventType::bits_count()` 为不同事件类别预设了位图长度上限，供上层按类别申请输出缓冲区。
- `AbsInfo` 已定义但当前仓库主要消费点仍集中在 `Event` 和 `get_event_bits()`。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-driver-base` | 基础设备信息和错误模型 |
| `strum` | 为 `EventType` 提供 `FromRepr` |

### 3.2 主要消费者
- `components/axdriver_crates/axdriver_virtio`
- `os/arceos/modules/axdriver`
- `os/arceos/modules/axinput`
- `os/StarryOS/kernel/src/pseudofs/dev/event.rs`

### 3.3 分层关系总结
- 向下不依赖任何总线或设备实现。
- 向上为输入驱动暴露统一事件契约。
- 更上层的 `ax-input` 与 StarryOS evdev 适配负责把这些事件变成系统可消费能力。

## 4. 开发指南
### 4.1 何时修改这里
应在以下场景修改 `axdriver_input`：

- 输入设备类别需要新增真正通用的元信息或操作。
- 事件模型需要与上层 evdev 风格接口保持一致演进。
- `EventType` 或位图长度契约需要扩展。

如果只是新增某个具体输入设备驱动，通常应在外部实现 `InputDriverOps`，而不是改这里。

### 4.2 实现新驱动时的建议
1. `read_event()` 无事件时应返回 `DevError::Again`，不要把“暂时没数据”误写成 `Io`。
2. `get_event_bits()` 应清晰区分“不支持该事件类型”和“查询失败”。
3. `device_id()`、`physical_location()`、`unique_id()` 最好保持稳定，便于上层做设备识别。
4. 若驱动支持绝对轴，后续应考虑把 `AbsInfo` 相关能力补齐，而不是另起私有接口。

### 4.3 常见坑
- 不要把输入事件缓冲、poll/wakeup 机制塞进本 crate；那属于更上层。
- 不要把 `get_event_bits()` 的 `false` 和 `Err(...)` 混为一谈。
- 不要假设上层一定会持续持有设备；当前 `ax-input` 采用“收集后一次性交付”的模型。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录，当前主要通过以下整机路径验证：

- `virtio-input` 驱动是否能正确返回事件位图和事件流。
- `ax-input` 是否能收集设备。
- StarryOS 的 evdev 伪设备是否能基于这些接口工作。

### 5.2 建议补充的单元测试
- `EventType::bits_count()` 的边界值。
- `FromRepr` 与枚举值映射。
- mock 输入设备上 `get_event_bits()` / `read_event()` 的契约测试。

### 5.3 集成测试重点
- QEMU `virtio-input` 键盘/鼠标事件通路。
- StarryOS `event.rs` 的 `ioctl`、`read_at()` 和 `poll()` 路径。
- 多输入设备并存时的能力位图和设备名回传。

### 5.4 风险点
- 事件位图长度或编码一旦不一致，会在上层表现为“设备存在但功能位识别错误”。
- 若把无事件错误映射错，会让轮询或伪文件系统阻塞语义出现异常。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 通过 `ax-driver` 和 `ax-input` 直接消费它，是当前仓库中的主线使用场景。

### 6.2 StarryOS
StarryOS 在 `pseudofs/dev/event.rs` 中直接使用 `InputDriverOps`、`EventType`、`InputDeviceId` 等符号，把它变成 evdev 风格输入设备，因此是更贴近上层能力的一侧消费者。

### 6.3 Axvisor
当前仓库里没有看到 Axvisor 直接以 `axdriver_input` 为核心接口。它不是虚拟化输入分发框架，也不是通用的宿主输入抽象层。
