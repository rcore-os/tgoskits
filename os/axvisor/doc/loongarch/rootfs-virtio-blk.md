# LoongArch Rootfs And Virtio Block

本文记录 LoongArch Linux guest 在 QEMU quick-start 中使用 virtio-blk 磁盘 rootfs 的方式。

## 当前方式

当前 LoongArch Linux quick-start 只支持 virtio-blk rootfs。VM 配置使用：

```toml
dtb_path = "/guest/linux/linux-loongarch64-qemu-rootfs-smp1.dtb"
cmdline = "root=/dev/vda rootfstype=ext4 rootwait rw init=/init console=ttyS0 ..."
```

启动路径是：

```text
Axvisor 加载 LoongArch Linux kernel 和 DTB
  -> setup_bootinfo 构造 LoongArch Linux bootinfo
  -> QEMU 暴露 virtio-blk rootfs 磁盘
  -> Linux 枚举 virtio-blk 并挂载 /dev/vda
  -> 执行 rootfs 中的 /init
```

这条路径依赖 guest 侧 virtio-blk 设备链路，quick-start 不再准备 initramfs 配置。

## Rootfs 设备链路

典型 cmdline 是：

```text
root=/dev/vda rw console=ttyS0
```

guest 需要走完整设备链路：

```text
Linux virtio-blk driver
  -> PCI/virtio device discovery
  -> virtqueue setup
  -> disk I/O
  -> device interrupt
  -> hypervisor injects guest IRQ
  -> guest handles completion
  -> mount /dev/vda
```

当前 LoongArch quick-start 已经把这条链路作为 Linux guest 的唯一启动路径。

## Initramfs 状态

initramfs 曾用于早期 bring-up，用来绕开：

- guest PCI 枚举；
- virtio-blk device discovery；
- virtqueue I/O；
- virtio-blk 完成中断；
- 外部设备中断注入 guest；
- guest idle 后等待设备中断唤醒。

现在 quick-start 已经切换到 rootfs，`--linux` 和 `--linux-rootfs` 都使用 `linux-loongarch64-qemu-rootfs-smp1.toml`。仓库里的通用 ramdisk/initrd 加载逻辑仍可被其他配置使用，但 LoongArch QEMU quick-start 不再维护 initramfs 作为启动模式。

QEMU runtime 配置中挂载：

```toml
"virtio-blk-pci,drive=disk0"
```

VM cmdline 使用 `root=/dev/vda`，因此 Linux guest 启动依赖 virtio-blk rootfs。
