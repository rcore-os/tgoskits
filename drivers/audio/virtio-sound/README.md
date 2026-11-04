# VirtIO Sound PCM 核心

该 crate 提供不依赖操作系统的 VirtIO Sound PCM 控制、播放与采集核心。它复用 `virtio-drivers` 的公开 transport 与 `VirtQueue`，自行维护 control、event、TX、RX 四条队列及 VirtIO Sound 控制状态。运行时通过 [PcmCard](../../interface/rdif-audio/src/pcm.rs) 使用能力，不直接接触 transport、队列或设备 DMA 地址。

## 1. 能力边界

卡对象是可移动的单所有者设备核心。所有入队、完成回收和控制操作都通过 `&mut self` 串行化，不要求操作系统锁或调度 trait；运行时只需按期调用 `service()`。

### 1.1 四队列所有权

[VirtioSound](src/device/mod.rs) 独占 control、event、TX、RX 四条队列。control 承载 `PCM_INFO`、`SET_PARAMS`、`PREPARE`、`START`、`STOP` 与 `RELEASE`；event 持续补充设备事件缓冲；TX 和 RX 分别处理播放与采集。[PcmCard](../../interface/rdif-audio/src/pcm.rs) 只暴露 stream 代际、控制 token、数据 token、完成和有限服务预算。

播放提交会把一个完整 period 复制进内部 DMA，完成时返回 token 与帧数。采集在 `PREPARE` 后预挂 RX 缓冲，[`PcmCompletion`](../../interface/rdif-audio/src/pcm.rs) 的采集完成携带可移动 [`PcmBuffer`](../../interface/rdif-audio/src/pcm.rs)；运行时复制数据后通过 [`recycle_capture()`](src/device/session.rs) 归还。缓冲携带 [`PcmBufferOwner`](../../interface/rdif-audio/src/pcm.rs)，卡拒绝来自其它声卡的缓冲。

### 1.2 服务与背压

[service_queues()](src/device/lifecycle.rs) 按 `PcmServiceBudget` 的 control、playback、capture、event 预算推进四队列，不忙等设备响应。完成队列有界；容量不足时 `PcmServiceReport::backpressure` 置位，已完成的设备项保持在队列或 in-flight 状态，直到运行时取走完成，不会先报告成功再丢弃唯一 token。

播放与采集的完成检查覆盖精确 used token、`used_len`、PCM status、帧对齐和 latency 边界。关闭阶段的 status-only 或短帧对齐 RX flush 返回 `CaptureCancelled`，不会把正常 `RELEASE` 当成设备协议错误。

## 2. 构造与生命周期

构造失败只在尚未注册队列时返回普通错误。队列初始化或初始请求提交失败后，构造返回 `Resetting` 状态的卡对象；运行时继续调用 `service()`，确认设备 status 为 0 后才释放队列、控制/event 池、session 和 in-flight DMA。

### 2.1 非安全构造义务

[VirtioSound::new()](src/device/mod.rs) 是 `unsafe` 入口，因为 Rust 无法在运行时证明队列 HAL 与 PCM DMA 使用同一设备地址域。调用前必须满足：

1. `transport` 独占目标声卡，且在确认 status 0 或卡析构前不会被其它驱动重置。
2. `H::dma_alloc`、`H::share`、`H::unshare` 与 `DeviceDma` 描述同一设备地址域。
3. 队列和 PCM buffer 的方向、缓存一致性与实际平台一致。
4. 首版适配使用直接映射、一致性 DMA 且无 IOMMU；这些条件不能在核心内反查。

下面的示例把 `QueueHal`、`transport` 和 `dma` 作为适配层已经验证过的能力；`unsafe` 块列出的是构造契约，不是为了绕过普通类型检查。

```rust
let card = unsafe {
    // Caller has established the four assumptions listed above.
    VirtioSound::<QueueHal, _>::new(
        transport,
        dma,
        now_ns,
        discovery_deadline_ns,
    )?
};
```

适配层负责 PCI 探测、MMIO、DMA 后端、IRQ 或 worker 调度。核心不包含 `virt_to_phys`，也不拥有 StarryOS 或 ALSA 会话。

### 2.2 释放与失败隔离

[`STOP` 与 `RELEASE` 分开处理](src/device/control.rs)；`STOP` 完成后仍继续服务已有 TX/RX/event。`RELEASE` 响应成功后仍保持 `Released` 会话，继续并行回收该方向的 TX/RX；只有 exact used 全部清空、成功 Control completion 可发布且 session slot 已关闭时才报告 release 成功。完成队列背压会延迟这个 token，不会提前成功。之后重新 `open()` 会分配新代际；公开 generation 或 control sequence 耗尽时返回 `TokenExhausted`。

[finish_confirmed_stop()](src/device/lifecycle.rs) 只在读取到 status 0 后释放硬件可见 DMA。未确认 reset 时，队列由 `ManuallyDrop`、在飞请求由 `InFlightDma` 保留，transport 映射也保持到对象销毁；先 Drop 会隔离而不是释放这些资源。

## 3. 验证状态

当前仓库已有契约测试、宿主测试、clippy、HDA 回归、Starry 节点发布，以及当前无诊断 `3/3` profile 证据。数据 mmap errno 边界修正后的正式 profile 已通过，当前快照的最终 `starry-kernel` Clippy 为 `73/73` 通过；该结果仍只覆盖首版列出的 headless ABI/生命周期、播放和非零 tone 采集，不代表完整 Linux PCM 支持。

### 3.1 已取得证据

`cargo xtask clippy --package virtio-sound --package rdif-audio` 的定向检查为 `2/2` 通过。公开 [`recycle_capture()`](src/device/session.rs) 接受 `deadline_ns = 0`，只选择已完成 header/status 的 RX 槽；核心契约覆盖连续 8 次 RX 回收、后续 8 个 completion，以及非零 RX 超时、控制超时、reset 前 DMA 安全这组 `4/4` 路径。正式 `cargo xtask test --since` 选中 17 个受影响包并全部通过，其中 `virtio-sound` 的四个公开生产 core/VirtQueue 契约用例通过：

- `discover_tx_rx_and_confirmed_shutdown`
- `control_timeout_keeps_dma_until_reset_confirmed`
- `bad_used_length_quarantines_until_confirmed_reset`
- `release_waits_for_capture_drain_and_cancels_short_rx`

这些用例位于 [tests/contract.rs](tests/contract.rs)，使用真实 `VirtQueue`、生产 `PcmCard`、受控 transport 与 dma-api 后端。RELEASE 用例使用 8 period capture 和每轮 `capture_ops = 1` 的预算：提前发布成功会在 `1/8` 停止，当前实现完成 `8/8`。这证明核心不会在 TX/RX used buffer 尚未回收时提前报告 `RELEASE` 成功。

Starry 外部启动配置另有两条真实 guest 结果：audio feature 关闭时，挂载 `virtio-sound-pci,streams=2` 后三个 `/dev/snd` 节点均不存在；audio feature 开启时，`controlC0`、`pcmC0D0p`、`pcmC0D0c` 均发布为字符设备。这两条结果证明 PCI 发现、服务线程和节点发布边界，不证明任何 PCM ioctl、数据读写或媒体播放。

headless 的原版 ALSA 1.2.13 prebuild 已成功，下载、固定 SHA 校验和 guest 工具检查完成。当前正式 headless 整体通过，覆盖原版工具、control/PCM ioctl、短读写、`SYS_mmap` 精确数据 errno、status/control fallback、poll/ppoll/epoll、dup/fork、last-close、SIGKILL/reopen、DRAIN/XRUN 和同一 OFD 阻塞 read 与 `DROP`。当前 playback 通过原版 `aplay`，宿主输出 `PCM_WAVEFORM_OK frames=4096 bytes=16384`。独立 ALSA link directory 和交叉工具链自带 libc 避免两个 musl 初始化段进入同一 ELF，ALSA 源码未修改。

当前无诊断 capture profile 完成纯录 96000 帧、全双工 96000 帧和受控重录 48000 帧，频率约为 `997 Hz`，并输出 `VIRTIO_SOUND_CAPTURE_PASSED`。同一轮 headless、playback 与 capture-duplex 汇总为 `3/3` 通过。

### 3.2 尚未覆盖

`virtio-sound` 已加入宿主标准测试白名单 [std_crates.csv](../../../scripts/test/std_crates.csv)。当前集成快照的 `starry-kernel` clippy 为 `73/73` 通过。采集失败传播区分 running 非 fatal XRUN 与 fatal 整卡 cleanup：fatal 时 worker 的 [`handle_capture_recycle_error()`](../../../os/StarryOS/kernel/src/sound/worker.rs) 先把当前 `WorkerSession` 回插 session 表，再由 [`fail_device()`](../../../os/StarryOS/kernel/src/sound/worker/completion.rs) 清理所有 session，不对出错 buffer 本身做回插。数据 `MAP_SHARED` mmap 在 OPEN/Failed 返回 `EBADFD`、configured 且未广告 MMAP 时返回 `ENXIO`；status/control metadata 仍回退 `SYNC_PTR`。`MAP_PRIVATE` 数据映射仍经 `file_mmap()` 返回 `ENODEV`，是首版已知的 Linux data guard 差异。未直接覆盖的 ioctl 参数、故障顺序和完整 Linux PCM 能力仍是兼容性缺口。PA fixture 不使用固定 2 秒 source latency 的 `module-sine-source`，而由独立 tone sink、`module-sine` 和 `module-remap-source` 构成实时输入。
