# `axdriver_display` 技术文档

> 路径：`components/axdriver_crates/axdriver_display`
> 类型：库 crate
> 分层：组件层 / 显示设备类别接口层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`components/axdriver_crates/axdriver_virtio/src/gpu.rs`、`os/arceos/modules/axdisplay/src/lib.rs`、`os/StarryOS/kernel/src/pseudofs/dev/fb.rs`

`axdriver_display` 的职责是为显示设备定义统一的驱动接口。它不负责探测设备，也不负责把帧缓冲暴露成 `/dev/fb0`，更不提供窗口系统或图形合成能力；它只定义显示驱动最小需要暴露的信息和操作，让 `virtio-gpu` 之类的具体实现可以被 `ax-driver` 聚合层和 `ax-display` 上层模块统一消费。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 只处理“显示设备驱动应该向上暴露什么”：

- `DisplayInfo` 描述显示分辨率和帧缓冲位置。
- `FrameBuffer` 封装设备映射出来的帧缓冲内存。
- `DisplayDriverOps` 定义查询信息、获取帧缓冲、判断是否需要刷新以及执行刷新这四类操作。

它在分层上位于：

- `ax-driver-base` 之上：继承统一的 `BaseDriverOps`。
- 具体显示驱动之下：例如 `ax_driver_virtio::VirtIoGpuDev` 会实现它。
- `ax-display` 之下：上层模块通过 `AxDisplayDevice` 使用它，而不是直接接触具体 GPU 驱动类型。

### 1.2 关键类型
| 符号 | 作用 | 备注 |
| --- | --- | --- |
| `DisplayInfo` | 描述宽、高、帧缓冲虚拟地址和大小 | 是上层显示能力的元信息入口 |
| `FrameBuffer<'a>` | 对帧缓冲内存的薄包装 | 不拥有显存，只包装已映射内存 |
| `DisplayDriverOps` | 显示设备统一 trait | 继承 `BaseDriverOps` |

### 1.3 帧缓冲对象模型
`FrameBuffer` 的设计非常克制：

- `from_raw_parts_mut()` 允许驱动把一段已经映射好的设备内存包装成帧缓冲。
- `from_slice()` 允许直接用切片构造。
- 它本身不做像素格式协商、双缓冲管理或显存映射建立。

因此，`FrameBuffer` 只是“对一段可写图像内存的访问句柄”，不是图形 API。

### 1.4 与上下层的接线关系
在当前仓库中，最典型的实现来自 `ax_driver_virtio::VirtIoGpuDev`：

- `try_new()` 内部调用 `virtio_drivers::device::gpu::VirtIOGpu`。
- 通过 `setup_framebuffer()` 建立帧缓冲。
- 用 `resolution()` 生成 `DisplayInfo`。
- `flush()` 最终转发给底层 VirtIO GPU 刷新。

再往上一层：

- `ax-driver` 把具体实例包装为 `AxDisplayDevice` 放入 `AllDevices.display`。
- `ax-runtime` 调用 `ax_display::init_display(all_devices.display)`。
- `ax-display` 提供 `framebuffer_info()` / `framebuffer_flush()`。
- StarryOS 的 `pseudofs/dev/fb.rs` 再把这组能力包装成伪文件系统的 framebuffer 设备。

### 1.5 边界澄清
最重要的边界是：**`axdriver_display` 只定义“显示驱动该如何向上暴露帧缓冲能力”，它不是图形子系统，也不是用户可见显示服务本身。**

## 2. 核心功能说明
### 2.1 主要能力
- 为显示设备统一定义 `DisplayDriverOps`。
- 用 `DisplayInfo` 传递最关键的屏幕和帧缓冲信息。
- 用 `FrameBuffer` 表达设备映射显存的可写视图。
- 让不同显示驱动能被 `ax-driver` 和 `ax-display` 统一消费。

### 2.2 关键 API 与语义
- `info()`：返回显示元信息。
- `fb()`：返回帧缓冲视图。
- `need_flush()`：说明写入显存后是否还需要显式提交。
- `flush()`：把帧缓冲内容真正推到设备侧。

`need_flush()` 的存在很关键，因为它把“直写显存即可见”和“还需要显式提交”这两类设备统一在一套接口里。

### 2.3 当前实现特征
- 该 crate 自身没有 Cargo feature，也没有内建具体 GPU 驱动。
- `FrameBuffer::from_raw_parts_mut()` 是 `unsafe`，调用者必须保证地址区间有效且可访问。
- 像素格式、模式切换、硬件加速等更复杂语义并未纳入当前接口模型。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-driver-base` | 提供 `BaseDriverOps`、`DeviceType`、`DevError` |

### 3.2 主要消费者
- `components/axdriver_crates/axdriver_virtio`
- `os/arceos/modules/axdriver`
- `os/arceos/modules/axdisplay`
- `os/StarryOS/kernel/src/pseudofs/dev/fb.rs`

### 3.3 分层关系总结
- 向下没有具体总线或设备依赖。
- 向上为显示驱动统一暴露帧缓冲语义。
- 最终由 `ax-display` 把能力整理成更接近上层使用的形式。

## 4. 开发指南
### 4.1 什么时候应修改这里
适合修改 `axdriver_display` 的情况是：

- 需要给所有显示驱动新增共通元信息。
- 需要为显示驱动补充真正“类别共通”的操作。
- 需要修正帧缓冲生命周期或刷新语义的契约。

如果只是给某个具体显示驱动补充模式设置逻辑，通常应改对应实现 crate，而不是这里。

### 4.2 实现新驱动时的建议
1. 先保证 `DisplayInfo` 中的宽、高、`fb_base_vaddr`、`fb_size` 都准确可用。
2. 明确 `fb()` 返回的内存是否长期有效，以及是否与 `flush()` 配套。
3. 若设备支持直接显存映射但无需刷新，应让 `need_flush()` 返回 `false`。
4. 若显存地址来源于 MMIO 或 DMA 映射，应在驱动实现层完成映射建立，不要把映射职责推给本 crate。

### 4.3 常见坑
- 不要把 `FrameBuffer` 当成图形渲染 API；它只是显存切片。
- 不要把 `DisplayInfo` 当成模式枚举器；当前只表达当前工作模式。
- 不要在这里引入用户态或伪文件系统语义；那属于 `ax-display` 或更上层。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立测试目录，当前验证主要依赖：

- `virtio-gpu` 驱动能否正确实现 `DisplayDriverOps`。
- `ax_display::framebuffer_info()` 和 `framebuffer_flush()` 是否能正常工作。
- StarryOS 的 `/dev/fb` 是否能据此提供可用帧缓冲设备。

### 5.2 建议补充的单元测试
- `FrameBuffer::from_slice()` 和 `from_raw_parts_mut()` 的基本包装行为。
- `need_flush()` / `flush()` 契约在 mock 设备上的一致性。
- `DisplayInfo` 元信息正确传递到上层。

### 5.3 集成测试重点
- QEMU `virtio-gpu` 启动和 framebuffer 写屏验证。
- `ax-display` 单全局设备路径。
- StarryOS `fb` 设备的 `ioctl`、`mmap` 和刷新路径。

### 5.4 风险点
- 帧缓冲地址和大小一旦错误，上层看到的不是普通错误，而是直接的显存越界风险。
- 若驱动对 `need_flush()` 语义实现不一致，表现往往是“写屏成功但屏幕不更新”。

## 6. 跨项目定位分析
### 6.1 ArceOS
ArceOS 通过 `ax-driver` 和 `ax-display` 直接消费本 crate 的接口，是当前最明确的主线使用者。

### 6.2 StarryOS
StarryOS 不直接实现新的显示驱动 trait，而是通过 `ax-display` 提供的能力在 `pseudofs/dev/fb.rs` 中构造 framebuffer 设备，因此它更偏上层消费者。

### 6.3 Axvisor
当前仓库中没有看到 Axvisor 直接把 `axdriver_display` 作为核心图形抽象使用。它不是 Axvisor 的虚拟显示设备框架。
