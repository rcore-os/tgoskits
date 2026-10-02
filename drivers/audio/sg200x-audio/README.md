# SG200x 麦克风采集

`sg200x-audio` 提供板载 ADC 左声道的单声道 S16_LE 采集，支持 16 kHz、48 kHz。硬件核心管理 ADC、I²S 和 DMA；`ax-driver::audio` 负责板级资源与 IRQ，StarryOS `pseudofs::dev::audio` 提供用户接口。

## 1. 硬件接口

`Capture` 持有控制与采集队列，`Interrupt` 确认中断并锁存错误。平台通过 `Resources` 传入 MMIO 映射与晶振频率，通过 `DeviceDma` 提供 DMA 内存。

### 1.1 时钟与输入

`Frontend::prepare` 从固件留下的 MIPIMPLL、APLL 和 synthesizer 寄存器计算实际父时钟，只调整 AUD0/AUD3 分频，不重编程共享 PLL。无法精确产生目标时钟时返回 `Error::Clock`。ADC 先输出 BCLK，再等待 I²S 接收器复位完成；复位超时返回错误。

`set_gain` 接受 0..=24，对应 0..48 dB、每步 2 dB。零表示最低增益，不表示静音。输入接 ADC 左路，I²S0 使用单 slot、16 位内存格式，I²S3 只提供 ADC 所需的 MCLK。

### 1.2 DMA 所有权

`DmaRoute` 区分物理通道和握手请求号。`Ring` 使用 64 字节对齐的描述符、128 字节硬件块和一致性音频缓冲区；一个通知周期可以含多个硬件块。`Config` 要求周期为 64 帧的倍数，范围 64..8192 帧，缓冲区包含 2..16 个完整周期且不超过 32768 帧。缓冲区必须大于一个周期加 `Config::DMA_BLOCK_FRAMES`，为首次周期通知留出服务余量；64 帧周期因此至少需要 192 帧缓冲区。

`Cursor` 根据 DMA 目的地址累计进度，按地址变化而非中断次数判断周期。无法排除漏过整圈时报告溢出；消费方在复制后再次检查进度，拒绝已被覆盖的数据。DMA 内存通过易失访问读取，避免引用设备正在写入的区域。

`stop` 中止本通道并有界等待 `CH_EN` 清零，保留共享控制器状态。超时后保留 DMA 内存，将对象置为不可重用状态，以免释放设备仍可能访问的内存。探测阶段在没有活动 DMA 通道时设置共享路由。

## 2. 系统接入

StarryOS 的 `sg2002-audio` feature 启用设备探测和 ALSA 录音入口。硬件资源与 IRQ 注册成功后，系统发布声卡节点。

### 2.1 录音会话

`AudioFile` 是打开文件描述的所有者：一个独立采集打开占用设备，另一个返回 `EBUSY`；`dup` 和 `fork` 共享同一个 `Arc`，最后引用关闭时停止采集。`Stream` 维护参数、状态、硬件进度和应用进度，支持参数协商、准备、启动、读取、停止、排空和溢出后的重新准备。

参数重设或释放后，`Card::change_stream` 在解锁后通知等待者，使阻塞读取和 `poll/select` 观察到状态变化。`HW_PARAMS` 在配置阶段失败时撤销旧参数并回到 `Open`。捕获 `DRAIN` 停止运行中的流；有余量时进入 `Draining`，保留已在进行的读取直至短读结束。`DROP` 和溢出后丢弃的数据保持不可读；`Draining` 状态下新发起的读取返回 `EBADFD`，非阻塞 `DRAIN` 完成状态操作后返回 `EAGAIN`。

`pcmC0D0c` 提供交错帧读取、状态、`SYNC_PTR` 和 `poll`；`controlC0` 提供声卡查询及单值 `ADC Capture Volume`。当前接口面向录音，不含播放或音频 mmap。硬中断通知固定服务线程，由服务线程唤醒读者。用法和板测入口见 [录音诊断](../../../apps/starry/sg2002-audio/README.md)。

### 2.2 参考资料

寄存器配置参考 Sipeed SDK 提交 `d4003f15b35d43ad4842f427050ab2bba0114fa5` 的 [ADC](https://github.com/sipeed/LicheeRV-Nano-Build/blob/d4003f15b35d43ad4842f427050ab2bba0114fa5/linux_5.10/sound/soc/cvitek/cv181xadc.c)、[I²S](https://github.com/sipeed/LicheeRV-Nano-Build/blob/d4003f15b35d43ad4842f427050ab2bba0114fa5/linux_5.10/sound/soc/cvitek/cv1835_i2s.c) 和 [DMA](https://github.com/sipeed/LicheeRV-Nano-Build/blob/d4003f15b35d43ad4842f427050ab2bba0114fa5/linux_5.10/drivers/dma/cvitek/cvitek-dma.c)。原实现版权归 CVITEK 等原作者；本软件包采用 GPL-2.0-or-later。Linux ABI 以 v6.6 为基准，消费者调用链对照 alsa-lib v1.2.14。

### 2.3 实板结果

LicheeRV Nano 上，两档 `arecord` 采集和生命周期诊断均已通过。首次采集的录音文件保留了约两秒的启动瞬态。采样率、增益和音质尚未使用受控声源校准。
