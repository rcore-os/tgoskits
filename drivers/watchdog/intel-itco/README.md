# Intel iTCO 看门狗寄存器核心

该 `no_std` crate 仅接纳 Intel PCI ID `8086:2918`（ICH9 TCO v2）和 `8086:54a3`（TCO v6），负责校验芯片资源并实现带寄存器读回的状态转换；不据此推断其他代际兼容。

平台适配层负责 PCI 探测、端口授权、GCS 映射、看门狗注册、串行访问和启动策略。构造控制器不会访问硬件或启动计时器。调用方必须显式选择 `start(timeout)` 或 `adopt_running(timeout)`，并按自身生命周期策略停止设备。

## QEMU 验收

Rust suite 的 `intel-itco-qemu` 用例使用 Q35 ICH9 v2：接管并停止 QEMU 初始运行的计时器，再验证启动、倒计时、ping 重载和停止后的 `HALT` / `NO_REBOOT` 读回。该用例不添加 NVMe 设备。

```sh
cargo xtask arceos test qemu --test-group rust --test-case intel-itco-qemu --target x86_64-unknown-none
```

用例不等待或触发看门狗超时，因此不验证复位行为。v6 与实体硬件尚未验证。`boot_status()` 报告的是 v2 legacy `BOOT_STS`；v6 返回 `false` 不代表已确认没有发生超时。

状态机参考 Linux `drivers/watchdog/iTCO_wdt.c` 和 `drivers/mfd/lpc_ich.c` 的行为；实现未复制 Linux GPL 源码。
