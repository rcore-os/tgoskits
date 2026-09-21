# Orange Pi 5 Plus Linux 与 Zephyr 双客户机场景

该场景在 SD 版 Orange Pi 5 Plus 上通过 Axvisor 同时运行 Linux 感知客户机和 Zephyr 控制客户机。Linux 使用 USB 摄像头和 RKNPU 生成感知结果，通过 `robot-ivc` 发送给 Zephyr；Zephyr 独占 UART6，负责底盘、机械臂和输入看门狗。仓库只保留已验证的 SD 配置；eMMC 迁移要求见第6节。

## 1. 工作负载与资产

本场景按设备职责划分客户机，避免 Linux 和 Zephyr 同时操作同一外设。Axvisor 始终保留物理管理串口；启用网络 Shell 时还必须保留 RTL8125 及其宿主依赖。

### 1.1 客户机职责

两个客户机通过同名 `robot-ivc` 虚拟设备建立通道，通道身份和大小由 IVC 模型统一管理。客户机名称来自各自 TOML 的 `base.name`，也用于网页窗格标题。

| 客户机 | 主要职责 | 关键设备 |
| --- | --- | --- |
| Linux | 摄像头采集、RKNPU 推理、发布感知结果 | `/usbdrd3_1`、RKNPU、根存储、`robot-ivc` |
| Zephyr | 接收感知结果、控制底盘和机械臂 | UART6 `/serial@feb90000`、`robot-ivc` |
| Axvisor | VM 管理、物理串口、可选网页控制台 | RTL8125、宿主 PCIe 路径 |

Linux 配置保留 `clk_ignore_unused` 和 `pd_ignore_unused`，防止启动末尾的通用清理关闭其他系统仍在使用的资源。这两个参数不能阻止已绑定驱动主动复位设备或关闭时钟，因此不能代替第 2 节的设备所有权隔离。

### 1.2 客户机镜像

Axvisor 从宿主文件系统加载 Linux 内核、initramfs 和 Zephyr 镜像。板卡运行命令只上传 Axvisor FIT，不会自动安装这些客户机资产，因此启动前必须确认文件存在且非空。

```text
/guest/linux/orangepi-5-plus-6.1.99-axivc
/guest/linux/initramfs.cpio
/guest/zephyr/orangepi-robot-control-sdk
/guest/zephyr/orangepi-robot-control-sdk.dtb
```

Zephyr 控制镜像通过 `tgosimages` 的机器人应用入口构建，不从某个开发者的 AKA 工作目录
直接产出：

```bash
# 在 tgosimages 仓库根目录执行
./scripts/apps/aka-rk3588-zephyr.sh \
  --image-name orangepi-robot-control-sdk
```

对应产物为：

```text
IMAGES/orangepi/zephyr/orangepi-robot-control-sdk
IMAGES/orangepi/zephyr/orangepi-robot-control-sdk.dtb
```

Linux 根文件系统还应包含与当前内核版本匹配的 `axvisor.ko`、感知程序、模型和运行脚本。
这些内容由 Linux 客户机发布包负责安装；本场景只约定 Guest 中的运行接口，不固化某台
开发板的部署目录。

### 1.3 SD 存储约定

Linux 客户机使用 `linux-smp1-sd.toml`，其根设备是 `/dev/mmcblk1p2`。ostool FIT
启动会绕过 SD 卡上的 `boot.scr`，因此 board 配置还会显式设置 AxVisor
宿主的 SD 根设备。宿主和 Linux 客户机中的 `mmcblk` 编号不能相互推导。

## 2. 网络 Shell

当前 SD 构建配置启用板载网络 Shell，用于同时观测 AxVisor、Linux 和 Zephyr。

### 2.1 启用开关

网络 Shell 需要 PCIe、RTL8125、`browser-console` 和监听地址同时存在，当前构建
配置已按下列方式启用。只启用 `browser-console` 而没有 PCIe 与 RTL8125 驱动时，
Axvisor 没有可用的实体管理网卡；只启用网卡驱动则不会发布网页服务。

```toml
features = [
  "ax-driver/rk3588-pcie",
  "ax-driver/realtek-rtl8125",
  "ax-driver/rockchip-sdhci",
  "ax-driver/rockchip-dwmmc",
  "browser-console",
  "fs",
]

[env]
AXVM_HTTP_BIND = "0.0.0.0:8080"
```

`browser-console` 发布 Axvisor、Linux 和 Zephyr 三个独立 WebSocket 字节流。服务没有 TLS 和认证，只能监听可信管理网络；不应把端口 `8080` 暴露到公共网络。

### 2.2 宿主资源隔离

Linux 使用 `guest_type = "passthrough"` 且 `passthrough = []`，因此从整板直通基线开始。AxVM 会自动移除 PCIe Host Bridge，但 PCIe PHY、USB 控制器和 USB/DP PHY 是设备树中的独立节点，不会因为 PCIe Bridge 被移除而自动消失。Linux 与 Axvisor 各自维护时钟和复位状态，无法协调同一真实硬件的所有权。

| 排除节点 | 保留给宿主的资源 | 原因 |
| --- | --- | --- |
| `/phy@fee00000`、`/phy@fee10000`、`/phy@fee20000`、`/phy@fee80000` | RK3588 PCIe PHY 集合 | 防止 Linux 重配或复位 Axvisor RTL8125 所在的 PCIe 链路 |
| `/usbdrd3_0` | 未用于机器人摄像头的 USB0 DWC3/OTG 控制器 | 防止 Linux 在启动和 USB gadget 服务阶段切换该实体控制器状态 |
| `/phy@fed80000` | USB0 使用的 USB3/DisplayPort Combo PHY | 防止 Linux 修改其 PLL、lane mux、GRF、时钟和批量复位状态 |

`/usbdrd3_0` 与 `/phy@fed80000` 是控制器和 PHY 的完整所有权单元，应同时排除。只移除控制器仍可能让独立 PHY 节点被驱动探测；只移除 PHY 会给 DWC3 留下不完整依赖。机器人摄像头实际连接在 `/usbdrd3_1/usb@fc400000`，上述隔离不影响摄像头、RKNPU、根存储、IVC 或 UART6。

即使以后关闭网络 Shell，也应保留 Linux TOML 中的这些排除项，避免同一场景
因网页功能开关改变实体设备所有权。

### 2.3 定位证据

受控 A/B 测试把问题分成两个阶段。未隔离时，Axvisor 先取得 DHCP 地址并发布网页，随后 Linux 启动使 HTTP、WebSocket 和 ICMP 永久失联；Linux、Zephyr 和物理串口仍继续运行，因此故障不在网页行编辑器或客户机网络栈。

第一阶段保护 PCIe PHY 后，网页能够越过 Linux 早期设备初始化，但仍在 systemd 的 `Manage USB device functions` 附近失联。继续把 `/usbdrd3_0` 与 `/phy@fed80000` 作为一组排除后，eMMC 和 SD 均完成 Linux 启动、三路 WebSocket 与完整机器人验收。当前证据确认的是设备组冲突；若需要定位到单个 CRU、GRF、复位或 IRQ 位，还需增加寄存器和中断状态采样。

最终实测中，eMMC 在网页就绪后完成 300 次连续 HTTP `200` 探测，SD 完成 229 次连续 HTTP `200` 探测，均无永久失联。两块板上的 Linux 都完成 1860 次推理和 IVC 发送且丢包为零，Zephyr 完成一轮抓取、放置和看门狗停车验证。

## 3. 构建与启动流程

启动前确认机器人活动范围安全、SD CI 板卡空闲，并确认第 1.2 节列出的客户机资产已同步到宿主文件系统。

### 3.1 SD 构建

SD 使用 `linux-smp1-sd.toml`，Linux 根设备为 `/dev/mmcblk1p2`。宿主根设备由 SD 板卡
配置的 U-Boot 参数另行指定。在标准 `tgoskits` 仓库根目录执行：

```bash
cargo xtask axvisor build \
  --arch aarch64 \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/dual-linux-zephyr/build-aarch64-unknown-none-softfloat.toml
```

板卡执行环境使用本目录的 `board-orangepi-5-plus-dualguest-robot.toml`。实际上传和启动由
项目 CI 或部署环境的标准 board runner 完成；ostool 服务地址、端口和板卡租约不属于
仓库场景说明。

成功时 Linux 应从 `/dev/mmcblk1p2` 挂载根文件系统。不能根据宿主 U-Boot 使用的 `/dev/mmcblk0p2` 推断 Linux 客户机编号。

### 3.2 网页访问

网页由开发板上的 Axvisor 直接发布，不依赖运行 `cargo xtask` 的主机继续提供代理。启用网络 Shell 后，启动日志给出的 `web_console` 才是实际访问地址。

```text
Axvisor network ready:
  interface = eth0
  ipv4 = <board-ip>/24
  web_console = http://<board-ip>:8080/
```

浏览器应显示 Axvisor、Linux 和 Zephyr 三个窗格。若 Linux 启动后网页永久失联，首先核对 Linux TOML 的六个 `disabled` 节点，不要先通过绑核、增加网页线程或降低客户机日志量掩盖硬件所有权冲突。

## 4. 验收与退出

验收需要同时证明 Linux 感知、IVC 传输、Zephyr 控制和宿主网页服务；单独看到客户机登录提示符或网页首页不能证明完整场景可用。

### 4.1 机器人验收

在 Linux 中进入已安装的机器人运行包，按照第 5 节执行有限验收。
`STARRY_` 是感知程序沿用的日志前缀，不表示当前客户机是 StarryOS。

### 4.2 网络验收

启用网页时，应从同一管理网段持续访问 `/api/consoles`，并分别连接 `/ws/axvisor`、`/ws/vm-1` 和 `/ws/vm-2`。探测必须覆盖 Linux 完整启动和机器人脚本运行期，不能只在 Linux 启动前检查一次。

三路 WebSocket 应分别接受 Axvisor 命令、Linux shell 输入和 Zephyr shell 输入。HTTP 在短期高日志压力下可设置合理请求超时，但出现持续 `000`、ICMP 同时失联或所有 WebSocket 永久断开时，应按第 2.2 节检查实体设备所有权。

### 4.3 安全退出

Linux 与 Axvisor 使用同一实体根存储，结束测试前必须先刷新 Linux 文件系统，再释放板卡会话。直接断电可能造成 ext4 根文件系统损坏。

```text
Linux:   sync
Axvisor: vm stop 1
Axvisor: vm stop 2
Axvisor: exit
```

从 Linux 返回 Axvisor shell 时依次按 `Ctrl+X`、松开、再按 `h`。完成关闭后使用 `Ctrl+A`、松开、再按 `x` 退出 ostool 串口会话。

## 5. SD 双客户机 CI

这是 Linux + Zephyr 的实体板集成回归测试：检查内核或驱动更新后，是否出现
“能够启动，但摄像头/NPU 变慢、两客户机失联或执行器没完成动作”的问题。
这些真实设备与时序无法仅由主机单元测试证明。AxVisor 提供 IVC 和设备隔离，Linux
负责感知，Zephyr 独占 UART6，负责决策、底盘及机械臂。

Linux 登录 Shell 出现 `orangepi@orangepi5plus:~` 提示符后，由本目录的 board 配置运行：

```sh
sudo -n /home/orangepi/robot/aka-rk3588/run_dual_pick_ci_once.sh --min-fps 28
```

消息和失败处理按以下顺序进行；IVC 通道 key 为 `0x49564301`：

1. **配置。** Linux 读取本板标定与控制参数，发送带 session、配置 CRC 的 `BEGIN`、
   9 条 `CHUNK`、`COMMIT`。Zephyr 逐条返回 `ACK`，校验并应用配置后返回 `APPLIED`。
   回复错误、拒绝或超时均失败；BEGIN 最多等 90 秒。配置完成后才开始性能计时。
2. **感知与动作。** Linux 每帧真实采集、解码、运行 NPU 和红桶检测，经 IVC 发送
   48 字节 `PerceptionResultV2`：序号、时间戳及球/桶的可见性、位置、大小、置信度。
   Zephyr 取最新有效结果决定底盘动作，以独立的 50 ms 周期推进机械臂；逐帧结果
   **没有 ACK**。前两个约 10 秒窗口各须达到 28 FPS。约第 20 秒起仍真实推理，
   但把发送给 Zephyr 的球/桶结果替换为预设场景，以驱动实际控制流程。
3. **执行反馈。** Zephyr 检查机械臂受检终点，随后对三轮依次执行停止、正转、停止、
   反转、停止。每阶段最多 2 秒，读取实际位置与速度；正反转需方向和位置变化正确，
   停车需四组采样的位置稳定、速度在允许范围内。检查位 `checks=31` 仅在周期、终点、
   停车命令、双向运动和最终停车反馈都完成后才成立。错误会锁定本次 CI 失败。
4. **完成回复。** 感知程序至少运行 62 秒后发送 `CONTROL_FINISH`，等待最多 10 秒；
   期间继续发送末尾场景，避免输入断流。Zephyr 返回 `CONTROL_RESULT`，状态为
   `pending`、`failed` 或 `success`，附本次 session、配置 CRC、完成周期、检查位和错误码。
   旧会话回复不能充当成功；失败回复、错误检查位或等不到成功回复都会使 CI 失败。
5. **最终判定。** Linux 关闭摄像头、释放模型并关闭 IVC 后，才生成唯一的
   `DUAL_PICK_APPLICATION_PASS`。脚本核对两个窗口、匹配的控制结果、至少 62 秒时长、
   清理及零退出码；board 入口还须 `sync` 成功，才输出独占一行的
   `DUAL_PICK_CI_PASS guest=linux-zephyr`。中间日志和 Zephyr 串口文字不能代替最终结果。

NPU 调用错误、IVC 发送失败或丢帧、执行器读写/反馈错误都会失败；进入 CI 控制模式后，
未完成时约 350 ms 输入断流也会停车并锁定失败。少量坏帧允许剔除并记录，连续采集/解码
失败达到 10 次则退出，坏帧不计入有效 FPS。这项测试验证真实感知性能、双向通信和
受控执行器反馈；预设场景允许模拟夹持成功，不能证明识别准确率、真实抓球入桶、
地面行驶、长期稳定性或硬件急停。

本 SD 场景使用板卡类型 `OrangePi-5-Plus-DualGuest-robot`，与 StarryOS + Zephyr 场景顺序
共用同一块板。感知程序、Zephyr 镜像和 CI 脚本须配套更新。Linux 还需为上述
固定命令配置限定的免密 sudo。board 入口以 `shell_check_steps` 逐步注入短命令。

## 6. 如需迁移到 eMMC

当前仓库只提供 SD 配置。如需增加 eMMC，建议在 `dual-linux-zephyr/emmc/`
中放置三个 eMMC 专用文件，不要把第二个 `build-*.toml` 放到当前 SD
wrapper 中。

### 6.1 Linux VM 配置

复制 `linux-smp1-sd.toml` 为 `emmc/linux-smp1-emmc.toml`。保留内存布局、镜像路径、
`disabled` 资源和 `robot-ivc` 不变，只修改 `[kernel].cmdline` 中的 Linux 客户机
根设备：

```toml
# SD
cmdline = "root=/dev/mmcblk1p2 ..."

# eMMC（历史板卡编号，仍需实机确认）
cmdline = "root=/dev/mmcblk0p2 ..."
```

### 6.2 AxVisor build 配置

复制当前 `build-aarch64-unknown-none-softfloat.toml` 为
`emmc/build-aarch64-unknown-none-softfloat.toml`。`features`、`log`、`target` 和 `[env]`
保持不变，只把 `vm_configs` 的 Linux 项指向 eMMC VM 配置：

```toml
vm_configs = [
  "test-suit/axvisor/normal/board-orangepi-5-plus/dual-linux-zephyr/emmc/linux-smp1-emmc.toml",
  "test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr/zephyr-smp1.toml",
]
```

### 6.3 board 配置

复制 `board-orangepi-5-plus-dualguest-robot.toml` 为
`emmc/board-orangepi-5-plus-dualguest-robot-emmc.toml`，并修改 `board_type` 为 eMMC
板卡在 ostool 中的独立类型，例如：

```toml
board_type = "OrangePi-5-Plus-DualGuest-robot-emmc"
```

SD board 配置的 `uboot_cmd` 第一项显式设置了 SD 宿主 bootargs。eMMC 版应将该项
删除，使用板卡已确认的 eMMC 默认 bootargs；如果板卡没有可用的默认值，则将
它改为实测的 eMMC 宿主根分区。后续 UART6 时钟、管脚寄存器、
`shell_prefix`、`shell_check_steps[].shell_cmd`、`success_regex`、`fail_regex` 和 `timeout` 均保持不变。

Linux VM 的 `[kernel].cmdline` 控制客户机根分区，board 配置中的 U-Boot
bootargs 控制 AxVisor 宿主根分区，两者需分别确认，不能互相代替。
