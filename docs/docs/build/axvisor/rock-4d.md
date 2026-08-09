---
sidebar_position: 5
sidebar_label: "ROCK 4D"
---
# ROCK 4D Linux Guest

ROCK 4D 的 Linux VM 配置使用 `image_location = "fs"`。Axvisor 会从板卡根文件系统读取以下资产，TGOSKits 的构建和 board test 不会自动生成或安装它们：

```text
/guest/linux/rock-4d
/guest/linux/rock-4d-linux-smp1.dtb
```

Linux kernel 使用 Radxa BSP 的 `linux/rk2410` profile 构建，guest DTB 的源文件由 TGOSKits 仓库维护。运行 ROCK 4D board 用例前，先设置两个工作区路径和板卡连接信息：

```bash
export TGOSKITS_ROOT=/path/to/tgoskits
export ROCK4D_BSP=/path/to/rock-4d/bsp
export ROCK4D_HOST=<board-linux-ip>
export ROCK4D_USER=radxa
```

## 1. 构建 Linux kernel

使用 BSP 构建 raw ARM64 kernel `Image`：

```bash
cd "${ROCK4D_BSP}"
./bsp linux rk2410

test -s .src/linux/arch/arm64/boot/Image
```

如果正在复用 `.src/linux` 中有意保留的本地 kernel 修改，使用 BSP 提供的 `--dirty` 选项重新构建：

```bash
cd "${ROCK4D_BSP}"
./bsp --dirty linux rk2410
```

## 2. 生成 Guest DTB

从 TGOSKits 维护的 DTS 生成 guest DTB：

```bash
cd "${TGOSKITS_ROOT}"

mkdir -p tmp/axvisor/rock-4d
dtc -I dts -O dtb \
  -o tmp/axvisor/rock-4d/rock-4d-linux-smp1.dtb \
  os/axvisor/configs/vms/rock-4d/linux-smp1.dts

test -s tmp/axvisor/rock-4d/rock-4d-linux-smp1.dtb
```

## 3. 部署 Guest 资产

将 BSP kernel 和 guest DTB 一起部署到板卡的 Axvisor guest 目录：

```bash
scp \
  "${ROCK4D_BSP}/.src/linux/arch/arm64/boot/Image" \
  tmp/axvisor/rock-4d/rock-4d-linux-smp1.dtb \
  "${ROCK4D_USER}@${ROCK4D_HOST}:/tmp/"

ssh "${ROCK4D_USER}@${ROCK4D_HOST}" \
  'sudo install -D -m 0644 /tmp/Image /guest/linux/rock-4d && \
   sudo install -m 0644 /tmp/rock-4d-linux-smp1.dtb \
     /guest/linux/rock-4d-linux-smp1.dtb && \
   sudo sync && \
   test -s /guest/linux/rock-4d && \
   test -s /guest/linux/rock-4d-linux-smp1.dtb && \
   sha256sum /guest/linux/rock-4d /guest/linux/rock-4d-linux-smp1.dtb'
```

`bsp`、`dtc`、`scp`、`ssh` 或板端写入失败时必须停止，不能继续使用旧 kernel 或 DTB。

## 4. 运行 Board 用例

部署完成并确认两项资产均非空后，从 TGOSKits 仓库运行：

```bash
cd "${TGOSKITS_ROOT}"
cargo xtask axvisor test board --board rock-4d-linux
```

board 用例覆盖从 U-Boot 启动 Axvisor 到 Linux guest 登录提示的路径，但不修改板卡的持久文件系统。更新 BSP kernel 或 guest DTS 后必须重新执行对应的构建、部署和 `sync` 流程。
