# 统一 SDIO 与 AIC8800 OS 无关驱动架构

## 问题与目标

项目原有多套 SDIO 协议：控制器、旧 `sdio-host` 和 AIC8800 分别编码
CMD5/CMD52/CMD53，并重复解析 CCCR/FBR/CIS 和维护 Function 状态。本次只
保留 `sdmmc-host` 与 `sdmmc-protocol`：前者定义非阻塞物理 host 能力，
后者独占 SD/MMC/SDIO 卡协议。Memory-only 与 IO-only 明确支持；Combo card
返回 `UnsupportedComboCard`。AIC8800 DC/DW 不在本次范围。

## 依据与方案选择

- 卡协议以 SD Association 的 SDIO Simplified Specification 3.00 为语义依据，
  覆盖 CMD5、CMD52、CMD53、CCCR、FBR、CIS、Function 生命周期和 Function IRQ。
- Linux 6.x 的 `sdio_func` 生命周期及 `drivers/mmc/core/sdio_irq.c` 只用于核对
  Function/IRQ 所有权，不复制其内核线程、claim/release 或等待模型。
- 项目内部比较过“保留多套实现并加兼容层”“控制器继续暴露高级 SDIO API”与
  “统一卡协议”。兼容层仍会保留重复状态源，高级控制器 API 会把卡协议下沉到
  硬件层，因此选择唯一 `sdmmc-host` 能力接口加唯一 `sdmmc-protocol` 卡协议。
- AIC 比较过同步包装器、驱动自建线程和 owner 驱动状态机。同步包装器会隐藏
  取消/超时，驱动线程会绑定 OS；因此核心与 adapter 均只返回有限步骤进度，
  由外层 owner 决定等待策略。

## 四层职责

| 层 | crate/位置 | 职责 |
| --- | --- | --- |
| Driver Core | `drivers/net/aic8800` 默认模块 | 寄存器语义、固件/命令/RX/TX/AP 状态机；只接收显式时间、熵、SDIO 完成和 IRQ 快照 |
| Capability Adapter | `drivers/net/aic8800/src/rdif`（`rdif` feature） | 转换 `SdioCard`、拥有型 DMA、RDIF token、IRQ 快照和核心事件；有界 SPSC、无执行线程 |
| OS Glue | `drivers/ax-driver` | FDT、MMIO、CV181x pinmux/reset/clock、DMA capability 和设备注册 |
| Runtime | `net/ax-net`、`axruntime` | 固定 CPU owner、定时唤醒、IRQ 注册/亲和性/同步和队列调度 |

`aic8800` 是唯一 AIC crate 且始终为 `no_std`。默认 feature 不编译 RDIF；
可选 adapter 不依赖 `ax-sync`、`ax-task`、`axruntime`，也不创建线程、休眠
或自旋。两层没有独立发布和生命周期需求，因此不增加第二个 crate。

## 事务与 IRQ 所有权

`sdmmc-host` 定义 move-only `HostParts`；`SdMmcIrqHost::into_parts()` 一次性
拆出 `HostParts { bus, irq, card_irq }`。
硬 IRQ 独占 `irq`，只读/确认/屏蔽状态并发布预分配快照。固定 CPU owner
独占 `bus`、`card_irq`、`SdioCard` 与 `AicDevice`，只有它能推进协议、完成
DMA、处理 FIFO 或修改 Function 状态。

命令完成、DMA、错误和 `CARD_INT` 可同时到达；adapter 先把 CARD_INT 事实
交给核心，再用同一 acknowledged snapshot 推进活动 host 事务。CMD53 使用
拥有型 `PreparedDma`，完成或 abort 后才恢复 `CompletedDma`。RDIF
`DmaBuffer` 只通过有界 SPSC 转移，提交失败原样归还 token。

## 启动、等待和回滚

1. ax-driver 映射 MMIO、执行 SDIO1 SoC 设置并注册 portable device；reset
   settle 以绝对 deadline 交给 owner，不在驱动中 sleep。
2. ax-net 固定 owner CPU，注册并启用硬 IRQ；poll group 仍保持 disabled，
   IRQ 只能唤醒启动状态机，不能开放网络队列。
3. owner 完成 IO-only 卡初始化、Function 生命周期、固件和 FDRV；每步只
   返回 Ready、WaitForInterrupt、RetryAt 或错误。
4. 成功后才 refill RX、rearm 并 publish 队列，再执行可选启动 transaction。
   Wi-Fi 控制同样使用 `start/advance/cancel`。
5. 失败先 disable+synchronize IRQ，再 cancel/abort；证明 host DMA 停止后
   释放队列，无法证明时隔离整个 ownership domain。

安全连接必须携带调用方拥有的 32 字节熵。熵缺失返回
`EntropyUnavailable`，禁止用时间戳代替随机源。

## FDT/ACPI 参数边界

板级物理地址、IRQ、clock/reset/power-domain、pinctrl、DMA 地址宽度、总线频率
和产品网络策略只允许出现在 OS Glue 的固件翻译层。当前 SG2002/CV181x 路径
使用 FDT：SDIO consumer 节点通过 `reg-names` 提供 `sdio`、`syscon`、`crg`、
`rtcsys-ctrl`、`rtcsys-io`，或者分别用 `cvitek,syscon`、`cvitek,crg`、
`cvitek,rtcsys-ctrl`、`cvitek,rtcsys-io` phandle 引用 provider。缺少必要资源时
probe 显式失败，不回退到写死物理地址。

通用 SD/MMC 属性 `bus-width`、`min-frequency`、`max-frequency`、`no-1-8-v`、
`cd-gpios` 与 clock/reset/power-domain 由 rdrive 统一解析。AIC 附加策略使用
`dma-address-bits`、`post-power-on-delay-ms`、`aic,startup-timeout-ms`、
`aic,control-timeout-ms`、`aic,queue-size` 和 `aic,max-frame-size`。可选启动 AP
必须显式配置 `aic,startup-mode = "access-point"` 及 `aic,ap-ssid`、
`aic,ap-channel`、`aic,ap-ipv4`、`aic,ap-prefix-length`；未配置时只注册
`wlan0`，不偷偷采用产品 SSID/IP。

支持 ACPI 的平台使用 rdrive ACPI memory/IRQ/_CCA 资源构造同一 portable
配置；CV181x 当前只存在 FDT probe，因此本次不伪造 ACPI 节点或默认资源。
控制器寄存器 offset、bitfield、AIC Function 1 和固件规定的 512-byte block
属于 silicon/protocol 常量，仍保留在 portable crate。

## 依赖与源码门禁

- `aic8800` 默认依赖树不得出现 `rd-net`、`rdif-eth`、`ax-sync`、`ax-task` 或
  `axruntime`；不得出现全局 runtime、线程创建、sleep、yield 或 spawn。
- `rdif` feature 只增加便携 capability 依赖，不得引入 `ax-sync`、`ax-task`
  或 `axruntime`，所有队列必须有界且通过所有权转移。
- 带子模块的源码目录使用 `foo/mod.rs + foo/child.rs`；约 400 行开始审视职责，
  800 行以上必须拆分。寄存器字段使用 `tock-registers`，卡协议 wire bits 保持
  强类型编解码，不冒充 MMIO 寄存器。

## 测试边界

纯编解码、状态机、fake MMIO/DMA/host 使用 std host test；私有单元测试在
源文件末尾，`tests/` 只调用公开 API。必须启动 ArceOS/QEMU 的路径使用
axtest。FDT、真实 IRQ、CIS、固件、持续收发和吞吐最终以
`board-licheerv-nano-sg2002-wifi` 实板结果为准，QEMU 不能替代。

参考资料：

- [SDIO Simplified Specification 3.00](https://www.sdcard.org/cms/wp-content/themes/sdcard-org/dl.php?f=PartE1_SDIO_Simplified_Specification_Ver3.00.pdf)
- [Linux SDIO function definition](https://github.com/torvalds/linux/blob/45c13f3f9e3bb15fd89ff2864c6f627a3b4b4229/include/linux/mmc/sdio_func.h)
- [Linux SDIO IRQ lifecycle](https://github.com/torvalds/linux/blob/45c13f3f9e3bb15fd89ff2864c6f627a3b4b4229/drivers/mmc/core/sdio_irq.c)
