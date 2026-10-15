# 虚拟 UART 接收进展

`axdevice` 的 `Uart16550` 和 `Pl011` 使用固定 256 字节 RX FIFO，通过 `SerialBackend` 接收应用层字节。接收进展由输入通知与客户机消费共同驱动：通知启动一次接收，消费释放的空间继续接收余量，不要求每个字节对应一次 vCPU 唤醒。

## 1. 进展边界

`GuestConsoleMux` 保存输入字节，`RunSignals` 只保存可合并的工作标志。FIFO 中已有数据时，UART 必须能靠客户机的正常寄存器读取推进到 backend 尾部，不能把工作标志误当作输入计数。

### 1.1 通知合并

输入先入 `GuestState.input`，随后 `AxvmManager::notify_vm()` 经 `notify_devices()` 发布工作。`RunServices::poll_devices(false)` 先消费工作标志，再调用设备的 `poll()`。如果 73 字节在一次 poll 前已全部入队，旧实现只取前 64 字节；普通 RBR/DR 读取只消费本地 FIFO，剩余 9 字节会等到下一次输入、设备工作或显式 poll 才被接收。

这个序列证明组件存在进展缺口，但不足以判定某次 QEMU 超时或串口停点的原因。宿主发送、物理 RX、应用输入队列、vCPU 调度和虚拟 UART 的现场证据需要分别核对。

### 1.2 空间释放

`Uart16550::read()` 与 `Pl011::read()` 在数据寄存器消费后 FIFO 为空时调用 `fill_rx()`；16550 的 FCR 清 RX 也释放空间并尝试续读。每次只读一批，不循环读到 backend 为空，不增加定时器或合成 RX 中断。FIFO 有数据时，客户机继续消费即可再次触发这一过程。

QEMU v10.1.0 的 [16550 RBR 读取](https://github.com/qemu/qemu/blob/v10.1.0/hw/char/serial.c#L439-L463)和 [PL011 DR 读取](https://github.com/qemu/qemu/blob/v10.1.0/hw/char/pl011.c#L231-L251)也在消费后调用 `qemu_chr_fe_accept_input()`，由容量控制后续输入。本实现保留既有简化模型：16550 loopback 不屏蔽外部 backend 接收，PL011 的 RX enable 位只影响既有中断条件；不宣称完全复现 QEMU 或物理硬件的 gating。

## 2. 所有权与并发

`RxFifo` 管理已接收字节和在途读取的预留容量；`SerialEndpoint` 管理读取串行化与 IRQ 发布。所有入口都在 vCPU 任务上下文使用可睡眠 mutex，后端读写和控制器调用不持 UART 状态锁。

### 2.1 接收读锁

`poll()` 通过 `SerialEndpoint::lock_rx()` 等待已有 backend read 结束，再重新检查 FIFO 空间。这保留了本次工作通知的义务：即使前一读取已经观察到 backend 为空，后来入队字节对应的 poll 也不会因“已有读在途”而直接返回。

寄存器消费和 FCR 清除通过 `try_lock_rx()` 续读；已有读取在途时，它们只刷新当前 IRQ，由在途读负责提交和再次发布电平。`is_locked()` 的快退避免同一任务的 backend 回调递归尝试读锁被 native lockdep 拒绝，真正取得所有权仍依赖 `try_lock()`。backend 可以重入寄存器访问，但不得从 read 回调递归调用同一 UART 的 `poll()`。

### 2.2 容量与清除

持有接收读锁后，`RxFifo::begin_read()` 在短暂状态锁内预留 `min(64, FIFO 剩余容量)`，`SerialEndpoint::poll_rx()` 只给 backend 这一长度的切片。`RxFifo::finish_read()` 把实际返回的前缀提交到 FIFO 后释放预留；FIFO 满时不调用 backend，余量保留在原队列。正常接收不会先从 backend 取走字节再因本地容量不足丢弃。

PL011 的 poll 路径因此不会产生 RX FIFO overrun，当前模型不再向客户机上报 OE。`SerialBackend::read()` 只传递字节，没有物理接收错误元数据；`Pl011State::receive_error` 仅初始化为零和清零，DR/RSR 读取的接收错误状态保持为零。这一边界不等于物理串口没有溢出，也不影响 16550 对本地 loopback 写入容量不足的既有 overrun 处理。

在途读期间，RBR/DR 可消费已提交字节，loopback 只能占用未预留空间。FCR 清除只删除已提交字节并保留预留；读返回时作为后续到达追加在清除之后的 loopback 字节后面。这样已接受字节的提交顺序和容量上限都由同一状态锁确定。宿主测试中的 backend unwind 由 `RxCompletion` 释放预留，接收读锁由 guard 释放，不留下永久 busy 状态。

对于容量为 `N` 的 `RxFifo`，在途读取期间 loopback 可用的总容量上限为 `N - reserved`。因此，即使已提交字节尚未填满 FIFO，16550 的 loopback 写入也可能被拒绝并报告 overrun；该状态表示本次 loopback 字节被丢弃，不表示已提交字节被覆盖。即便 backend 随后返回零字节，写入时仍须保留其尚未释放的容量。

### 2.3 IRQ 发布

锁顺序为接收读锁到短暂 UART 状态锁，或 IRQ 发布锁到短暂 UART 状态锁；持状态锁时不取得这两把外层锁。`refresh_irq()` 取得发布锁后才重新读取当前条件，然后在状态锁外调用 `IrqLine`，避免旧快照成为最后发布值。生产 sink 只进入控制器状态、事件队列和唤醒路径，不同步回入 UART；`IrqLine` 自身已有 source/input 串行门，同样不支持 sink 同步重入。

顺序 THR 写仍先消费 TX-empty，再在 backend 返回后置为完成，组件级线路保留低、高两次发布。并发完成用 pending level 表示，控制器队列也可以合并同源 level；这里不提供逐 TX 请求的边沿计数保证。16550 原有 TX 背压边界不由本次 RX 修复改变。

## 3. 回归证明

测试保留在 `axdevice` 的现有串口功能模块，使用真实 UART、FIFO、`SerialEndpoint` 和 `IrqLine`，只替换字节 backend 与控制器 sink。宿主测试证明组件状态和锁顺序；native mutex、控制器 ingress 与客户机执行仍需定向 Axvisor QEMU 证据。

### 3.1 功能与交错

已有两种 UART 的收发测试增加一次输入 73 字节和 529 字节的场景，前者只启动一次 poll，后者在客户机消费前多次 poll。之后仅通过正常状态寄存器与数据寄存器读出全部原序字节，检查无 RX overrun，结束后 IRQ 为低。FCR 功能测试验证只丢弃已进入 256 字节 FIFO 的内容，backend 余量继续接收。

独立并发测试把首个 backend 空读暂停在提交前，再入队新字节并启动第二 poll。既有宿主 mutex provider 的线程局部一次性通知只在真正等待前发送事件，区分等待与直接返回；释放首个读后，第二 poll 必须接收并拉高 IRQ。另一项测试覆盖预留容量、并发 FCR/loopback、backend 同步重入寄存器与 unwind 后恢复；顺序 THRE 的既有 `[true, false, true]` 断言继续保留。

### 3.2 执行链与边界

`scripts/test/std_crates.csv` 已包含 `axdevice`，`scripts/axbuild/src/test/std.rs` 负责由 `cargo xtask test --since <基线>` 选择受影响软件包、执行真实测试并汇总失败。红绿验证需保留相同增强测试，临时移除消费续读或把通知 poll 改为 busy 时跳过，分别确认完整字节判定和实际等待判定失败，再恢复修复验证通过；格式化、定向 Clippy 和实际 QEMU 结果按执行记录报告，不把计划作为通过证据。
