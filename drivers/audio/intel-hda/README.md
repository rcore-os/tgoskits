# Intel HDA 控制器核心

该 no_std crate 提供 Intel High Definition Audio 控制器初始化、CORB/RIRB 编解码器命令传输、模拟音频路由发现，以及固定格式的 PCM 播放流。当前仅接收 Intel 厂商、HDA PCI 类别 04:03:00 的功能；控制器必须声明至少一个输出流，并支持本实现要求的 256 项 CORB/RIRB 环。播放流 descriptor 位于所有输入流 descriptor 之后，按 GCAP 输入流数选择偏移。其他厂商、数字显示音频、HDMI ELD、GPU/显示接线、音频中断运行时和用户态音频接口不在本模块范围内。

平台适配层负责 PCI 枚举、BAR 资源占有与映射、DMA 域配置、计时能力和音频设备注册。BAR 映射通过 mmio-api 的 Mmio 所有权传入 MappedHdaIo；DMA 队列和样本缓冲通过 dma-api 的设备级能力分配。模块不读取启动参数，不引用内核全局状态，也不承担 PCI 发现、任务调度或操作系统锁。调用者必须串行化同一控制器的所有操作，不能在硬中断上下文调用同步编解码器命令。

创建控制器会复位设备、启用命令环、探测编解码器并配置一个可用的模拟输出路径；成功后，播放接口只接受固定为 16-bit stereo、48 kHz 的四个 4096-byte period。`submit` 在接受新 period 前同步轮询硬件位置；若发现有完成 token 尚未由 `complete` 取走，会返回 `Again` 且不接受本次数据，调用方应先反复调用 `complete` 取走已完成 token，再重试。`prepare` 不会丢弃未取走的完成 token；需要取消时调用 `abort`。无支持路由、DMA 能力不足、状态超时或回读异常时返回错误。shutdown 只有在播放流、CORB/RIRB 与全局复位都通过状态回读确认后才释放可能被设备访问的 DMA 内存；无法证明设备已停止时会保留该内存而不返还给分配器。

HDA 寄存器和命令协议以 Linux 7.2.3 的 HDA 上游实现及规范行为为对照。该 crate 使用 Apache-2.0 许可；Linux GPL 实现代码未复制到本 crate。模块行为测试和编译检查不能证明物理 Intel HDA 硬件兼容；当前没有 TGOSKits 上的实体板卡验收。


## QEMU 验收边界

`test-suit/arceos/drivers/intel-hda` 在 Q35 上使用 `intel-hda`、`hda-duplex` 和 `wav` 音频后端，实际探测 PCI/BAR 并运行生产控制器的 MMIO、CORB/RIRB、DMA 播放及关闭路径。用例通过系统注册的通用播放接口提交四个非零 PCM period，要求完成 token 与提交顺序一致、无流错误，校验完整输出波形，并成功 release/shutdown；缺设备或失败会使测试失败。

```sh
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case intel-hda
```

结果证明 QEMU 控制器与 DMA 播放生命周期，不证明可听输出、HDMI/ELD 或实体硬件兼容。后两类工作不在本次验收范围。

## 通用播放能力与 ArceOS 接入

`rdif` 功能实现 `rdif_audio::Playback`，保留单一控制器所有权、复制提交、完成 token、取消和失败隔离。不依赖 ArceOS，也不建立音频全局状态。当前广告配置为 S16_LE、48 kHz、双声道、1024-frame period、4 个 period；其他配置在硬件写入前拒绝。

ArceOS 的 `ax-driver/intel-hda` 是显式启用的生产 PCI 适配器：使用既有设备发现、MMIO 映射和设备级 DMA 域绑定，将控制器注册到 `rdrive`。设备对象经 `take_playback_devices()` 一次性移交给调用方。此版本使用有界轮询，PCI INTx 与 HDA INTCTL 均不启用；不注册未拥有的中断。

QEMU 用例不再在测试探测回调中直接操作控制器，而是在启动后使用通用播放能力。`wav` 后端记录输出，任务工具为每次运行创建独立临时文件并自动删除；4096 帧以帧位置和声道编码，校验完整连续波形与前后静音，从而发现遗漏、重排、重复或样本改写。通过复制后立即清零调用方缓冲区，实际输出同时检验复制提交契约。取消和 shutdown 也通过公共接口验证。

QEMU 参数参考 [QEMU 音频后端文档](https://www.qemu.org/docs/master/system/invocation.html)。该验证证明公共播放接口、系统发现/注册、实际 DMA 与 QEMU 输出样本，不证明物理可听声音或真机兼容性；不迁入完整 ALSA/OSS、HDMI 或录音。
