# aka-rk3588 tennis robot

该应用用于在 Orange Pi 5 Plus 的 StarryOS 环境中运行 RK3588 网球机器人视觉程序。
StarryOS 直接使用 Linux 预先部署到共享根文件系统
`/home/orangepi/robot/aka-rk3588` 的程序，不依赖 StarryOS 网络下载。

仓库保留已经在 Orange Pi Jammy 上使用 GCC 11 原生编译并完成实机验证的 AArch64
`tennis` 程序。模型、`librknnrt.so`、默认配置和运行脚本由
[`prepare-package.sh`](prepare-package.sh) 从 `aka-rk3588` 固定提交下载，不在本仓库
重复保存。

固定源码版本和 SHA256 记录在 [`source.env`](source.env) 中。当前程序最高依赖
`GLIBC_2.34`，适用于机器人共享的 Jammy/StarryOS 根文件系统，不适用于仅含 musl
的通用 Alpine rootfs。

## 准备部署包

在开发主机执行：

```bash
cd apps/starry/aka-rk3588
./prepare-package.sh
```

脚本下载固定提交源码归档并校验 SHA256，然后用仓库中的预编译 `tennis` 替换源码
归档中的构建产物。生成文件为：

```text
target/aka-rk3588/aka-rk3588.tar.gz
```

已有归档可通过环境变量复用，进行无网络打包：

```bash
AKA_RK3588_SOURCE_ARCHIVE=/path/to/aka-rk3588-5528e074.tar.gz \
  ./prepare-package.sh
```

## Linux 部署

先启动开发板 Linux，将部署包传入开发板。不要覆盖一台已经完成实机校准的机器人
配置；部署前应备份：

```text
/home/orangepi/robot/aka-rk3588/config
```

然后将部署包解压到：

```text
/home/orangepi/robot/aka-rk3588
```

每台机器人的 `lekiwi_calibration.json` 和 `lekiwi_pick_config.txt` 应继续使用各自的
实机校准与调试结果。

## StarryOS 安全演示

完成 Linux 部署后执行：

```bash
cargo xtask starry app board -t aka-rk3588 \
  -b OrangePi-5-Plus-robot
```

默认演示直接从共享根文件系统运行一次摄像头采集和 RKNN 网球识别，不驱动车轮和
机械臂。成功标志为：

```text
AKA_RK3588_DEMO_PASSED
```

完整捡球流程必须在已校准且周边安全的机器人上手动执行：

```bash
cd /home/orangepi/robot/aka-rk3588
export LD_LIBRARY_PATH="$PWD/lib:${LD_LIBRARY_PATH:-}"
./run_lekiwi_full.sh
```

## 更新版本

更新时应使用新的完整提交号和真实源码归档 SHA256，并在目标兼容的 AArch64 Linux
环境重新生成 `tennis`。替换预编译程序后同步更新 `source.env` 中的二进制 SHA256。
不要使用分支名或 `HEAD` 作为下载和构建输入。
