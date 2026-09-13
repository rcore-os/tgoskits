# PL011 early console 有界启动

## 问题

`Pl011::open()` 会先关闭 UART，再等待 `UARTFR.BUSY` 清零。早期启动阶段尚不能假设系统定时器可用；如果硬件、固件或映射异常使 `BUSY` 永久置位，无界轮询会让系统在输出诊断信息之前永久卡住，而且 `UARTCR` 已经被修改。

## 接口与失败语义

`Pl011::open()` 返回值由 `()` 改为 `Result<(), ConfigError>`。这是有意的 breaking API：调用者必须决定初始化失败后是否退出、选择其他控制台或继续无控制台启动。当前可报告的失败是已有的 `ConfigError::Timeout`，不新增重复的错误类型。

`open()` 使用固定次数的寄存器轮询预算，不读取系统时钟，也不依赖中断。这样同一代码可以在 MMU、时钟源和运行时尚未完成初始化时使用。预算只限制启动阶段的 `BUSY` 等待，不改变运行期 `set_config()` 的现有语义。

## 回滚与发布

轮询前完整保存 `UARTCR`，然后关闭 UART。若预算耗尽，驱动把 `UARTCR` 原值完整写回，再返回 `ConfigError::Timeout`；在此之前不修改 FIFO、interrupt mask 或其他配置寄存器。因此失败不会留下半初始化的控制寄存器状态。

someboot 只有在 `open()` 成功后才发布 PL011 early-console 实例：

- cmdline `earlycon=pl011,...` 把 timeout 映射为明确错误，并保持原有 early-console/debug 状态不变；
- FDT `stdout-path` 路径把失败转换为 `None`，允许现有启动流程继续，但不安装失败实例，也不更新 debug MMIO 状态；
- `UartPort::startup()` 原样传播 `ConfigError`，由运行时调用者决定策略。

NS16550、PL011 文件结构以及运行期 `set_config()` 不属于本次修改范围。

## 验证

最低层回归使用模拟寄存器永久保持 `UARTFR.BUSY`，验证调用在固定预算后返回 `ConfigError::Timeout`，并验证 `UARTCR` 与调用前逐位一致。集成验证覆盖 some-serial 单元测试、someboot 编译检查，以及 AArch64 上 ArceOS、StarryOS 和 Axvisor 的启动路径。
