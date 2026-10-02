# VirtIO block 请求与停机验证

## 1. 复位边界

此用例通过公开的 `BlockController` 和 `HardwareQueue` 接口，验证 `ax-driver` 的 VirtIO MMIO 控制器在仍持有请求时停止设备。QEMU 提供实际的块设备，ArceOS 提供分配器、锁和计时器；不使用宿主机运行时替身。

### 1.1 正常请求

`exercise_rejected_directions()` 先为读请求提供 `ToDevice` 缓冲区、为写请求提供 `FromDevice` 缓冲区，要求驱动在发布描述符前以 `InvalidRequest` 拒绝，并将请求留在原批次中。`exercise_io()` 随后使用同一生产驱动，注册真实 IRQ 回调后依次写入非零数据、刷新并读回比较。回调只确认中断和发布原子事件，任务收到事件后才调用 `drain_completions()`；缺失中断或完成状态错误会使测试失败。后续请求复用同一队列，验证描述符被正确回收。完成后注销回调，再让该队列进入停机验证。刷新请求故意携带非零 LBA，transport 在通知设备前读取真实队列中的请求头，确认驱动将 Flush 的 sector 编码为零。

### 1.2 成功停机

`exercise_stop()` 向真实队列提交读请求，保留未排空的完成项，再触发 `QuiesceIrqs` 或 `Watchdog`，再由队列关闭接口回收请求。只有 `TransportControl::stop()` 读回设备状态零，驱动才能报告 `ControllerState::Shutdown` 并将原 `CpuDmaBuffer` 返回。用例检查请求只完成一次、重复关闭队列不会再次完成请求，并复用返回的缓冲区验证停止后拒绝提交且保留调用方所有权。

复位确认依据 [VirtIO 1.2 第 2.4 节](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html)：设备通过状态零确认复位后，在重新初始化前不得再通知驱动或访问队列。当前驱动不会自动重新初始化。

### 1.3 失败隔离

`ResetFaultTransport` 复用生产 MMIO 探测入口，用 `Fault` 枚举选择协商拒绝、延迟复位或拒绝复位；其余操作转发给真实 `MmioTransport`。延迟模式暂时隐藏已完成的复位，验证 `RegisterPending` 重试期间不能回收请求；拒绝模式吞掉运行后的复位写入，验证控制器不会虚报终态、队列析构保留 DMA 页。队列地址、通知和配置访问仍交给真实 transport。

`RawBlock` 通过 `ManuallyDrop` 保留未确认停止的队列内存，`VirtioBlockQueue::drop()` 同时保留未完成请求。析构只做有限次寄存器访问，不调用带无界等待的 `queue_unset()`。拒绝复位的设备在此测试中有意留下隔离资源，随后由整个 QEMU 进程退出回收。

### 1.4 协商拒绝

额外的真实设备通过 `ResetFaultTransport` 隐藏 `FEATURES_OK`，模拟设备拒绝驱动选择的特性。测试要求 `register_fdt_transport()` 返回错误，且记录该路径确实执行。`RawBlock::new()` 必须在创建队列和设置 `DRIVER_OK` 前检查现代设备的确认状态；旧版 MMIO 没有此握手，不执行该检查。

## 2. 执行证据

用例由 ArceOS Rust 套件的功能选择器识别，软件包为 `arceos-test-suit`，功能为 `virtio-block-lifecycle`。构建配置复用 Rust 套件；虚拟设备配置位于本目录。

### 2.1 本地运行

在仓库根目录执行项目入口，输出必须包含 `ARCEOS_TEST_END feature=virtio-block-lifecycle` 和 `ArceOS test suite run OK!`，且外层汇总报告用例通过：

```bash
cargo xtask arceos test qemu --arch aarch64 --test-group rust --test-case virtio-block-lifecycle
```

正常读写设备使用 QEMU `null-co` 后端上的临时快照保存写入，其余故障设备直接使用 `null-co`。无需预制磁盘镜像；QEMU 退出后自动清理快照，运行环境需要允许写入 QEMU 的系统临时目录。`qemu-aarch64.toml` 的失败匹配会把断言失败传播为任务失败。

### 2.2 持续集成

现有 AArch64 Rust 套件持续集成会通过 `ARCEOS_RUST_STANDALONE_FEATURES` 单独启动此用例，使设备所有权与其他文件系统测试隔离。测试路径沿用已有的 `rust/cases` 路由。

本用例仅配置 AArch64 QEMU `virt`，覆盖现代 VirtIO MMIO 设备的请求、真实 IRQ、协商拒绝和复位故障。该测试范围不代表驱动只能用于 AArch64；其他架构、PCI transport、实体板卡和自动恢复不由此用例证明。
