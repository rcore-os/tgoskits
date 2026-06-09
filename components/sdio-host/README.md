# sdio-host

SDIO 主机控制器 trait 抽象。`#![no_std]`，无外部依赖。

上层驱动（如 aic8800 WiFi）通过 `SdioHost` trait 访问 SDIO 总线，平台驱动（如 sdhci-cv1800）负责实现。`SdioCardIrq` trait 提供中断上下文安全的卡中断屏蔽/恢复操作。

## 核心 trait

### `SdioHost`

| 方法 | 说明 |
|---|---|
| `init(&mut self)` | 控制器初始化 + SDIO 卡枚举 (CMD5→CMD3→CMD7) |
| `read_byte(func, addr)` | CMD52 单字节读 |
| `write_byte(func, addr, val)` | CMD52 单字节写 |
| `write_byte_read(func, addr, val)` | CMD52 RAW 模式写后读 |
| `read_fifo(func, addr, buf)` | CMD53 FIFO 读 (fixed address) |
| `read_fifo_inc(func, addr, buf)` | CMD53 递增地址读 |
| `write_fifo(func, addr, buf)` | CMD53 FIFO 写 (fixed address) |
| `write_fifo_inc(func, addr, buf)` | CMD53 递增地址写 |
| `set_block_size(func, size)` | 设置 function block size |
| `set_clock(hz)` | 设置总线时钟频率 |
| `enable_func(func)` | 使能 SDIO function |
| `vendor_device_id()` | 返回 (vendor_id, device_id) |
| `enable_irq()` / `disable_irq()` | 中断信号开关 |
| `card_irq_ctrl()` | 返回 `Arc<dyn SdioCardIrq>` |

### `SdioCardIrq`

ISR 安全的中断控制，用于在中断上下文中屏蔽/恢复卡中断，不持锁、不分配堆。

## 模块

- `error` — `SdioError` 枚举 (Timeout / CrcError / IoError / Unsupported)
- `cccr` — CCCR / FBR 寄存器地址与 CIS tuple 常量
- `cmd` — CMD52/CMD53 参数构造常量、R5 响应标志、OCR 掩码

## 依赖关系

```
aic8800 (WiFi 驱动) --uses SdioHost--> sdio-host <--impl SdioHost-- sdhci-cv1800 (平台驱动)
```
