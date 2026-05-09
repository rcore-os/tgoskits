# `ax-driver-base` 技术文档

> 路径：`components/axdriver_crates/axdriver_base`
> 类型：库 crate
> 分层：组件层 / 驱动共性契约层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`os/arceos/modules/axdriver/src/prelude.rs`

`ax-driver-base` 是整个 `axdriver_crates` 体系的最小公共基座。它不负责探测设备、枚举总线、管理 DMA，也不组织 `AllDevices` 这样的设备聚合对象；它只把所有驱动都必须共享的设备分类、错误模型和基础元信息接口集中定义出来，供 `axdriver_block`、`axdriver_net`、`axdriver_display`、`axdriver_input`、`axdriver_vsock` 以及上层 `ax-driver` 聚合层复用。

## 1. 架构设计分析
### 1.1 设计定位
`ax-driver-base` 的职责非常克制，核心只有三件事：

- 用 `DeviceType` 给驱动实例贴上统一类别标签。
- 用 `DevError` / `DevResult` 统一设备操作失败语义。
- 用 `BaseDriverOps` 规定所有设备都必须暴露的最小信息面。

这意味着它在驱动栈中的位置很明确：

- 它不是 `ax-driver` 那样的驱动聚合层。
- 它不是 `ax-driver-pci` 或 `ax-driver-virtio` 那样的总线/传输层。
- 它也不是 `axdriver_block`、`axdriver_net` 这类具体设备类别层。

它解决的问题只有一个：**让不同类别驱动至少拥有一致的“名字、类别、错误类型和可选 IRQ”表达方式。**

### 1.2 关键对象
| 符号 | 作用 | 在体系中的位置 |
| --- | --- | --- |
| `DeviceType` | 统一设备类别枚举 | 被 `ax-driver::AxDeviceEnum`、`AllDevices` 和日志输出使用 |
| `DevError` | 统一设备错误枚举 | 被所有 `*DriverOps` 实现共享 |
| `DevResult<T>` | 设备操作统一返回类型 | `Result<T, DevError>` 的别名 |
| `BaseDriverOps` | 所有设备的最小公共 trait | 是各类别 `*DriverOps` 的共同父接口 |

`DeviceType` 当前覆盖 `Block`、`Char`、`Net`、`Display`、`Input`、`Vsock` 六类。其中 `Char` 在当前 `ax-driver` 聚合层里还没有对应容器字段，这也说明 `ax-driver-base` 的枚举范围略大于当前 ArceOS 运行时实际接入面。

### 1.3 接口约束
`BaseDriverOps` 的接口极小：

- `device_name()`：提供用于日志、调试和上层识别的设备名。
- `device_type()`：把实例绑定到统一类别。
- `irq_num()`：默认为 `None`，只有需要向上暴露 IRQ 号的驱动才覆写。

这种设计有两个直接后果：

1. 各类别 crate 可以在自己的 trait 中追加能力，而不必重复定义名称和类别接口。
2. 上层聚合层和日志系统可以只依赖 `BaseDriverOps` 做通用处理，而不用知道具体是块设备、网卡还是输入设备。

### 1.4 与相邻层的边界
| 层次 | 负责内容 | 不负责内容 |
| --- | --- | --- |
| `ax-driver-base` | 设备类别、错误模型、最小元信息接口 | 设备探测、总线枚举、DMA、具体读写协议 |
| `axdriver_block`/`net`/`display`/`input`/`vsock` | 各设备类别专属 trait 与少量实现 | 全局设备聚合、系统初始化时序 |
| `ax-driver-pci` / `ax-driver-virtio` | 总线访问、传输探测和设备包装 | 跨类别统一错误模型定义 |
| `ax-driver` | 设备探测、分类、聚合、向上交付 `AllDevices` | 重新定义基础错误与元信息接口 |

这里最关键的边界澄清是：**`ax-driver-base` 只是公共契约层，不是驱动管理器。**

## 2. 核心功能说明
### 2.1 主要能力
- 为所有驱动提供统一的 `DeviceType`。
- 为所有驱动提供统一的 `DevError` / `DevResult`。
- 为各类别 trait 提供共同父接口 `BaseDriverOps`。
- 让上层 `ax-driver::prelude` 可以用一套统一名字重导出基础类型。

### 2.2 典型使用方式
在各类别 crate 中，常见模式是：

1. 先实现 `BaseDriverOps`。
2. 再实现本类别的专属 trait，例如 `BlockDriverOps` 或 `NetDriverOps`。
3. 最终由 `ax-driver` 聚合层把实例包装进 `AxDeviceEnum` 或 `Ax*Device`。

也就是说，`ax-driver-base` 只定义“底座接口”，并不规定设备如何初始化、如何被发现、如何被消费。

### 2.3 当前实现特征
- 该 crate 没有 Cargo feature，也没有内部子模块拆分，说明它被刻意维持在最稳定、最少变化的一层。
- `DevError` 主要覆盖驱动常见失败语义，例如 `Again`、`NoMemory`、`Unsupported`、`BadState` 等，适合 no_std 驱动环境的通用错误归一。
- `core::fmt::Display` 只为 `DevError` 提供人类可读文本，不承担额外错误上下文聚合。

## 3. 依赖关系图谱
### 3.1 直接依赖
`ax-driver-base` 当前没有本地或外部 Rust 依赖；`Cargo.toml` 的 `[dependencies]` 为空。这进一步说明它只承担语言层面的共性抽象。

### 3.2 主要消费者
- `axdriver_block`
- `axdriver_display`
- `axdriver_input`
- `axdriver_net`
- `axdriver_vsock`
- `ax-driver-virtio`
- `os/arceos/modules/axdriver`
- `platform/axplat-dyn` 的动态块设备适配路径

### 3.3 关系总结
可以把依赖方向概括为：

- 向下：无本地依赖。
- 向上：被所有类别层、总线适配层和聚合层共享。
- 向旁：通过 `ax-driver::prelude` 间接进入 `ax-display`、`ax-input`、`ax-net`、`ax-fs` 等上层模块。

## 4. 开发指南
### 4.1 何时应该修改本 crate
只有在以下场景才应改动 `ax-driver-base`：

- 需要新增一种全新的设备类别。
- 需要为所有驱动统一引入新的基础元信息。
- 需要补充跨类别通用的错误语义。

如果只是新增块设备、网卡、显示设备或输入设备能力，通常应改对应的类别 crate，而不是这里。

### 4.2 修改时必须同步检查的地方
1. 若新增 `DeviceType` 成员，需要同步检查 `ax-driver::AxDeviceEnum`、`AllDevices`、`prelude.rs` 和上层消费者是否都能识别该类别。
2. 若调整 `DevError`，需要确认 `ax_driver_virtio::as_dev_err()`、`axdriver_block`、`axdriver_net` 等映射逻辑仍然一致。
3. 若给 `BaseDriverOps` 增加新方法，会影响所有驱动实现，是高风险接口变更。

### 4.3 常见误区
- 不要把类别专属能力塞进 `BaseDriverOps`。例如 `read_block()`、`flush()`、`read_event()` 都应留在各自类别 trait 中。
- 不要把设备探测接口放进这里；探测属于 `ax-driver` 或总线/传输适配层的职责。
- 不要把“设备名可打印”误解为“设备可被系统自动管理”；管理逻辑在上层。

## 5. 测试策略
### 5.1 当前验证形态
该 crate 没有单独的 `tests/` 目录。它的正确性主要通过编译期接口一致性和上层集成使用来验证。

### 5.2 建议的单元测试重点
- `DeviceType` 的基础匹配逻辑。
- `DevError` 的 `Display` 文本。
- `BaseDriverOps` 默认 `irq_num()` 返回 `None` 的契约。

### 5.3 集成测试重点
- 各类别驱动能否以同一套 `BaseDriverOps` 被 `ax-driver::prelude` 和 `AllDevices` 使用。
- `DevError` 是否能被 `ax-driver-virtio`、`axdriver_net`、`axdriver_block` 等映射成一致行为。

### 5.4 风险点
- 新增设备类别时，最容易漏掉上层聚合容器和日志路径。
- 改动 `BaseDriverOps` 是典型的“低层小改动、全栈大影响”风险点。

## 6. 跨项目定位分析
### 6.1 ArceOS
这是当前仓库中最主要的直接消费方。ArceOS 通过 `ax-driver` 聚合层把它作为整个驱动体系的公共接口底座。

### 6.2 StarryOS
StarryOS 不是直接围绕 `ax-driver-base` 写业务逻辑，而是通过共享的 `ax-driver`、`ax-display`、`ax-input` 等模块间接复用这套基础契约。

### 6.3 Axvisor
当前仓库里没有看到 Axvisor 直接把 `ax-driver-base` 当作其核心设备管理接口。即便未来在某些宿主兼容路径中复用它，它也只会扮演“共享驱动契约层”，而不是虚拟机设备分发中心。
