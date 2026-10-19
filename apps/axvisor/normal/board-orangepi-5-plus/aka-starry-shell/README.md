# StarryOS + Zephyr AKA 联调

## 1. 用途

在 Orange Pi 5 Plus 机器人上，由 StarryOS 执行相机采集、NPU 感知和控制流程，Zephyr shell 运行 AKA 控制端，驱动经 UART6 连接的车轮和机械臂等控制设备。验证一轮业务完整执行及 IVC 数据交互。

本例需要配套相机、UART6 连接的控制设备和这台机器的真实校准参数。仅进入两个客户机的 shell 不构成完成测试，完整通过标准见第 5 节。

## 2. 准备镜像和业务文件

客户机镜像和文件系统由 tgosimages 构建。在 tgosimages 仓库执行：

```bash
./build.sh platform orangepi-5-plus all
```

构建前按 tgosimages 的说明准备 AKA 运行包及真实校准文件。将产物部署到板卡原生 Linux，保持以下位置：

```text
/guest/
├── starry/orangepi-5-plus
├── zephyr/orangepi-5-plus
├── zephyr/orangepi-5-plus.dtb
└── rootfs-aarch64-orangepi-jammy-1.img
```

`-1.img` 是 StarryOS 独立使用的 ext4，文件系统内部应有：

```text
/opt/aka/
├── bin/tennis-perception
├── run_dual_pick_ci_once.sh
└── 配套模型、运行库、控制脚本和机器校准文件
```

Zephyr 二进制和 DTB 使用同一次构建的产物。StarryOS 使用支持 VirtIO 块设备的 Error 日志镜像；本目录的 [starry-guest-build.toml](starry-guest-build.toml) 是其应用构建配置。

宿主 AxVisor 保留物理存储和网络；StarryOS 使用独立 VirtIO 磁盘，访问相机和 NPU；Zephyr 使用 UART6 控制设备。资源及设备分配以 [starry-smp1.toml](starry-smp1.toml) 和 [zephyr-smp1.toml](zephyr-smp1.toml) 为准。两者各使用一个 vCPU，分别选择 MPIDR `0x400` 和 `0x200`。

## 3. 使用 Cargo 启动 AxVisor

先在构建机设置板卡服务地址，将主机和端口占位符替换为自己的环境值：

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
```

以下命令在构建机的 TGOSKits 仓库根目录执行。交叉编译器须位于 PATH，板卡服务中应有连接上述设备的机器人。

先确认板卡可用：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
ostool board ls --server "$BOARD_SERVER"
```

手动测试需要交互配置。下面从正式板卡配置中保留 U-Boot 初始化及客户机启动参数，只移除自动输入步骤和自动测试总超时：

```bash
app=apps/axvisor/normal/board-orangepi-5-plus/aka-starry-shell
# 去掉自动输入步骤和自动测试总超时，生成交互版板卡配置
sed '/^\[\[shell_check_steps\]\]/,$d' \
  "$app/board-orangepi-5-plus-aka-shell.toml" \
  | sed '/^timeout[[:space:]]*=/d' > /tmp/aka-starry-shell-manual.toml

cargo xtask axvisor board \
  --config "$app/build-aarch64-unknown-none-softfloat.toml" \
  --board-config /tmp/aka-starry-shell-manual.toml \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

该命令构建并启动 AxVisor，客户机使用板端已部署的镜像。启动参数 `axvisor.vm_start=1,2` 同时启动：

| VM | 客户机       | 用途               |
| -- | ------------ | ------------------ |
| 1  | StarryOS     | 感知及控制工作负载 |
| 2  | Zephyr shell | AKA 控制端         |

看到 `axvisor:/$` 后，在 AxVisor 输入：

```text
vm list
vm console 2
```

## 4. 在客户机中运行

### 4.1 启动 Zephyr 控制端

控制端是 StarryOS 侧工作负载的对端，要先于 4.3 节的工作负载启动。`aka run` 在独立线程中启动机器人控制程序——aka-rk3588 仓库的 `orangepi_robot_control`，构建时链入 Zephyr shell 镜像：它从 IVC 通道接收 StarryOS 侧的指令，经 UART6 驱动车轮和机械臂，启动后先等待 30 秒稳定期（`settle_ms=30000`）再进入控制循环。`aka status` 查询该线程的状态和结果码。在 `zephyr:~$` 提示符下执行：

```text
aka run
aka status
```

预期分别包含：

```text
aka_run=0
aka_state=running result=0
```

控制端启动后，按 Ctrl+X，松开后按 h，返回 AxVisor。然后输入：

```text
vm console 1
```

### 4.2 检查 StarryOS 运行环境

在 StarryOS 的 `root@starry:~#` 下执行：

```bash
# 客户机只挂 VirtIO 磁盘，物理 eMMC/SD 不应出现在 StarryOS 中
test -b /dev/blk0
echo BLOCK_EXIT=$?
test ! -e /dev/mmcblk0
echo STORAGE_EXIT=$?
# AKA 主程序位于客户机 ext4 内
test -f /opt/aka/bin/tennis-perception
echo APP_EXIT=$?
cd /opt/aka
```

三项返回码均应为 0。`/dev/blk0` 是 StarryOS 客户机的 VirtIO 磁盘；`/opt/aka` 是这个客户机文件系统中的目录。

### 4.3 执行一轮 AKA

继续在 StarryOS 中执行：

```bash
# FPS 参数必须为正；0.01 只用于确认感知管线有数据产出
./run_dual_pick_ci_once.sh --min-fps 0.01
rc=$?
sync
printf '\nAKA_RESULT exit=%s\n' "$rc"
```

等待程序自行运行结束。本例按业务流程完成情况判定。

## 5. 查看结果

成功时检查脚本打印类似：

```text
DUAL_PICK_CI_CHECK_PASS min_fps=0.01
AKA_RESULT exit=0
```

完整判定包括31项控制检查、两个感知窗口、工作负载至少运行62000 ms、应用正常退出、清理完成及 IVC dropped=0。检查脚本返回非零、设备初始化失败或中途退出，都不能记为通过。

完成后按 Ctrl+X、h 返回 AxVisor，按 Ctrl+A、x 退出板卡会话。再次运行前使用板卡复位流程：该场景包含设备直通和 IVC 端点，`vm stop/reset/delete` 不能替代板级复位来回收设备。

## 6. 自动执行

需要自动输入和判定时，在构建机仓库根目录执行：

```bash
cargo xtask axvisor app board -g normal \
  -t board-orangepi-5-plus/aka-starry-shell \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

运行器完成与上面相同的控制端启动和 StarryOS 工作负载，最终要求独占行 `AKA_SHELL_CI_PASS`，且外层命令返回 0。

## 7. 配置文件

本目录的配置覆盖宿主构建、板级自动测试和两台客户机，手动流程从同一份板卡配置裁剪而来：

| 文件                                                                                  | 作用                                     |
| ------------------------------------------------------------------------------------- | ---------------------------------------- |
| [build-aarch64-unknown-none-softfloat.toml](build-aarch64-unknown-none-softfloat.toml) | 构建 AxVisor，注册两个客户机             |
| [board-orangepi-5-plus-aka-shell.toml](board-orangepi-5-plus-aka-shell.toml)           | 板级初始化、启动参数、自动命令和结果判定 |
| [starry-smp1.toml](starry-smp1.toml)                                                   | StarryOS 镜像、磁盘及设备分配            |
| [zephyr-smp1.toml](zephyr-smp1.toml)                                                   | Zephyr 镜像、DTB、虚拟控制台和 UART6     |
| [starry-guest-build.toml](starry-guest-build.toml)                                     | tgosimages 构建 StarryOS 时使用的配置    |
