---
sidebar_position: 5
sidebar_label: "ROCK 4D"
---

# ROCK 4D Linux Guest

ROCK 4D 的客户机 kernel 和 DTB 由构建机准备，`prepare_guest_payload()` 将其放入宿主 initramfs，Axvisor 统一按文件路径加载。VM 配置 `os/axvisor/configs/vms/rock-4d/linux-smp1.toml` 使用 `${env:AXVISOR_GUEST_ASSETS}` 指定本地目录。

## 1. 客户机资源

Linux kernel 使用 Radxa BSP 的 `linux/rk2410` profile 构建，guest DTS 由 TGOSKits 维护。资源打包前必须非空；缺少资源时报错，不继续使用旧归档。

### 1.1 Linux kernel

先设置工作区和本地资产目录，再构建 BSP raw ARM64 `Image`。有意保留 BSP 本地修改时使用其 `--dirty` 选项。

```bash
export TGOSKITS_ROOT=/path/to/tgoskits
export ROCK4D_BSP=/path/to/rock-4d/bsp
export AXVISOR_GUEST_ASSETS="${TGOSKITS_ROOT}/tmp/axvisor/guest-assets"
cd "${ROCK4D_BSP}"
./bsp linux rk2410
mkdir -p "${AXVISOR_GUEST_ASSETS}/linux"
cp .src/linux/arch/arm64/boot/Image "${AXVISOR_GUEST_ASSETS}/linux/rock-4d"
test -s "${AXVISOR_GUEST_ASSETS}/linux/rock-4d"
```

### 1.2 Guest DTB

`dtc` 编译仓库中的 guest DTS；输出目录结构与 VM TOML 的打包输入路径一致。

```bash
cd "${TGOSKITS_ROOT}"
dtc -I dts -O dtb \
  -o "${AXVISOR_GUEST_ASSETS}/linux/rock-4d-linux-smp1.dtb" \
  os/axvisor/configs/vms/rock-4d/linux-smp1.dts
test -s "${AXVISOR_GUEST_ASSETS}/linux/rock-4d-linux-smp1.dtb"
```

## 2. 板卡启动

`axvisor test board` 复用 FIT 或 HTTP Boot 的宿主归档交接，不再要求先通过 SSH 将启动镜像写入板卡根文件系统。客户机可写磁盘的路径及所有权仍由 VM 配置定义。

### 2.1 执行用例

本地资源准备完成后，通过现有板卡服务运行完整 U-Boot、Axvisor、Linux 客户机链路。

```bash
cd "${TGOSKITS_ROOT}"
cargo xtask axvisor test board --board rock-4d-linux
```

### 2.2 根文件系统选择

没有宿主 `root=` 时直接从 initramfs 加载客户机，不切根。显式磁盘根时，`builtin::prepare_root()` 将 `/guest/builtin` 整包替换到磁盘、刷盘并切根；只读根、空间不足或安装失败会停止启动。板卡实际交接及根切换需要在对应硬件上验证，QEMU 结果不能代替。
