# aka-rk3588 tennis robot

该应用提供可直接运行的 RK3588 网球机器人 AArch64 用户态程序，并通过 StarryOS
板级 app 会话将构建包提供给 Orange Pi 5 Plus。仓库内的预编译资产包含程序、RKNN
模型、`librknnrt.so`、默认配置和运行脚本，不需要在运行 app 前另外下载或编译
`aka-rk3588` 源码。

源码版本记录在 [`source.env`](source.env) 中。预编译程序由固定提交
`5528e074a05b01219276cfd5c50f5d88c2880123` 在 Orange Pi Jammy 上使用 GCC 11
原生编译，最高依赖 `GLIBC_2.34`。`prebuild.sh` 会先按
[`SHA256SUMS`](prebuilt/aarch64/SHA256SUMS) 校验仓库资产，再生成
`aka-rk3588.tar.gz`。构建环境和二进制哈希见
[`SOURCE`](prebuilt/aarch64/SOURCE)。

## 安全演示

```bash
cargo xtask starry app board -t aka-rk3588 \
  -b OrangePi-5-Plus-robot
```

默认演示只执行一次摄像头采集和 RKNN 网球识别，不驱动车轮和机械臂。程序通过本次
会话下载到 `/tmp/aka-rk3588`，成功标志为：

```text
AKA_RK3588_DEMO_PASSED
```

目标根文件系统需兼容 AArch64 GLIBC 2.34，并提供 UVC、libusb 和 TurboJPEG 运行库，
同时启用 Orange Pi 5 Plus 的 USB、摄像头和 RKNPU 能力。当前预编译包面向机器人
共享的 Jammy/StarryOS 根文件系统；不能直接放入仅含 musl 的通用 Alpine rootfs。

## 部署到共享根文件系统

需要持久安装并运行完整捡球流程时，先启动开发板默认 Linux：

```bash
cargo xtask starry app board -t aka-rk3588 \
  -b OrangePi-5-Plus-robot \
  --linux-stage
```

命令会打印 `aka-rk3588.tar.gz` 的会话下载地址并进入 Linux。不要直接覆盖一台已经
校准的机器人配置；先备份 `/home/orangepi/robot/aka-rk3588/config`，再下载并解压构建
包。每台机器人的 `lekiwi_calibration.json` 和 `lekiwi_pick_config.txt` 应继续使用各自
实机校准和调试结果。

持久部署后，Linux 和 StarryOS 使用同一根文件系统中的程序：

```bash
cd /home/orangepi/robot/aka-rk3588
export LD_LIBRARY_PATH="$PWD/lib:${LD_LIBRARY_PATH:-}"
./run_vision_once.sh
./run_lekiwi_full.sh
```

`run_lekiwi_full.sh` 会实际控制底盘和机械臂，只能在已校准且周边安全的机器人上运行。

## 更新预编译程序

更新时应在目标兼容的 AArch64 Linux 环境中，从新的固定提交重新原生编译。随后替换
`prebuilt/aarch64` 中对应文件，同时更新 `source.env`、`SOURCE` 和 `SHA256SUMS`。
不要使用分支名或 `HEAD` 表示源码版本，也不要提交要求高于目标根文件系统的 GLIBC
版本的二进制。
