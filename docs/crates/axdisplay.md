# `axdisplay` 技术文档

> 路径：`os/arceos/modules/axdisplay`
> 类型：库 crate
> 分层：ArceOS 层 / 显示能力接线层
> 版本：`0.3.0-preview.3`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`os/arceos/modules/axruntime/src/lib.rs`、`os/arceos/api/arceos_api/src/imp/display.rs`、`os/StarryOS/kernel/src/pseudofs/dev/fb.rs`

`axdisplay` 位于 `axdriver_display` 之上、`arceos_api::display` 与 StarryOS framebuffer 设备之下，是一个非常薄的显示能力接线层。它不实现任何 GPU 驱动，也不负责多显示器管理；它做的事情只有一件：从 `axdriver` 聚合层交来的显示设备容器里取出一个主显示设备，保存为全局对象，然后向上提供帧缓冲信息和刷新接口。

## 1. 架构设计分析
### 1.1 真实定位
当前实现只有一个核心全局对象：

- `MAIN_DISPLAY: LazyInit<Mutex<AxDisplayDevice>>`

这已经直接说明了它的设计取舍：

- 全局单主设备。
- 以互斥锁保护访问。
- 只向上暴露 framebuffer 信息和 flush。

因此它不是 `axdriver_display` 那样的驱动类别层，而是更靠近“用户可见能力”的上层模块。

### 1.2 初始化主线
当前仓库中的调用链非常直接：

1. `axruntime` 在完成平台和内核基本初始化后调用 `axdriver::init_drivers()`。
2. `axdriver` 把显示设备按类别收集进 `AllDevices.display`。
3. `axruntime` 在 `feature = "display"` 下调用 `axdisplay::init_display(all_devices.display)`。
4. `axdisplay` 从容器中取出一个设备，存入 `MAIN_DISPLAY`。
5. `arceos_api` 与 StarryOS 等更上层模块通过 `framebuffer_info()` / `framebuffer_flush()` 使用它。

### 1.3 关键 API
| API | 作用 |
| --- | --- |
| `init_display()` | 从底层设备容器初始化显示模块 |
| `has_display()` | 查询是否已有主显示设备 |
| `framebuffer_info()` | 返回当前主显示设备的 `DisplayInfo` |
| `framebuffer_flush()` | 刷新当前主显示设备的帧缓冲 |

### 1.4 单设备模型的真实含义
`init_display(mut display_devs: AxDeviceContainer<AxDisplayDevice>)` 的核心实现只有：

- `display_devs.take_one()`
- `MAIN_DISPLAY.init_once(...)`

这意味着：

- 当前 `axdisplay` 只会选择一个显示设备。
- 如果底层探测出了多个显示设备，其余设备不会在本模块中保留。
- 本模块没有设备优先级选择、热插拔或多显示输出管理逻辑。

这一点和 `axinput` 的“收集所有设备”模型非常不同。

### 1.5 与上下层的边界
| 层次 | 负责内容 | 不负责内容 |
| --- | --- | --- |
| `axdriver_display` / `VirtIoGpuDev` | 具体显示驱动与帧缓冲语义 | 全局主显示管理 |
| `axdisplay` | 选择一个主显示设备并导出 framebuffer 能力 | 设备探测、模式设置、多显示器管理 |
| `arceos_api::display` | 暴露更稳定的 API 入口 | 保存或管理设备对象 |
| StarryOS `/dev/fb` | 把 framebuffer 能力包装成伪文件系统设备 | 驱动初始化与主设备选择 |

### 1.6 边界澄清
最关键的边界是：**`axdisplay` 不是显示驱动层，而是“把一个已经探测好的显示设备变成系统主 framebuffer 能力”的接线层。**

## 2. 核心功能说明
### 2.1 主要能力
- 持有一个全局主显示设备。
- 向上暴露 framebuffer 元信息。
- 向上暴露 flush 操作。
- 通过 `has_display()` 提供最小存在性判断。

### 2.2 与上层 API 的关系
`os/arceos/api/arceos_api/src/imp/display.rs` 直接把：

- `axdisplay::framebuffer_info()` 封装为 `ax_framebuffer_info()`
- `axdisplay::framebuffer_flush()` 封装为 `ax_framebuffer_flush()`

因此 `axdisplay` 是 ArceOS 显示 API 的直接实现后端。

### 2.3 在 StarryOS 中的作用
StarryOS 的 `pseudofs/dev/fb.rs` 会：

- 调用 `axdisplay::framebuffer_info()` 获取尺寸和地址；
- 周期性调用 `axdisplay::framebuffer_flush()`；
- 把 framebuffer 包装成可 `read` / `write` / `mmap` 的伪设备。

这说明 `axdisplay` 已经处在“对用户可见能力很近”的一层，但仍不是最终设备节点层。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `axdriver` | 提供 `AxDisplayDevice`、`AxDeviceContainer` 与 `DisplayInfo` |
| `axsync` | 为主显示设备访问提供互斥 |
| `lazyinit` | 延迟初始化全局主显示设备 |
| `log` | 初始化日志 |

### 3.2 主要消费者
- `os/arceos/modules/axruntime`
- `os/arceos/api/arceos_api`
- `os/StarryOS/kernel/src/pseudofs/dev/fb.rs`

### 3.3 分层关系总结
- 向下消费已经探测好的显示设备对象。
- 向上提供更稳定、更简单的显示能力接口。
- 不参与驱动发现，也不承担最终设备文件语义。

## 4. 开发指南
### 4.1 适合修改这里的场景
应修改 `axdisplay` 的情况主要包括：

- 需要调整全局主显示设备的管理方式。
- 需要在上层导出的显示能力中增加真正通用的操作。
- 需要改进主设备选择策略。

如果只是修改具体 GPU 初始化或 framebuffer 建立逻辑，应去对应驱动实现。

### 4.2 扩展时需要注意的点
1. 当前 API 假设“系统只有一个主显示设备”；若要支持多显示器，需要从类型和接口层整体改造。
2. `framebuffer_info()` 和 `framebuffer_flush()` 都隐含依赖 `MAIN_DISPLAY` 已初始化，调用方通常应先检查 `has_display()`。
3. 若要暴露更复杂能力，例如模式切换或页面翻转，应先确认这些能力应属于 `axdisplay` 还是具体驱动 trait。

### 4.3 常见坑
- 不要把 `axdisplay` 写成“显示子系统”；它没有渲染、合成或窗口管理逻辑。
- `init_display()` 只拿一个设备，不要假设所有底层显示设备都会被保留。

## 5. 测试策略
### 5.1 当前有效验证面
当前主要验证路径包括：

- `virtio-gpu` 启动后 `axdisplay::has_display()` 为真。
- `framebuffer_info()` 返回的宽高和显存大小与底层一致。
- `framebuffer_flush()` 能触发实际屏幕刷新。
- StarryOS `/dev/fb` 读写和 `mmap` 能正常工作。

### 5.2 建议补充的测试
- 无显示设备时 `init_display()` 和 `has_display()` 行为。
- 单设备初始化后 `framebuffer_info()` / `framebuffer_flush()` 的基本契约。
- 多显示设备输入时只保留一个设备的行为是否符合预期。

### 5.3 风险点
- 单主显示模型若被误用到多显示场景，问题不会在编译期暴露，而会在运行期表现为设备被静默忽略。
- `framebuffer_info()` 返回的地址和大小完全信任底层驱动，错误会直接传导到上层 framebuffer 访问。

## 6. 跨项目定位分析
### 6.1 ArceOS
`axdisplay` 是 ArceOS 本体中的显示能力模块，直接承接 `axdriver` 聚合出来的显示设备，并为 `arceos_api` 提供后端实现。

### 6.2 StarryOS
StarryOS 直接消费它，把 framebuffer 能力包装成 `/dev/fb` 伪设备，因此是它在当前仓库里最明确的跨项目落点。

### 6.3 Axvisor
当前仓库没有看到 Axvisor 直接使用 `axdisplay`。它不是虚拟显示控制器，也不是 VMM 侧显示抽象。
