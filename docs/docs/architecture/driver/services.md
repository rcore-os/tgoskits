---
sidebar_position: 11
sidebar_label: "领域服务"
---

# 领域服务与系统交付

领域服务把驱动能力转换为系统需要的块区域、字节流、帧端口、显示输入接口、连接或设备树。服务是职责划分，不意味着每类设备都存在独立的 `rd-*` 软件包，也不要求普通业务请求反复查询注册表。

## 1. 交付边界

设备管理负责发现和身份，运行时负责执行与完成，服务层负责可消费对象及领域状态。三者可能位于同一 crate，但不能混用职责。

### 1.1 交付物

交付物隐藏哪些硬件细节取决于能力形态。provider 消费方本来就是平台层，而普通文件或协议调用不应接触设备寄存器。

| 能力 | 交付对象 | 主要消费方 |
| --- | --- | --- |
| 块设备 | 块句柄、`BlockVolume`、区域读写视图 | 文件系统与根卷选择 |
| 串口 | 字节输入、发送入口、控制台输出 | 终端、控制台与日志 |
| 网络 | `EthernetFramePort`、协议及控制状态 | socket 与网络系统接口 |
| 显示 | `DisplayDevice` 与帧缓冲信息 | 显示模块及用户态适配 |
| 输入 | `InputDevice` 与事件能力 | 输入事件消费者 |
| vsock | CID、连接及事件接口 | 连接传输适配 |
| USB host | 设备变化、描述符和设备对象 | USB 设备管理与 USBFS |
| 平台 provider | 时钟、复位、IRQ、引脚及总线控制 | HAL、SoC 和驱动绑定 |

共享的是按合同交付能力，不是业务数据格式或服务选择策略。运行时内部算法由[运行时与完成](runtime.md)维护。

### 1.2 查询与接管

`devices.rs` 等初始化代码可以枚举注册包装并接管设备，HAL 和 USB 主机管理也需要查询身份。普通请求应通过已经交付的领域句柄访问，不能在每次 I/O 中重新 probe。

take 成功后注册包装可能为空，原 `Device<T>` 不再代表可以取得第二份硬件所有权。交付失败和服务不可用的处理需要结合[生命周期](lifecycle.md)，不能在服务层静默重新拿取设备。

## 2. 存储区域服务

存储服务区分原始块设备、分区元数据、受限区域和文件系统。`fs/ax-fs-ng` 内的 block runtime、volume 和 root 模块分别维护这些状态。

### 2.1 控制器到设备句柄

驱动侧 `drivers/ax-driver/src/block/binding.rs` 的 `RdifBlockDevice` 保存 `BindingIrqBinding`，FS 侧 `block/runtime/lifecycle/mod.rs` 的同名类型保存 `BlockIrqSource`。两者之间需要解析 IRQ，不能因同名直接省略转换。

`BlockRuntime::from_rdif_devices()` 与 `from_rdif_sources()` 建立控制器和块句柄集合。服务需要控制器已可用才能发起有效 I/O，不能将注册成功当作所有队列已经 Ready。

### 2.2 卷与根选择

`volume/scan.rs` 的 `scan_volumes()` 校验块大小和数量，依次识别 GPT、MBR，未识别时返回 Raw 整盘卷。解析错误直接传播，不等于“没有分区表”。

| `BlockVolume` 字段 | 领域含义 |
| --- | --- |
| `disk_id`、`partition_id` | 磁盘与分区身份；Raw 的分区编号为 0 |
| `region` | 起始块和块数 |
| `table_kind`、`bootable` | 分区表类型与启动标志 |
| `partuuid`、`partlabel` | 可选 UUID 和分区标签 |

`root.rs` 的 `RootSpec` 解析启动参数，`VolumeReader` 限制区域访问。选择根卷和识别文件系统不属于控制器 probe，也不属于 `ax-driver` 的分区服务。

## 3. 字节、帧与连接服务

不同传输需要不同的服务身份与背压。字节流、Ethernet 帧和 vsock 连接不能统一为一张物理设备列表。

### 3.1 串口与控制台

`os/arceos/modules/axruntime/src/serial/mod.rs` 提供串口运行时发送和订阅相关对象，`serial/worker.rs` 处理实际收发与日志。软件输出屏障、控制命令和 RX 订阅分别有状态，写入接口接受字节不等于发送器已经空闲。

早期控制台与运行时控制台交接需要配置恢复及输出归属，`console::activate_before_smp()` 的结果决定继续使用哪条输出路径。紧急输出不作为普通 tty 控制接口暴露。

### 3.2 帧与连接

`net/ax-net/src/device/driver.rs` 的 `EthernetFramePort` 对协议层隐藏寄存器与描述符。`NetworkRuntimeBuilder` 成功后交付运行时和端口，`init_network()` 再建立逻辑接口与协议状态；缺少物理网卡时允许空端口协议初始化。

`Service` 持有单个 smoltcp `Interface`，`Router` 汇聚逻辑设备，`NetControl` 提供状态快照。系统接口不另建一套竞争的地址、路由或网卡表。Unix socket 与 vsock 使用独立传输，vsock 的 CID 和连接事件不依赖物理 Ethernet 队列。

## 4. 显示、输入与设备树

这类服务在驱动能力上建立设备视图。设备信息快照、长期持有的驱动对象和事件队列分别有生命周期。

### 4.1 显示与输入

`devices.rs` 接管后构造 `RdifDisplayDevice` 与 `RdifInputDevice`，由各领域模块消费。显示包装保存设备及帧缓冲信息，输入包装转换标识、能力位图和事件。

事件就绪不表示用户态已经读取，帧缓冲地址也不自动成为永久有效的用户映射。Linux 文件描述符、ioctl 和用户态内存处理由系统适配负责，不下沉到可复用硬件核心。

### 4.2 USB 设备视图

`USBHost::probe_devices()` 返回连接和断开集合，`Core` 维护拓扑，`open_device()` 获取实际设备对象。hub、普通外设、主机控制器是不同层次，不能都当作一个 `rdrive` 注册条目。

StarryOS 的 `os/StarryOS/kernel/src/pseudofs/usbfs/manager.rs` 管理主机，`tree.rs` 保存系统视图，`irq.rs` 接入事件。该管理路径可以查询注册主机，但不会因此授权普通请求在硬 IRQ 中遍历全局注册表。

## 5. provider 与消费策略

平台控制资源常由其他驱动或 HAL 直接消费，不一定创建用户可见服务。默认设备、根卷、控制台或路由选择属于消费者策略。

### 5.1 资源服务

时钟、复位、pinctrl、PCIe 和中断控制器的句柄为平台层提供操作。消费者需使用对应资源 ID 和访问规则，不复制 provider 寄存器状态到另一张未经同步的表。

### 5.2 发布与撤销

服务发布之前应完成该领域需要的初始化，撤销时应处理上层引用和未完成操作。网络端口构造失败、根卷缺失、USB 断开与显示设备缺失的系统影响不同，不能使用统一“忽略设备错误”的策略。

```mermaid
flowchart LR
    Capability[已交付驱动能力] --> Runtime[领域运行与完成]
    Runtime --> View[块区域 字节 帧 事件 或设备树]
    View --> Policy[消费者选择与系统策略]
    Policy --> User[文件 协议 控制台 或平台调用]
```

图中的领域视图不是公共 `AllDevices` 容器。具体启动位置和功能开关由[系统集成](integration.md)及[构建配置](features.md)维护。
