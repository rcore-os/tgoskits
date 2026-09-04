# Orange Pi 5 Plus 实板故障排查与恢复指南

> 适用：Orange Pi 5 Plus **eMMC 版**（RK3588）+ 本仓库 StarryOS 本地 U-Boot 部署
> 最后更新：2026-08-04
> 相关文档：[实板刷写总览](board-flash-rk3588-visionfive2.md)、[board-linux-starry-debug](../../../.claude/skills/board-linux-starry-debug/SKILL.md)

本文档整理手边单板调试中常见问题：**`starry uboot` 卡住**、**串口乱码/无设备**、**SPL 启动失败**、**eMMC 与 SPI 混淆**、**无读卡器/无 Windows 时的 Linux 恢复**，以及 **U-Boot 已出现后** 的 **TFTP/loady 兼容**、**旧版 U-Boot 环境变量缺失**、**1500000 串口双向不稳定**。

---

## 1. 先分清两件事

| 目标 | 本仓库产物 | 正确方式 | 错误方式 |
|---|---|---|---|
| 把 **StarryOS 内核** 跑到板子上 | `target/.../starryos.bin` | U-Boot 串口加载（`starry uboot`） | `rkdeveloptool wl 0 starryos.bin` |
| **恢复整盘 Linux 系统**（eMMC/SD） | 项目内 **没有** `orangepi.img` | 官方几 GB 镜像 + MaskROM 整盘写 | 把 14MB 的 `starryos.bin` 当整盘镜像 |

StarryOS 设计是：**只替换内核到 RAM**，rootfs 继续用 eMMC 上已有的 Orange Pi Linux ext4（`mmcblk0p2`）。
因此必须先让板子能进 **U-Boot 或 Linux**，再跑 `cargo xtask starry uboot`。

---

## 2. 三根线 / 两个 Type-C 口

Orange Pi 5 Plus 上电调试常需 **三条独立连接**：

| 连接 | 接法 | 用途 |
|---|---|---|
| **3 pin TTL** | GND、RX←板 TXD、TX→板 RXD | 调试串口 `/dev/ttyUSB0`，**1500000** |
| **Type-C 电源** | **靠近 RJ45 网口** 的 Type-C → 5V/4A | 日常上电 |
| **Type-C 数据线** | 另一 Type-C（USB Device）→ PC | **仅 MaskROM 烧录**；`lsusb` 见 `2207:350b` |

常见误区：

- **只有 Type-C 数据线、没有 TTL**：PC 能进 MaskROM，但 **`starry uboot` 无法工作**（没有调试串口）。
- **烧录完成后仍插着 PC 数据线**：板子可能停在 MaskROM/异常状态，无法正常启动。
- **把电源口当烧录口**：PC 认不到 Rockchip 设备。

`board connect` / ostool-server 是 **实验室远程板卡池** 流程；手边单板用 **`starry uboot`**，不需要 `localhost:2999`。

---

## 3. StarryOS 手边部署标准流程

**前提**：eMMC（或 SD）上已有可启动的 Orange Pi Linux，且能进 U-Boot；**板子与 PC 以太网互通**（见 `orangepi-5-plus-uboot.toml` 的 `[net]`）。

```bash
cd ~/codes/os/tgoskits

# 首次选板卡配置
cargo xtask starry defconfig orangepi-5-plus

# 构建 + 经 U-Boot TFTP 传 FIT 并启动（Orange Pi 官方 U-Boot 无 loady）
cargo xtask starry uboot \
  --uboot-config os/StarryOS/configs/board/orangepi-5-plus-uboot.toml
```

**上电时序（重要）**：

1. 板子 **先断电**；**不要** 开 picocom / 其它占用 `/dev/ttyUSB0` 的程序。
2. 运行命令，等到：`Waiting for board on power or reset...`
3. **再插 Type-C 电源** 冷启动。
4. 成功标志：串口出现 `STARRY_ORANGEPI_BOOT_OK`，随后 `root@starry:/root #`。

配置文件：`os/StarryOS/configs/board/orangepi-5-plus-uboot.toml`。部署前用 `ip -br link` 确认 `[net].interface` 与 PC 接板子的网卡一致。

---

## 3.1 `Unknown command 'loady'` / `No TFTP config, using loady`

Orange Pi 官方 U-Boot **通常不编译 `loady`**。未配置 `[net]` 时 ostool 会 fallback 到串口 Ymodem，因而失败。

**标准做法**：在 `orangepi-5-plus-uboot.toml` 启用 `[net]`，由 U-Boot 通过 **TFTP** 拉取 `image.fit`。该板载 U-Boot 没有 `net list`，因此本地配置使用静态 `board_ip`，让 ostool 直接执行 `tftp`，不因探测失败而回退到 `loady`。

**若已配 `[net]` 仍见 `No TFTP config`**：ostool 0.24 会把 TOML 的 `[net]` 解析到顶层，而本地 runner 只读 `local.net`。本仓库 `axbuild` 已在读取/运行前归一化（`scripts/axbuild/src/context/uboot.rs`）。

**若 FIT 已 staged 但仍见 `No network boot request available`**：说明 `net list` 不受支持且未配置静态 `board_ip`。使用仓库当前模板即可。模板故意不设置 `tftp_dir`，让 ostool 自动安装、配置并验证 `tftpd-hpa`，再把 FIT 放进实际服务目录（通常 `/srv/tftp`）。

---

## 3.2 旧版 U-Boot 与 ostool 0.24 的兼容问题

SPI/eMMC 恢复后能进 U-Boot，不代表 `starry uboot` 能一次跑通。Orange Pi 官方 U-Boot 与 ostool 0.24 之间还有几层不兼容，按日志顺序排查：

| 阶段 | 日志 | 原因 | 处理 |
|---|---|---|---|
| 1 | `No TFTP config, using loady` | `[net]` 未被 `local.net` 读到 | 本仓库 `axbuild` 已归一化；重新编译 xtask |
| 2 | `Unknown command 'loady'` | 官方 U-Boot 无 Ymodem | 必须走 TFTP，不能无 `[net]` |
| 3 | `Unknown command 'net'` / `net list` 失败 | 旧 U-Boot 无 `net list` | 配静态 `board_ip`，勿依赖 dhcp 探测 |
| 4 | `No network boot request available` | ostool 因 `net list` 失败认为网络不可用 | 同上 + `uboot_cmd` 设 `serverip` |
| 5 | `Cannot determine kernel entry address` | 无 `kernel_addr_r` / `loadaddr` | 在 toml 里写死加载地址 |
| 6 | `setenv ... failed: Timeout` | 1500000 下串口 **TX 不稳定** | 见 §3.3 |

当前推荐配置见 `orangepi-5-plus-uboot.toml`（按 PC 实际网卡改 `interface` / IP）：

```toml
kernel_load_addr = "0x400000"
fit_load_addr = "0x5480000"
bootm_addr = "0x5480000"
uboot_cmd = ["setenv serverip 192.168.6.192"]

[net]
interface = "enp3s0"          # ip -br link 确认
board_ip = "192.168.6.100"
netmask = "255.255.255.0"
# 不设 tftp_dir → ostool 自动装/验 tftpd-hpa，FIT 进 /srv/tftp
```

**PC 侧网络示例**（同一子网）：

| 角色 | 地址 |
|---|---|
| PC（`enp3s0`） | `192.168.6.192/24` |
| 板子 U-Boot | `192.168.6.100` |
| TFTP 服务 | PC 上 UDP 69（`tftpd-hpa`） |

首次运行若提示安装 `tftpd-hpa`，输入 `y`。验证：

```bash
systemctl is-active tftpd-hpa    # active
ss -lun | rg ':69\b'             # 0.0.0.0:69
ip -4 -br addr show dev enp3s0  # 192.168.6.192/24
```

**成功标志**（日志）：出现 `tftp ... && bootm`，**不再**出现 `loady` 或 `No network boot request`。

---

## 3.3 串口能打断 U-Boot，但 `setenv` 超时

若已能 `Waiting...` 后进入 U-Boot（有时能跑完 `setenv autoload`），却在后续 `setenv ipaddr` / `setenv netmask` / 任意带 `cmd-ok` 的长命令处 **连续 Timeout**：

```text
cmd: setenv autoload yes
cmd `setenv autoload yes` failed: Timeout, retrying...
Error: command `setenv autoload yes` failed after retries
```

这说明 **串口读大致可用，写/回显不可靠**。常见表现：

- 日志里偶发 `�et`、`�oady` 等乱码前缀
- **Ctrl+C 单字节** 能打断并出现 `<INTERRUPT>`
- **完整命令 + 回显** 经常收不全

| 检查项 | 说明 |
|---|---|
| TTL 芯片 | 你的是 **CH340**（`1a86:7523`），Orange Pi 文档推荐 CH340 @ 1500000，但线材/接线差时高波特率更易出问题 |
| 接线 | GND 共地；**RX←板 TXD、TX→板 RXD**；尽量 **不要** 从 TTL 给板子供电，只用板子 5V 电源 |
| 线长 | 杜邦线越短越好，避免面包板/飞线过长 |
| 独占串口 | 跑 `starry uboot` 前退出 picocom |
| 冷启动时序 | 先 `Waiting...`，再插电；不要在 U-Boot 倒计时/autoboot 刷屏最猛时发命令 |

**手动验证**（比 ostool 更直观）：

```bash
picocom -b 1500000 --flow n /dev/ttyUSB0
```

冷启动到 `=>` 后逐条输入：

```text
echo ok
setenv ipaddr 192.168.6.100
setenv serverip 192.168.6.192
setenv netmask 255.255.255.0
```

- 若 **手动也乱码/丢字** → 先修硬件（换线、缩短线、换 USB 口/另一块 TTL）
- 若 **手动稳定、ostool 超时** → 属 ostool 与该 U-Boot 的交互节奏问题，可用手动 TFTP 启动（下节）

### 3.4 手动 TFTP 启动 StarryOS（绕过 ostool 串口命令）

当 TFTP 服务与网络已 OK，但 ostool 串口命令不可靠时，可手动完成最后一步：

```bash
# PC：准备 FIT（ostool 构建产物）
sudo cp target/aarch64-unknown-linux-musl/release/image.fit /srv/tftp/image.fit
sudo chmod 644 /srv/tftp/image.fit
```

picocom 进 U-Boot `=>`：

```text
setenv ipaddr 192.168.6.100
setenv serverip 192.168.6.192
setenv netmask 255.255.255.0
tftp 0x5480000 image.fit
bootm 0x5480000
```

若 `tftp` 也报 `Unknown command`，说明该 U-Boot **未编网络命令**，需换带 TFTP 的 U-Boot 或走 SD/eMMC 启动链上的其它加载方式。

---

## 4. `Waiting for board on power or reset...` 是什么意思

这不是编译失败。ostool 在此之后：

1. 每 20ms 向串口发 **Ctrl+C**
2. **无限等待** U-Boot 返回以 `<INTERRUPT>` 结尾的一行
3. 收到 U-Boot 响应后，经 **TFTP 或 loady** 传 FIT 并 `bootm`（Orange Pi 默认走 TFTP，见 §3.1）

因此 **卡住 = 板子从未进入 U-Boot 命令行**，与 `starryos.bin` 是否编译成功无关。

常见原因（按优先级）：

| 原因 | 说明 |
|---|---|
| SPL 启动链损坏 | 串口有 DDR 日志但 `SPL: failed to boot from all boot devices` |
| 时序错误 | 板子已上电跑完启动流程，才运行 `starry uboot` |
| 仍在 MaskROM | PC 数据线接着，或刚烧录未拔线 |
| 已在 Linux 里 | Ctrl+C 不会触发 U-Boot 的 `<INTERRUPT>` |
| 串口被占用 | picocom 未退出 → `Unable to acquire exclusive lock` |
| `/dev/ttyUSB0` 不存在 | TTL 未接或接错口 |

---

## 5. 启动链：BootROM → SPI → eMMC

```text
上电 → BootROM → SPI Flash 里的 SPL → 从 eMMC/SD 加载 U-Boot → Linux
                      ↑                        ↑
                 常在此处损坏              StarryOS 需要到这里
```

| 存储 | MaskROM 下 `rkdeveloptool` | 正常上电 SPL |
|---|---|---|
| **eMMC** | `list-partitions` 可见 GPT、bootfs/rootfs | 需 SPL 能 `mmc_init` 成功 |
| **SPI Flash** | 需 `cs 9` 切换后才操作 | SPL 从这里启动；损坏则全盘失败 |
| **microSD** | 插板子 TF 槽 + `cs 2` 可写 | SPL 可 fallback 到 MMC1 |

**关键区分**：MaskROM 能读写 eMMC **不等于** SPL 能从 eMMC 启动。
eMMC 内容正确时，仍可能因 **SPI 里坏的 SPL/分区表** 导致 `mmc_init: -123` 与 `MTD2 Invalid GPT`。

---

## 6. 典型串口日志解读

### 6.1 正常（串口与波特率 OK）

```text
PDDR4X, 2112MHz
channel[0] ... Size=4096MB
...
U-Boot ...
Hit any key to stop autoboot
```

或进入 `orangepi login:`。

早期少量乱码可忽略（Rockchip ROM 二进制混合输出）。

### 6.2 SPL 全线失败（当前最常见）

```text
Trying to boot from MMC2
mmc_init: -123
Trying to boot from MMC1
mmc_init: -95
Trying to boot from MTD2
part_get_info_efi: *** ERROR: Invalid GPT ***
Not fit magic
SPL: failed to boot from all boot devices
### ERROR ### Please RESET the board ###
```

| 日志 | 含义 |
|---|---|
| `mmc_init: -123` | 运行时 **eMMC 初始化失败** |
| `mmc_init: -95` | SD 槽无卡或不可用 |
| `MTD2 Invalid GPT` | **SPI Flash 启动链损坏** |
| `SPL: failed` | **进不了 U-Boot** → `starry uboot` 必卡 Waiting |

### 6.3 串口工具

```bash
picocom -b 1500000 --flow n /dev/ttyUSB0
```

- 波特率 **1500000**（不是 115200）
- 流控用 **`--flow n`**（无流控 8N1）
- 先看日志：**先开 picocom，再冷上电**

---

## 7. eMMC 整盘恢复（MaskROM）

当 eMMC 分区损坏或需重装 Orange Pi Linux 时：

**准备**：官方 `MiniLoaderAll.bin` + 几 GB 的 `Orangepi5plus_xxx.img`（**不是** `rkspi_loader.img`）。

```bash
# MaskROM：按住 MaskROM → 插电源 → 松开
lsusb | grep 2207
sudo rkdeveloptool ld    # 或完整版：~/rkdeveloptool-src/rkdeveloptool ld

LOADER=./MiniLoaderAll.bin
IMAGE=/path/to/Orangepi5plus_1.2.0_ubuntu_....img

sudo rkdeveloptool db "$LOADER"
sudo rkdeveloptool wl 0 "$IMAGE"    # 写 eMMC 整盘，耗时较长
sudo rkdeveloptool rd
```

**验证**（仍在 MaskROM）：

```bash
sudo rkdeveloptool list-partitions   # 应见 bootfs + rootfs
sudo rkdeveloptool read-flash-info   # eMMC 容量（如 121GB Samsung）
```

**常见误操作**：`wl 0 rkspi_loader.img` 会把 **SPI loader 写到 eMMC LBA0**，破坏 GPT；该文件应写 **SPI Flash**，不是 eMMC 整盘替代品。

---

## 8. eMMC 已写好仍起不来：修 SPI Flash

官方手册（§2.16）：**eMMC 烧录成功但无法启动 → 清空 SPI Flash 再试**。

原因：SPI 中 SPL/U-Boot 链损坏时，SPL 无法正确加载 eMMC 上的 U-Boot，即使 eMMC GPT 在 MaskROM 下完全正常。

---

## 9. 纯 Linux 修复 SPI（无 Windows / 无 Wine）

Ubuntu apt 自带的 **`rkdeveloptool 1.0.0` 没有 `cs` 命令**，无法切换 SPI，会报：

```text
sudo rkdeveloptool cs 1
command is invalid!
```

需从 Rockchip 源码编译 **完整版**：

```bash
sudo apt-get install -y git build-essential libusb-1.0-0-dev libudev-dev \
  pkg-config dh-autoreconf autoconf automake libtool

git clone https://github.com/rockchip-linux/rkdeveloptool.git ~/rkdeveloptool-src
cd ~/rkdeveloptool-src
./autogen.sh && ./configure && make -j$(nproc)

~/rkdeveloptool-src/rkdeveloptool -h | grep cs
# ChangeStorage: cs [storage: 1=EMMC, 2=SD, 9=SPINOR]
```

后续统一用 `RK=~/rkdeveloptool-src/rkdeveloptool`，避免调用 apt 旧版。

### 9.1 清空 SPI（eMMC 已恢复时优先）

```bash
RK=~/rkdeveloptool-src/rkdeveloptool
LOADER=/path/to/MiniLoaderAll.bin

# MaskROM + Type-C 数据线接 PC
lsusb | grep 2207
sudo $RK ld

sudo $RK db "$LOADER"
sudo $RK cs 9              # 切换到 SPI NOR
sudo $RK rfi               # 确认容量约 16MB/32MB，不是 121GB eMMC
sudo $RK ef                # 擦除 SPI
sudo $RK rd
```

拔掉 PC 数据线，TTL + 电源冷启动，应能进 U-Boot 或 Linux。

### 9.2 写入 SPI loader（擦除后仍异常时）

```bash
SPIIMG=/path/to/rkspi_loader.img   # MiniLoader 包内，约 4MB

sudo $RK db "$LOADER"
sudo $RK cs 9
sudo $RK rfi
sudo $RK wl 0 "$SPIIMG"
sudo $RK rd
```

**禁止**对 eMMC 执行 `wl 0 rkspi_loader.img`。

---

## 10. 无读卡器：用板子 TF 槽烧 microSD（可选）

若必须从 SD 启动进 Linux 再 `dd` 修 SPI，**不需要 USB 读卡器**：官方 §2.3.2 要求把 **microSD 插入板子 TF 槽**，MaskROM + **`rk3588_linux_tfcard.cfg`**（RKDevTool）烧录。

纯 Linux 等价操作（**必须用完整版 rkdeveloptool**）：

```bash
RK=~/rkdeveloptool-src/rkdeveloptool
LOADER=/path/to/MiniLoaderAll.bin
IMAGE=/path/to/Orangepi5plus_xxx.img

# microSD 插入板子 TF 槽，MaskROM
sudo $RK db "$LOADER"
sudo $RK cs 2              # SD 卡
sudo $RK rfi               # 必须是 SD 容量；若仍显示 121GB → 停，勿 wl
sudo $RK wl 0 "$IMAGE"
sudo $RK rd
```

进 Linux 后修复 SPI：

```bash
sudo dd if=/boot/rkspi_loader.img of=/dev/mtdblock0 conv=notrunc
# 或：sudo nand-sata-install → 选 7 Install/Update bootloader on SPI Flash
sync && sudo reboot
```

---

## 11. 问题速查表

| 现象 | 原因 | 处理 |
|---|---|---|
| `failed to open /dev/ttyUSB0` | TTL 未接 / 设备节点不存在 | 接 3 pin TTL；Type-C 数据线不是串口 |
| `Unable to acquire exclusive lock` | picocom 占用串口 | 退出 picocom 再跑 `starry uboot` |
| `Waiting...` 一直不动 | 无 U-Boot | 修启动链；见 §4、§8 |
| `No devices in rockusb mode` | 未进 MaskROM / 缺烧录 Type-C 线 | 按住 MaskROM 上电；数据线接 USB Device 口 |
| `Opening loader failed` | `MiniLoaderAll.bin` 路径或版本错误 | 用 Orange Pi 官方 RK3588 MiniLoader |
| `wl 0 rkspi_loader.img` 后更乱 | 误写 eMMC LBA0 | 整盘重写官方 eMMC 镜像；再清 SPI |
| eMMC `list-partitions` 正常但 SPL failed | SPI 启动链坏 | §9 清 SPI 或写 `rkspi_loader.img` |
| `rkdeveloptool cs` invalid | apt 版工具过旧 | §9 编译 rockchip-linux/rkdeveloptool |
| `Unknown command 'loady'` | 未配 `[net]` 或回退到 loady | 启用 TFTP；见 §3.1 |
| `No network boot request available` | 无 `net list` + 未配静态 IP | §3.2：`board_ip` + `serverip` |
| `Cannot determine kernel entry address` | 无 `kernel_addr_r` | §3.2：写死 `kernel_load_addr` 等 |
| `setenv ... Timeout` 三次后失败 | 1500000 串口 TX 不稳定 | §3.3；或 §3.4 手动 TFTP |
| `Unknown command 'net'` | 旧 U-Boot 无 net 子命令 | 正常；用静态 IP + `tftp` |
| 想生成 StarryOS 版 eMMC.img | 项目不支持 | 用 `starry uboot` 部署内核 |

---

## 12. 恢复完成后的 StarryOS 部署

1. picocom 冷启动确认 **U-Boot 或 Linux** 正常；板子网线接 PC 或同一局域网。
2. 确认 PC 网卡与 `orangepi-5-plus-uboot.toml` 中 `[net].interface`、`board_ip`、`uboot_cmd` 的 `serverip` 同一子网。
3. 确认 `tftpd-hpa` 在跑（§3.2）。
4. 退出 picocom；板子断电，执行：

```bash
cargo xtask starry uboot \
  --uboot-config os/StarryOS/configs/board/orangepi-5-plus-uboot.toml
```

5. 见 `Waiting...` 后 **再上电**。
6. 成功：`STARRY_ORANGEPI_BOOT_OK` → `root@starry:/root #`。
7. 若卡在 `setenv ... Timeout`：按 §3.3 查串口硬件，或 §3.4 手动 TFTP。

---

## 13. 相关路径

| 文件 | 说明 |
|---|---|
| `os/StarryOS/configs/board/orangepi-5-plus-uboot.toml` | 本地 U-Boot 串口配置 |
| `os/StarryOS/configs/board/orangepi-5-plus.dtb` | StarryOS 设备树 |
| `scripts/axbuild/src/context/uboot.rs` | ostool `[net]` → `local.net` 归一化 |
| `target/aarch64-unknown-linux-musl/release/starryos.bin` | StarryOS 内核二进制 |
| `target/aarch64-unknown-linux-musl/release/image.fit` | TFTP 加载的 FIT 镜像 |
| `MiniLoader/MiniLoaderAll.bin` | MaskROM loader（需自行下载） |
| `MiniLoader/rkspi_loader.img` | SPI Flash loader |
| `MiniLoader/rk3588_linux_emmc.cfg` | RKDevTool eMMC 配置 |
| `MiniLoader/rk3588_linux_tfcard.cfg` | RKDevTool TF 卡配置 |
| `MiniLoader/rk3588_linux_spiflash.cfg` | RKDevTool SPI 配置 |

官方资料：[Orange Pi 5 Plus Wiki](http://www.orangepi.org/orangepiwiki/index.php/Orange_Pi_5_Plus)、用户手册 §2.3.2（TF 卡）、§2.5（eMMC）、§2.16（清 SPI）。

---

## 14. 决策流程（简图）

```text
目标：跑 StarryOS
    │
    ├─ 板子能进 U-Boot/Linux？
    │     ├─ 是 → 配 [net] + 固定加载地址（§3.2）→ starry uboot（Waiting 后冷上电）
    │     │         ├─ loady / No TFTP → §3.1、§3.2
    │     │         ├─ setenv Timeout → §3.3 查串口；或 §3.4 手动 TFTP
    │     │         └─ 成功 → STARRY_ORANGEPI_BOOT_OK
    │     └─ 否 → picocom 看 SPL 日志
    │               ├─ SPL failed + Invalid GPT(MTD2)
    │               │     → eMMC 已在 MaskROM 验证 OK？
    │               │           ├─ 是 → 清 SPI（§9，完整 rkdeveloptool cs 9 ef）
    │               │           └─ 否 → 整盘写 eMMC 镜像（§7）→ 再清 SPI
    │               ├─ 无串口 → 查 TTL / 1500000 / TX-RX
    │               └─ 仅 MaskROM → 查烧录 Type-C 线
    │
    └─ 不要用 wl 0 写 starryos.bin；不要用 rkspi_loader.img 代替 eMMC 整盘镜像
```
