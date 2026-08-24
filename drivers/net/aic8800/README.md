# aic8800

AIC8800 系列 WiFi 芯片驱动核心，通过 SDIO 总线通信。**OS 无关**：核心代码不直接
依赖任何操作系统运行时；定时、休眠、让步、任务派生等能力通过 `aic8800::WifiRuntime`
trait 注入，由上层 OS glue 在初始化时调用 `aic8800::set_runtime` 安装。

队列中断路径当前支持 AIC8801、AIC8800D80、AIC8800D80X2。AIC8800DC/DW
的 command/FIFO function ownership 与本地 vendor 驱动证据不一致，probe 会明确
失败；不会回退到 kicker 或周期轮询。

## 用法

平台相关的资源（MMIO 映射、SDHCI 枚举）由上层 OS glue 负责；本 crate 从一个已
就绪且 IRQ source 尚未取走的 SDIO host 开始完成芯片侧 bring-up，并返回可消费一次的
`AicWifiNetDev`。`NetDevice::into_parts()` 将它拆成 RX/TX queue、SDHCI hard-IRQ
endpoint、task-context rearm control、general control 与 owner-CPU Wi-Fi control。

```rust
// 1. OS glue 注入运行时能力（一次，进程级）
aic8800::set_runtime(MY_RUNTIME);

// 2. 用已枚举好的 SDIO host 探测芯片，得到一次性设备
let wifi = aic8800::probe(sdio)?; // -> AicWifiNetDev

// 3. 选择启动事务；实际 SDIO 控制在 fixed-CPU queue owner 上执行
let wifi = wifi.with_startup_transaction(
    rd_net::WifiTransaction::open_access_point(b"MyAP".to_vec(), 6),
);

// 4. OS glue 连同物理 IRQ binding 交给 NetworkRuntimeBuilder 原子发布
```

运行时能力通过 trait 注入，不直接依赖 OS crate：

- `aic8800::WifiRuntime` — `now_nanos` / `sleep_ms` / `yield_now`，由 OS glue
  实现并经 `set_runtime` 安装；它不提供后台 RX/TX task 或周期 timer。
- SDHCI CARD_INT source move 到 `NetHardIrqEndpoint`。hard IRQ 只 mask/status
  snapshot；同 CPU queue owner drain FIFO、推进命令/RX/TX/AP 并执行原子 rearm。
- STA/AP 重配置使用 `WifiTransaction` 进入有界 owner queue，调用者不能直接访问
  SDIO/MMIO。

## 模块

```
src/
├── lib.rs              # crate 入口，re-export（probe / WifiRuntime / set_runtime）
├── common/             # 芯片型号、SDIO 寄存器地址、CRC 等常量
├── runtime.rs          # WifiRuntime 注入点（全局 set-once）
├── wireless/           # probe() 入口
├── fw/                 # 固件加载
│   ├── chip/           #   芯片版本检测与验证
│   ├── config.rs       #   BSP 系统配置常量
│   ├── firmware/       #   固件二进制选择与上传
│   └── protocol/       #   IPC 传输层 (SDIO CMD53 内存读写)
└── fdrv/               # WiFi 驱动核心
    ├── consts.rs       #   协议常量
    ├── core/           #   总线管理、SDIO 传输、初始化
    ├── crypto/         #   WPA2-PSK 四次握手 (PRF、AES-CCM、MIC)
    ├── net/            #   网络设备适配 (rd-net / rdif-eth)
    ├── protocol/       #   LMAC 命令/响应、扫描、连接、密钥安装
    ├── thread/         #   owner executor 调用的有界 RX/TX/AP 推进函数
    └── wifi/           #   高级 API (WifiClient) 和连接管理
```

## 支持的安全模式

- Open (无加密)
- WPA2-PSK / CCMP

## 固件

固件二进制（AICSemi 厂商 blob）**不随 crate 分发**，也不提交到仓库、不进发布
tarball。`build.rs` 在编译时把它们准备到 `OUT_DIR/firmware/`，`src/fw/firmware/data.rs`
再从那里 `include_bytes!` 嵌入；每个文件都按 SHA-256 逐字节校验。

`build.rs` 的固件来源优先级（命中即止）：

1. `$AIC8800_FIRMWARE_DIR/<name>` — 显式本地缓存 / 离线镜像目录。
2. 仓库内 `drivers/net/aic8800/firmware/<name>` — 可选的本地缓存；手动放入并通过
   SHA-256 校验后，可在离线构建时使用。
3. 从上游 pin 的 commit 下载 — 任一构建在前两项均不可用时使用。

清单、摘要与上游 pin 见 [`build.rs`](build.rs)，来源与文件列表见
[`firmware/README.md`](firmware/README.md)。

> 因此发布包可独立构建：`cargo publish` 校验 tarball 时会执行本 crate 的
> `build.rs` 自行准备固件，不依赖仓库根目录的全局预下载副作用。

## 依赖

- `sdio-host-cv1800` — SDIO 总线、move-only IRQ source 与原子 rearm 抽象
- `rd-net` / `rdif-eth` / `dma-api` — 网络设备能力与 `WifiControl` 控制面 trait
- `aes`, `hmac`, `sha1`, `pbkdf2` — WPA2 密钥派生
