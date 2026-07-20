# aic8800

AIC8800 系列 WiFi 芯片驱动核心，通过 SDIO 总线通信。**OS 无关**：核心代码不直接
依赖任何操作系统运行时；定时、休眠、让步、任务派生等能力通过 `aic8800::WifiRuntime`
trait 注入，由上层 OS glue 在初始化时调用 `aic8800::set_runtime` 安装。

支持芯片：AIC8801、AIC8800DC、AIC8800D80、AIC8800D80X2。

## 用法

平台相关的资源（MMIO 映射、SDHCI 枚举、IRQ 注册）由上层 OS glue 负责；本 crate 从
一个已就绪的 SDIO host 开始完成芯片侧 bring-up，并返回一个 `AicWifiNetDev`——它
同时是数据面（`rd_net::Net` 设备）和控制面（实现 `rd_net::WifiControl`）。

```rust
// 1. OS glue 注入运行时能力（一次，进程级）
aic8800::set_runtime(MY_RUNTIME);

// 2. 用已枚举好的 SDIO host 探测芯片，得到设备句柄
let mut wifi = aic8800::probe(sdio)?;   // -> AicWifiNetDev

// 3. SoftAP 或 STA（rd_net::WifiControl）
wifi.start_ap_open(b"MyAP", 6)?;          // 开放 SoftAP
// wifi.connect("SSID", "password")?;     // 或连接 STA

// 4. 把设备交给 ax-driver 注册进 rd-net / ax-net 设备模型
```

运行时能力通过 trait 注入，不直接依赖 OS crate：

- `aic8800::WifiRuntime` — `now_nanos` / `sleep_ms` / `yield_now` /
  轮询任务派生等，由 OS glue 实现并经 `set_runtime` 安装。
- 接收数据帧的唤醒走 `rd_net::WifiControl::set_rx_wake` 注册的回调（SDIO Wi-Fi
  走带外 RX，不经以太网 IRQ 框架）。

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

固件二进制（AICSemi 厂商 blob）**不随 crate 分发**，也不提交到仓库、不进发布
tarball。`build.rs` 在编译时把它们准备到 `OUT_DIR/firmware/`，`src/fw/firmware/data.rs`
再从那里 `include_bytes!` 嵌入；每个文件都按 SHA-256 逐字节校验。

`build.rs` 的固件来源优先级（命中即止）：

1. `$AIC8800_FIRMWARE_DIR/<name>` — 显式本地缓存 / 离线镜像目录。
2. 仓库内 `components/aic8800/firmware/<name>` — 可选的本地缓存；手动放入并通过
   SHA-256 校验后，可在离线构建时使用。
3. 从上游 pin 的 commit 下载 — 任一构建在前两项均不可用时使用。

清单、摘要与上游 pin 见 [`build.rs`](build.rs)，来源与文件列表见
[`firmware/README.md`](firmware/README.md)。

> 因此发布包可独立构建：`cargo publish` 校验 tarball 时会执行本 crate 的
> `build.rs` 自行准备固件，不依赖仓库根目录的全局预下载副作用。

## 依赖

- `sdio-host-cv1800` — SDIO 总线抽象 trait
- `sdhci-cv1800` — SG2002 SDHCI 控制器实现
- `rd-net` / `rdif-eth` / `dma-api` — 网络设备能力与 `WifiControl` 控制面 trait
- `aes`, `hmac`, `sha1`, `pbkdf2` — WPA2 密钥派生
