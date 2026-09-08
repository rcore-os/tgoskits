---
sidebar_position: 2
sidebar_label: "总体架构"
---

# 总体架构

`rdrive + rdif` 驱动框架以分层隔离为核心。新的宿主设备路径分为五层：平台发现来源、`rdrive` backend 分发、具体驱动 core、`rdif` 能力边界、领域 service 与上层消费方。各层之间通过明确的契约交互，不反向依赖。

## 五层结构

```mermaid
flowchart TB
    subgraph Sources["平台发现来源"]
        Static["Static: 显式平台元数据"]
        Fdt["FDT: device tree"]
        Acpi["ACPI: reserved API / unsupported first"]
    end

    subgraph Rdrive["rdrive"]
        Manager["Manager: register + typed registry only"]
        Register["DriverRegister"]
        Kind["ProbeKind: Static / Fdt / ACPI / PCI"]
        Backends["probe::{static_,fdt,acpi,pci}"]
    end

    subgraph DriverCore["Driver Core"]
        HwState["registers / descriptors / state machines"]
        Queues["queues / requests / events"]
    end

    subgraph Capability["Capability Boundary"]
        RdifBlock["rdif-block"]
        RdifEth["rdif-eth: NetDeviceParts"]
        RdifDisplay["rdif-display"]
        RdifInput["rdif-input"]
        RdifVsock["rdif-vsock"]
        RdifPlatform["rdif-intc / pinctrl / pcie / clk / timer / serial"]
    end

    subgraph Services["领域 service"]
        BlockVolume["block volume / partition"]
        NetService["ax-net: queue_runtime + Service"]
        UiService["display / input service"]
        VsockService["vsock service"]
    end

    subgraph Consumers["消费方"]
        Runtime["ax-runtime"]
        Fs["ax-fs / ax-fs-ng"]
        Net["ax-net"]
        Starry["starry-kernel"]
        Axvisor["axvisor"]
    end

    Sources --> Backends
    Backends --> Register
    Register --> Kind
    Kind --> DriverCore
    DriverCore --> Capability
    Capability --> Manager
    Runtime --> Backends
    Manager --> BlockVolume
    Manager --> UiService
    Manager --> VsockService
    Manager --> NetTake["ax-runtime: take_net_device"]
    NetTake --> NetPrepare["rd-net: prepare_device / DMA pools"]
    NetPrepare --> NetService
    Services --> Fs
    Services --> Net
    Services --> Starry
    Services --> Axvisor
```

`rdrive::Manager` 只保存 `DriverRegister` 和类型化设备 registry。Static、FDT、ACPI、PCI 各自拥有独立 `probe::*::{System, Info, FnOnProbe}`，不把平台状态合并成一个大 `System`。

网络分支的 registry 对象是 `PlatformNetDevice`，不是运行期共享网卡。`ax-runtime` 取出设备和 IRQ 映射，`rd-net` 消费 `NetDevice::into_parts()` 并准备 DMA 池，`ax-net::NetworkRuntimeBuilder` 再构造队列执行器和 `EthernetFramePort`。图中的领域 service 表示职责分组；网络 `Service` 与队列运行时实际位于 `net/ax-net` 同一 crate 内，具体执行域见[网络驱动](network.md)。

## 数据流方向

设备数据流是单向的：平台发现 → probe → driver core 实例化 → rdif 能力注册 → 领域 service 查询 → 上层消费。反向依赖被严格禁止：

- Driver Core 不依赖 `rdrive`、`ax-driver`、`ax-hal` 或平台 crate。
- `rdif-*` 不依赖平台、runtime 或任务调度。
- OS Glue 不引入上层 FS/NET 策略。
- Runtime wrapper（`rd-*`）不参与 probe、设备树解析或平台选择。

## 核心源码

| 源码 | 职责 |
| --- | --- |
| `drivers/rdrive/src/manager.rs` | `Manager`、`DeviceContainer`、类型化设备查询 |
| `drivers/rdrive/src/register/mod.rs` | `DriverRegister`、`ProbeKind`、`ProbeLevel`、`ProbePriority`、`RegisterContainer` |
| `drivers/rdrive/src/probe/mod.rs` | `ProbeError`、`OnProbeError`、backend 分发 |
| `drivers/rdrive/src/probe/static_.rs` | Static platform probe |
| `drivers/rdrive/src/probe/fdt/` | FDT probe、compatible 匹配 |
| `drivers/rdrive/src/probe/acpi.rs` | ACPI probe、HID/CID 匹配、MCFG/GSI routing |
| `drivers/rdrive/src/probe/pci/` | PCIe endpoint 枚举 |
| `drivers/rdrive/src/driver/mod.rs` | `PlatformDevice`、`register()` |
| `drivers/rdrive/src/descriptor.rs` | `Descriptor`、`DeviceId` |
| `drivers/ax-driver/src/lib.rs` | ArceOS glue：`module_driver!`、feature 分发 |
| `drivers/ax-driver/src/binding_info.rs` | `BindingInfo`、IRQ binding 元数据 |
| `drivers/ax-driver/src/binding_resolver.rs` | FDT/ACPI/PCI IRQ 解析 |

## 设计选择

### 为什么不用单一大容器

旧的 `AllDevices` 模型把 block/net/display/input/vsock 设备塞进一个全局结构体，runtime 启动时拆包逐个传给模块。这种设计的问题：

- 新增设备类别需要修改全局容器和 runtime 拆包逻辑。
- 全局设备交付与领域消费绑定在启动拆包流程中，动态注册需要额外修改消费路径。
- 上层模块被迫依赖整个容器类型，耦合扩散。

`rdrive + rdif` 改用类型化 registry：每个设备按 `DeviceId` 注册为 `DeviceOwner`（持有 `Box<dyn DriverGeneric>`），上层通过 `Device<T>` 弱引用按领域能力 trait 查询。registry 的动态注册能力与领域运行时的设备接入能力分别定义；当前物理网络在启动时一次性交付，Wi-Fi 控制事务不等于动态注册一块新网卡。

### 为什么 Capability Boundary 独立

`rdif-*` 只定义“某类设备向上暴露什么能力”，不包含设备发现、iomap、IRQ 注册或任务调度。这样做的收益：

- Driver Core 可以在不同 OS（ArceOS、StarryOS、Axvisor）之间复用而不重新实现能力接口。
- 上层模块面向 trait 编程，设备实现可替换。
- 能力契约稳定，硬件演进不影响上层 API。

### 为什么多来源并列

嵌入式/aarch64 平台常用 FDT，x86/服务器用 ACPI，PCIe 设备独立枚举，某些静态平台需要显式注册。把这些来源合并成单一 `System` 会引入隐式优先级和状态污染。`rdrive` 让每个 backend 拥有独立 `System`，通过 `ProbeKind` 分发，平台来源显式且可组合。

详细 backend 模型见 [rdrive 设备管理](rdrive.md)，初始化时序见 [设备探测与初始化](probe.md)。
