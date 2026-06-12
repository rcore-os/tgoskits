# aic8800

AIC8800 系列 WiFi 芯片驱动核心，通过 SDIO 总线通信。**OS 无关**：核心代码不直接
依赖任何操作系统运行时；定时、休眠、让步、任务派生等能力通过 `wifi-host::WifiRuntime`
trait 注入。ArceOS 适配实现位于 `glue_arceos`，由 `arceos` feature 门控。

支持芯片：AIC8801、AIC8800DC、AIC8800D80、AIC8800D80X2。

## 用法

平台相关的资源（MMIO 映射、SDHCI 枚举、IRQ 注册）由上层 OS glue 负责；本 crate 从
一个已就绪的 SDIO host 开始完成芯片侧 bring-up，并返回一个实现了
`wifi_host::WifiDriver` 的对象。

```rust
// 1. OS glue 注入运行时能力（仅 ArceOS 集成时）
#[cfg(feature = "arceos")]
aic8800::glue_arceos::install_runtime();

// 2. 用已枚举好的 SDIO host 探测芯片，得到与具体芯片无关的驱动句柄
let mut wifi: Box<dyn wifi_host::WifiDriver> = aic8800::probe(sdio)?;

// 3. SoftAP 或 STA
wifi.start_ap_open(b"MyAP", 6)?;          // 开放 SoftAP
// wifi.connect("SSID", "password")?;     // 或连接 STA

// 4. 取出网络设备交给上层协议栈注册（DMA 能力由上层注入）
let net = wifi.take_net(dma_op).expect("net taken once");
```

运行时能力通过 trait 注入，不直接依赖 OS crate：

- `wifi_host::WifiRuntime` — `now_nanos` / `sleep_ms` / `yield_now` /
  `spawn_poll_task` / `block_until`，由 OS glue 实现。
- 接收数据帧的唤醒回调通过 `set_rx_data_callback` 注册（SDIO Wi-Fi 走带外 RX，
  不经以太网 IRQ 框架）。

## Features

- `default` — 仅驱动核心，OS 无关，不引入任何 ArceOS runtime crate。
- `arceos` — 启用 `glue_arceos`，用 `ax-task` / `ax-hal` 实现 `WifiRuntime`，
  供 ArceOS / Starry 集成使用。

## 模块

```
src/
├── lib.rs              # crate 入口，re-export（probe / Aic8800Wifi / set_runtime）
├── common/             # 芯片型号、SDIO 寄存器地址、CRC 等常量
├── runtime.rs          # WifiRuntime 注入点（全局 set-once）
├── glue_arceos.rs      # ArceOS 运行时适配（feature = "arceos"）
├── wireless/           # WifiDriver 实现 + probe() 入口
├── fw/                 # 固件加载
│   ├── chip/           #   芯片版本检测与验证
│   ├── config.rs       #   BSP 系统配置常量
│   ├── firmware/       #   固件二进制选择与上传
│   └── protocol/       #   IPC 传输层 (SDIO CMD53 内存读写)
└── fdrv/               # WiFi 驱动核心
    ├── consts.rs       #   协议常量
    ├── core/           #   总线管理、SDIO 传输、初始化、PollSet
    ├── crypto/         #   WPA2-PSK 四次握手 (PRF、AES-CCM、MIC)
    ├── net/            #   网络设备适配 (rd-net / rdif-eth)
    ├── protocol/       #   LMAC 命令/响应、扫描、连接、密钥安装
    ├── thread/         #   RX/TX/AP 轮询任务
    └── wifi/           #   高级 API (WifiClient) 和连接管理
```

## 支持的安全模式

- Open (无加密)
- WPA2-PSK / CCMP

## 固件

固件二进制（AICSemi 厂商 blob）**不随 crate 分发**，也不提交到仓库。编译本 crate
时需要它们位于 `components/aic8800/firmware/`，按 SHA-256 摘要逐字节校验后经
`include_bytes!` 嵌入。

在本仓库内，任何编译 `aic8800` 的 `cargo xtask starry ...` / `cargo xtask clippy ...`
会自动按需下载这些 blob（清单、摘要与上游 pin 见
[`scripts/axbuild/src/firmware.rs`](../../scripts/axbuild/src/firmware.rs)，
来源与文件列表见 [`firmware/README.md`](firmware/README.md)）。

> 从 crates.io 独立使用本 crate 时，需自行将这些固件 blob 放入
> `firmware/` 目录后再编译，否则 `include_bytes!` 会因找不到文件而构建失败。

## 依赖

- `sdio-host-cv1800` — SDIO 总线抽象 trait
- `sdhci-cv1800` — SG2002 SDHCI 控制器实现
- `wifi-host` — `WifiDriver` / `WifiRuntime` trait
- `rd-net` / `rdif-eth` / `dma-api` — 网络设备能力
- `aes`, `hmac`, `sha1`, `pbkdf2` — WPA2 密钥派生
