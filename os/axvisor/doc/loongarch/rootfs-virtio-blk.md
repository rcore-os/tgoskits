# LoongArch Rootfs And Virtio Block

本文只记录 LoongArch Linux guest rootfs 方式，以及为什么当前 quick-start 先使用 initramfs 而不是 virtio-blk 磁盘 rootfs。

## 当前方式

当前 LoongArch Linux quick-start 使用 ramdisk/initramfs 作为 guest rootfs：

```toml
ramdisk_path = "/guest/linux/initramfs.cpio.gz"
ramdisk_load_addr = 0x0700_0000
cmdline = "root=/dev/ram rw console=ttyS0 ... init=/init ... pci=off ..."
```

启动路径是：

```text
Axvisor 加载 initramfs 到 guest 内存
  -> LoongArch bootinfo 传递 initrd base/size
  -> Linux 解包内存中的 initramfs
  -> 执行 /init
```

这条路径不依赖 guest 块设备。

## 磁盘 rootfs 需要的链路

如果改成磁盘 rootfs，典型 cmdline 是：

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

当前 LoongArch quick-start 没有把这条链路作为可靠启动路径。

## 当前绕开的内容

使用 initramfs 主要绕开：

- guest PCI 枚举；
- virtio-blk device discovery；
- virtqueue I/O；
- virtio-blk 完成中断；
- 外部设备中断注入 guest；
- guest idle 后等待设备中断唤醒。

QEMU runtime 配置里可以挂载：

```toml
"virtio-blk-pci,drive=disk0"
```

但当前 guest cmdline 使用 `root=/dev/ram` 并带有 `pci=off`，因此 Linux guest 启动 rootfs 时不会依赖 `/dev/vda`。

## 后续工作

1. 去掉 `pci=off`，确认 guest 能枚举 PCI host bridge。
2. 确认 virtio-blk 设备能在 guest 中出现为 `/dev/vda`。
3. 使用只读 rootfs 验证基本读 I/O。
4. 验证 legacy IRQ/MSI 的注入和完成路径。
5. 确认 guest idle 时，virtio-blk 完成中断能唤醒 vCPU。
6. 将 cmdline 切换到 `root=/dev/vda rw`。
