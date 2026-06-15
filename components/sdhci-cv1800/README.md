# sdhci-cv1800

Sophgo CV1800/SG2002 SDHCI 控制器驱动，为 SDIO WiFi 设备（如 AIC8800）提供总线传输。

实现 `sdio_host::SdioHost` 和 `sdio_host::SdioCardIrq` trait。

## 模块

- `lib.rs` — SDHCI 命令处理、CMD52/CMD53、PIO 数据传输、`SdioHost` 实现
- `hw_init.rs` — SoC 级硬件初始化 (Pinmux、时钟、复位)
- `irq.rs` — 中断状态管理、ISR handler、waker 注册
- `regs.rs` — SDHCI 寄存器偏移与位定义

## 用法

```rust
use sdhci_cv1800::CviSdhci;
use sdio_host::SdioHost;

let mut sdhci = CviSdhci::new(0x0500_0000);
sdhci.init()?; // CMD5→CMD3→CMD7→高速→4-bit→使能 Func1

let val = sdhci.read_byte(1, 0x0A)?;
sdhci.write_fifo(1, 0x00, &tx_buf)?;
```

## 设计要点

- **init 阶段**：轮询 MMIO 寄存器等待就绪（ISR 尚未注册）
- **运行阶段**：ISR 做 W1C + 设标志，`wait_*` 检查 `AtomicBool`
- **数据传输**：PIO 模式，逐块读写 Buffer Data Port
- **时钟**：400KHz (枚举) → 50MHz (高速) 或 25MHz (默认)
- **总线**：4-bit 模式，卡检测通过 HOST_CTL1 寄存器覆写

## 硬件地址 (SG2002 SDIO1)

| 模块 | 基地址 |
|---|---|
| SDIO1 控制器 | 0x0500_0000 |
| CRG | 0x0300_2000 |
| SYSCTRL | 0x0300_0000 |
| RTCSYS_CTRL | 0x0502_5000 |
| RTCSYS_IO | 0x0502_7000 |
