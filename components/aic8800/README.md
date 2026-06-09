# aic8800

AIC8800 系列 WiFi 芯片驱动，运行在 ArceOS 上，通过 SDIO 总线通信。

支持芯片：AIC8801、AIC8800DC、AIC8800D80、AIC8800D80X2。

## 用法

```rust
let bus = aic8800::connect("SSID", "password", "10.0.0.2", 24, "10.0.0.1")?;
// 网络设备已自动注册到 ax_net，可直接使用 TCP/UDP
```

`connect` 完成全部初始化：SoC 硬件初始化 → SDIO 枚举 → 芯片检测 → 固件加载 →
驱动初始化 (RX/TX 线程) → LMAC 配置 → 扫描 → 连接 → WPA2 握手 → 注册网络设备。

## 模块

```
src/
├── lib.rs              # crate 入口，re-export
├── common/             # 芯片型号、SDIO 寄存器地址、CRC 等常量
├── fw/                 # 固件加载
│   ├── chip/           #   芯片版本检测与验证
│   ├── config.rs       #   BSP 系统配置常量
│   ├── firmware/       #   固件二进制选择与上传
│   └── protocol/       #   IPC 传输层 (SDIO CMD53 内存读写)
├── fdrv/               # WiFi 驱动核心
│   ├── consts.rs       #   协议常量
│   ├── core/           #   总线管理、SDIO 传输、初始化
│   ├── crypto/         #   WPA2-PSK 四次握手 (PRF、AES-CCM、MIC)
│   ├── net/            #   ax_net 网络设备适配
│   ├── protocol/       #   LMAC 命令/响应、扫描、连接、密钥安装
│   ├── thread/         #   RX/TX 线程
│   └── wifi/           #   高级 API (WifiClient) 和连接管理
└── wireless/           # 顶层入口 (connect/shutdown)
```

## 支持的安全模式

- Open (无加密)
- WPA2-PSK / CCMP

## 依赖

- `sdio-host` — SDIO 总线抽象 trait
- `sdhci-cv1800` — SG2002 SDHCI 控制器实现
- `ax-plat-riscv64-sg2002` — 平台中断注册
- `aes`, `hmac`, `sha1`, `pbkdf2` — WPA2 密钥派生
