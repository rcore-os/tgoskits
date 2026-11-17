# 龙芯 PCI 配置空间与 Web 管理网络

## 1. 运行边界

龙芯 3A6000 的 Linux 客户机验收只需要独立 RAM、虚拟串口和虚拟块设备。
Axvisor Web 后台还需要宿主发现真实网卡并取得网络地址。两条路径共享启动入口，
但客户机测试成功不能证明宿主 PCI 网卡已被正确枚举。

### 1.1 实板证据

同一提交下，`cargo xtask axvisor test board --board loongson-3a6000-ll --test-case smoke`
和使用相同构建配置的 `cargo axvisor board` 均已进入 Linux 7.1 shell。
启用 `web-ui` 后，HTTP 服务能够监听 `0.0.0.0:8080`，但没有可用的物理网卡。
启用 PCI 枚举日志后，USB、显示和 SATA 被报告为 `00:00.2`、`00:00.3` 和 `00:00.4`。
这些位置与将原生 CAM 地址误读为标准 ECAM 地址的结果一致。

早先的 `axloader` 串口身份超时发生在内核入口之前；之后相同 Web 内核摘要已完成
交接。因此这项 PCI 修复针对宿主网卡发现，不将先前超时归因于内核配置。

### 1.2 上游协议

对照版本为本地 Linux 7.1，依据是 `drivers/pci/controller/pci-loongson.c` 的
`cfg1_map()`、`pdev_may_exist()` 和 `loongson_pci_ecam_ops`，以及
`drivers/acpi/pci_mcfg.c` 的 `LOONGSON_ECAM_MCFG`。
原生布局将总线放在位 16，设备功能号放在位 8，扩展寄存器高四位放在位 24。
非根总线访问另设位 28；根总线槽位 9 至 20 的非零功能不能访问。

`LOONGS` OEM、空或 `LOONGSON` table ID、OEM revision 1 和 segment 0 至 7
是 Linux 该兼容项的固件选择条件。QEMU 等标准 ECAM 固件继续使用标准布局。

## 2. 共享布局

配置空间访问包含两个消费者：`AcpiHandler` 为 AML 的 PCI operation region 读写
寄存器，`PcieGeneric` 为设备枚举及端点驱动提供访问。布局必须由一个类型拥有，
不能只修正枚举而让 `_PRT` 或其他 AML 方法继续读错同一设备。

### 2.1 分层与选择

`pcie::ConfigSpaceLayout` 保存 ECAM 与龙芯 CAM 的地址计算、访问限制和窗口
范围，由 `read_pci_ecam_regions()` 根据 MCFG 元数据生成 `AcpiPciEcam`。
`AcpiHandler::pci_register()` 和 `PcieGeneric` 消费同一布局。
这属于同一 CPU 架构内不同固件描述的硬件差异，不能用 `target_arch` 替代选择。

公共契约仍是 `rdif_pcie::Interface` 的配置寄存器读写，枚举、BAR、INTx 路由、
DMA 能力和网络队列所有权由现有层负责。本修复不新增驱动注册表、运行时任务或 trait。

| 所有者 | 标准 ECAM | 龙芯原生 CAM |
| --- | --- | --- |
| `pcie` | 总线位 20、设备位 15、功能位 12 | 总线位 16、设备位 11、功能位 8、扩展寄存器位 24 |
| `rdrive` | 根据 MCFG 提供标准布局 | 按固件兼容项选择原生布局并供 AML 使用 |
| `ax-driver` | 按既有 MMIO 能力注册控制器 | 使用同一能力映射原生配置窗口 |
| `ax-net` 与 Axvisor | 现有网卡、DHCP 和 HTTP 运行时 | 相同的网卡、DHCP 和 HTTP 运行时 |

硬件差异止于配置地址计算。两种布局继续通过现有控制器接口进入共同的 PCI 枚举。
`PciIterator` 在整个控制器遍历期间分配唯一的下游总线号，不能从没有配置寄存器的
合成根桥回读分配进度；否则多个根端口都会重用 bus 1，漏掉其他端口上的设备。

### 2.2 所有权与失败

`PcieGeneric` 持有 MMIO 映射，映射的生命周期覆盖全部配置寄存器操作。
地址、偏移、窗口边界和整型溢出在访问前检查；不可访问功能按 PCI 语义读为全一，
写入无副作用。AML 使用固件永久映射，但与枚举使用相同的地址计算。

原生窗口必须覆盖 Type 1 和扩展寄存器区域，不能继续用标准 ECAM 的总线数量乘
1 MiB 推导。固件选择及实际窗口布局需要进入已有启动日志，以区分驱动未启用、
端点未发现、IRQ 装配失败和 DHCP 未完成。

## 3. 验证与回退

这项改动涉及共享硬件访问边界，合入前需要 PCI 和 LoongArch 领域审查。
宿主测试证明固件选择及寄存器访问，实板证明真实枚举、网卡和管理后台；
标准 ECAM 的 QEMU 启动继续覆盖另一种布局。

### 3.1 回归证明

`firmware_selected_pci_access_reads_and_updates_the_real_function` 通过带校验和的
RSDP、XSDT 和 MCFG 装配真实 `AcpiTables`，然后经生产 `AcpiHandler` 读取设备身份
并更新 command 寄存器。配置缓冲区同时覆盖原生与错误 ECAM 位置，旧实现会把
龙芯设备读为不存在，确定性失败而不触发未映射内存访问。

该用例并入既有 `rdrive` 标准库测试，不新增独立内核启动。
总线分配回归通过两个合成根端口读回不同的真实端点；旧分配器会在两端口
重复返回同一 bus 1 端点。该测试复用既有 PCI 枚举模型。
必要验证包括受影响软件包的 `cargo xtask clippy`、同一回归的红绿结果、
龙芯板卡的真实网卡地址及 HTTP/WebSocket 访问，以及标准 ECAM 的定向 QEMU 用例。

### 3.2 默认启动

工作树本地配置复用 `tmp/axbuild/.axvisor.toml`、导出的构建 TOML 和 `.board.toml`。
它们选择 Linux 客户机、`web-ui`、实际网卡驱动及 `AXVM_HTTP_BIND`，让使用者通过
`cargo axvisor board` 启动。浏览器访问地址必须来自本轮运行时网络状态并经真实
请求验证，不以 HTTP 监听日志代替可达性。

回退不涉及持久数据迁移：恢复此前代码和本地构建配置后重新上板。
镜像、客户机块夹具和板卡原系统分区不因本修复而改写。

## 4. 物理网络

修复枚举后，开发板只有一个 `1f0a:6801` 以太网端点，即 Motorcomm YT6801。
启用 `realtek-rtl8125` 不能驱动该端点。Linux 的 `dwmac-motorcomm.c` 将其接入
DWMAC4，MAC 位于 BAR0 的 `0x2000` 偏移，片内 PHY 是 YT8531S。

### 4.1 驱动边界

`motorcomm-yt6801` 作为独立 `no_std` 驱动拥有芯片寄存器、eFuse、MDIO 初始化、
描述符环和中断快照。`ax-driver` 只负责 PCI 身份匹配、BAR、DMA 能力、IRQ 路由
以及初始化延迟；不将 Axvisor 或操作系统依赖引入可复用驱动。

已核对现有 `loongson_gmac`，其 GMAC3 寄存器和描述符格式不适用于 DWMAC4。
外部 `dwmac-my` 使用静态 HAL 和教学用途的轮询路径，缺少当前固定 CPU 网络
轮询组、独占 DMA token 和关闭契约，因此不将它作为运行时依赖。

### 4.2 队列生命周期

首版提供一个 RX/TX 队列对，通过 `NetDeviceParts` 接入现有运行时。
队列持有不能复制的 `DmaBuffer`，发布 OWN 前执行 DMA 屏障，完成后才归还 token。
MMIO 映射通过不可变共享所有权覆盖控制、队列和中断端点的完整生命周期。
控制端点独占接收过滤设置，队列不并发改写该寄存器。

硬中断只屏蔽和确认设备状态，并返回 `NetIrqSnapshot`；固定 CPU 的队列所有者
负责回收、补充和 `rearm_and_check()`。不引入后台定时轮询或无 IRQ 后备路径。
关闭时先屏蔽中断、停止 DMA，再等待软件复位完成；无法确认停止时返回
`DmaShutdownUnconfirmed`，由已有运行时保留 DMA 资源，避免提前释放。

### 4.3 失败与验收

BAR 长度、MAC、MDIO、复位和 DMA 地址必须验证；硬件等待有上限，失败保留
具体阶段。PCI 与 IRQ 注册失败沿现有探测错误返回，不报告网络已就绪。
初始范围为普通以太网帧和软件校验和，不增加卸载、RSS、WOL 或持久固件修改。
原生 INTx 使用 `DMA_MODE.INTM=1` 的电平模式，复位的脉冲模式会使接收完成
失去后续唤醒。PCH-PIC 初始化明确启用 HTMSI 转换、HT0 路由并设置向量，不依赖
固件留下的转发配置。

真实验收必须同时观察 IRQ 路由、收发完成、DHCP 地址、HTTP 请求和浏览器中的
客户机终端交互。本轮开发板通过 DHCP 取得地址，浏览器事件通道和客户机终端
均已连接；网页中执行 Linux `uname -a` 和 `sleep 1` 后正常返回。
描述符及解析的宿主测试不能替代这些板卡证据。
