---
sidebar_position: 0
sidebar_label: "专题边界"
---

# USB 实现专题

USB 专题记录公共驱动机制在主机控制器、总线枚举和 RK3588 资源绑定中的实现。主架构以发现、能力、资源、事件、运行时和生命周期组织；专题补充 TRB、hub 拓扑、PHY 及寄存器等具体对象，不把 USB 的执行模型定义成全框架标准。

## 1. 组件对应

`drivers/ax-driver/src/usb/` 绑定平台资源，`drivers/usb/usb-host/src/` 提供主机与设备对象，`backend/kmod/` 保存通用 Core 和控制器后端。系统 USBFS 等消费者保留用户接口和设备可见性。

### 1.1 公共机制

主机与外设属于不同发现层次，主机注册进入 `rdrive`，外设由 `Core` 的拓扑和枚举结果维护。对应关系不能压成一个统一设备列表。

| 公共章节 | USB 实现对象 |
| --- | --- |
| [探测](../probe.md) | FDT/PCI host probe 与 `PlatformUsbHost` |
| [能力](../capability.md) | `USBHost`、`CoreOp`、设备与端点操作 |
| [资源](../resources.md) | MMIO、`KernelOp`、DMA、PHY 与 GRF |
| [事件](../irq.md) | 独立 EventHandler 与完成路由 |
| [运行时](../runtime.md) | `Core`、future、`Finished`、`TWaiter` |
| [生命周期](../lifecycle.md) | 端点取消、配置切换和子树断开 |

网络的固定 owner、块的完成排空和 USB 的 Event Ring 消费是不同合同，不能互相替代。

### 1.2 专题结构

专题页面分别维护执行机制、拓扑编码、控制器到外设的发现，以及 SoC 实例资源。历史观察和外部源码对照具有明确适用范围。

| 页面 | 内容归属 |
| --- | --- |
| [异步执行](design.md) | TRB 发布、完成槽、事件及取消 |
| [拓扑路由](xhci-route-string.md) | 根端口、hub 路径、slot 和 Route String |
| [设备枚举](rk3588-device-discovery.md) | host probe、初始化、hub 及外设发现 |
| [控制器与 PHY](rockchip/rk3588.md) | Rockchip 初始化与实际等待条件 |
| [DMA 寻址](rockchip/rk3588-dma-analysis.md) | AC64、domain、缓存和地址证据 |
| [寄存器诊断](rockchip/rk3588-phy-register-analysis.md) | MMIO 偏移、配置与历史读零观察 |
| [U-Boot 对照](rockchip/u-boot-comparison.md) | 固定外部版本和函数差异 |
| [节点依赖](rockchip/usb-at-fc400000-dependencies.md) | 指定 DTS 的 phandle、时钟复位和映射 |

这些页面保留现有地址以保持交叉引用稳定，不按设备类别扩展另一套与公共机制重复的架构目录。

## 2. 实现与证据

专题中的寄存器值、节点路径和初始化时序只适用于所注明实现。当前源码、指定 DTS 和旧调试日志分别提供不同层次的证据。

### 2.1 主机与运行期

`PlatformUsbHost` 包含主机和事件入口，异步初始化建立控制器状态，枚举进一步形成设备树。probe 成功不证明外设传输成功，future 停止轮询不证明 DMA 已停止。

![USB 对公共执行与资源机制的实现](../images/usb-execution.svg)

图示的事件锁、完成路由和端点状态属于 USB 实现；通用生命周期条件仍由公共章节定义。

### 2.2 平台与历史记录

RK3588 guest DTS 示例不是每次宿主启动的实际 FDT。PHY 锁定与 DWC3 寄存器读值需要结合控制器实例、资源和初始化阶段，不能仅依据历史读零现象推导整个 SoC 的硬件结论。

外部源码对照不等于当前板卡验证；初始化、描述符读取、持续传输和取消分别需要对应证据。专题不改变硬件实现，也不声明所有配置均已验证。
