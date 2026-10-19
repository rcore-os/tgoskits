# PL011 中断接收回归

## 1. 真实接收链路

`serial-rx` 使用 `console::take_input()` 与 `TaskConsoleInput::read()` 检查接收内容，要求 `console::is_active()` 选中的设备为具有 IRQ 的 `PL011 UART`。原始 HAL 控制台或轮询串口不能使本用例通过。

### 1.1 固定晚到字节

宿主 `SerialRxFixture` 为 QEMU 建立独占 TCP 串口与 GDB 连接。`Debugger::inject_after_empty()` 使用 QEMU 的 MMIO 读观察点计数 `UARTDR` 读取，在前 63 字节读完、`read_rx_sample()` 已判断 `RXFE` 后，停在读取 `UARTRSR` 的指令前。此时宿主发送第 64 字节 `?`，确认 `UARTMIS.RX` 已置位，再移除观察点并恢复执行。寄存器地址来自本次启动的 PL011 映射日志；不依赖固定内核装载地址、DWARF 行号或生产测试钩子。

63 字节超过 QEMU PL011 的 16 字节 FIFO，同时小于 `IRQ_RX_BATCH_CAPACITY = 64`，使此次排空进入空 FIFO 分支。旧处理器在返回该分支后清 RX 状态，晚到字节仍留在 FIFO 却失去中断；修复后它经后续 IRQ、生产 worker、订阅队列和阻塞读取到达消费者。不能用任意长串或重复运行代替这个受控窗口。

### 1.2 后续接收

客户机逐字节比较第一批全部 64 字节和 UART 错误标志，排除多余数据后才打印 `SERIAL_RX_READY_2`。宿主随后独立发送第二批 30 字节，客户机再次验证完整内容。宿主还要求最终 `ArceOS test suite run OK!`；内容错误、panic、连接中断与超时均向外层返回失败。

### 1.3 无中断通知的诊断

两批接收后，客户机保持日志订阅关闭，先等待已发布日志离开 mailbox，再通过现有
`task_output().wait_idle()` 排空当前日志和 UART，避免前条输出的 TX IRQ 干扰。随后等待真实
串口 worker 处于 `Blocked` 且 IRQ waiter 已登记，再经 `publish_deferred_when_blocked_for_test()` 只写入一条 deferred 记录。
此后不发布普通日志、串口输入或控制命令；本地等待超时必须让 worker 进入真实
`LogReader::take()` 并取得完整唯一记录。五秒截止前检查消费端的 FI 观测标志，不能用
发布端入队或日志订阅的直接转发冒充 worker 消费。消费同轮还必须观察到真实
`wait_timeout()` 返回超时；如果是残留 TX IRQ 等其他唤醒抢先消费，测试立即失败。

用例启用 `fault-injection`，观察接口仅在该 feature 下编译；普通 IRQ 串口通知路径不变。
成功时打印 `SERIAL_DEFERRED_DIAGNOSTIC_OK`。

## 2. 执行条件

用例通过 Rust suite 的 feature、runner 和 axbuild 发现器接入，独立于 `all` 内核运行，仅列入 aarch64 矩阵。复用 suite 的 aarch64 build 配置，并通过本目录 `qemu-aarch64.toml` 选择 PL011 virt 机器。宿主目前需要 Linux，以便 QEMU chardev 的 `/dev/stdout` 日志沿现有成功、失败匹配链路传播；无需安装 GDB 客户端。

### 2.1 项目入口

正常入口同时运行受控晚到字节和后续输入检查，超时为 20 秒，不进行重试。

```bash
cargo xtask arceos test qemu --test-group rust --test-case serial-rx --arch aarch64 --list
cargo xtask arceos test qemu --test-group rust --test-case serial-rx --arch aarch64
```

### 2.2 缺陷敏感度

将 `Pl011Irq::handle()` 恢复为基准分支的排空后 `uarticr.set(active)`，同一用例会在已确认 `RXMIS` 置位的注入之后超时，外层返回非零。修复实现排除 RX/RT 的显式清除，两批接收均通过。将第二批预期首字节改错，会在 `serial phase 2, byte 0` 断言失败，外层同样返回非零；验证后必须恢复预期内容。

将 `SerialWorker::run()` 的 diagnostic fallback 临时禁用，同一 deferred 检查应在截止时失败，不能把普通日志或无关 RX 当作进展来源。恢复实现后再运行同一用例。
