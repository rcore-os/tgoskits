# ArceOS / Starry OS 移植 VisionFive 2 (昉·星光2)

## 参考文档

- [昉·星光 2](https://doc.rvspace.org/Doc_Center/visionfive_2.html)
- [昉·星光 2单板计算机快速参考手册](https://doc.rvspace.org/VisionFive2/Quick_Start_Guide/index.html)
- [昉·星光 2单板计算机软件技术参考手册](https://doc.rvspace.org/VisionFive2/SW_TRM/index.html)
- [昉·惊鸿-7110启动手册](https://doc.rvspace.org/VisionFive2/Developing_and_Porting_Guide/JH7110_Boot_UG/index.html)

## 快速开始

### 1. 编译 Starry OS

```bash
$ cd StarryOS
# 正常编译
$ make vf2
# 推荐开启 link time optimizations
$ make vf2 LTO=y
# 调试日志输出
$ make vf2 LOG=debug
```

内核镜像文件位于 `StarryOS_visionfive2.bin`

### 2. 准备 SD 卡

TODO: 初始化分区表、OpenSBI 等

### 3. 准备启动分区

1. 创建一个 FAT32 格式的分区
2. 将编译好的内核镜像文件拷贝至根目录并重命名为 `kernel`
3. 创建文件 `vf2_uEnv.txt`，写入以下内容：
   ```
   boot2=load mmc 1:3 $kernel_addr_r kernel; go $kernel_addr_r
   ```
   其中 `1:3` 代表 1 号卡槽的分区 3，请根据实际情况调整；环境变量中 `kernel_addr_r` 应为 `0x40200000`，如果不是的话请在此文件中进行覆盖

到这里，应当可以成功进入 ArceOS 并打印调试信息

### 4. 准备文件系统

1. 创建一个 ext4 格式的分区，并将“分区名称”设置为 `root`（注意不是卷标）；这里假设创建的分区是 `/dev/sda4`
2. 将 rootfs 刷写到此分区，如：
   ```bash
   sudo dd if=rootfs-riscv64.img of=/dev/sda4 status=progress bs=4M conv=fsync
   ```
   推荐先多次使用 `resize2fs -M xxx.img` 尽可能压缩镜像文件大小以加快刷写速度
3. 更新文件系统大小，扩大到整个分区：
   ```bash
   sudo resize2fs /dev/sda4
   ```

至此，应当可以进入 Starry OS 的命令行进行交互，不过由于目前还未实现网卡驱动，所以无法使用 apk 安装软件包，可以在创建基础文件系统后，将需要运行的软件拷贝至文件系统。

## TODO

- PLIC 无法工作

## 移植说明

该平台与 QEMU RISC-V Virt Machine 相似程度较高，本仓库的适配代码与 `axplat-riscv64-qemu-virt` 也仅存在一些配置上的差异。以下对其简单说明：

1. 配置文件：参考 [Linux 中的设备树配置文件](https://github.com/torvalds/linux/blob/master/arch/riscv/boot/dts/starfive/jh7110-common.dtsi)，修改 axconfig.toml，主要有以下内容需要调整：
   - `phys-memory-base`/`phys-memory-size`：物理内存区域
   - `kernel-base-paddr`/`kernel-base-vaddr`：内核代码基地址
   - `mmio-ranges`：这里我们为了方便直接把整个 `0x0` 到 `0x4000_0000` 都配置成了 MMIO 区域
   - `pci-*`：目前没有实现 PCI
   - `timer-frequency`：时钟频率
   - `rtc-paddr`/`plic-paddr`/`uart-paddr`/`uart-uirq`/`sdmmc-paddr`：外设相关配置

   `timer-irq` 和 `ipi-irq` 在 RISC-V 架构上是固定的。

2. 启动：最初我们在 U-Boot 中使用 booti 指令启动，因此伪装了 [Linux 启动镜像文件头](https://www.kernel.org/doc/html/v5.8/riscv/boot-image-header.html)，即代码 `boot.rs` 中 `.ascii  \"MZ\"` 这一段。后来我们才发现可以直接用 `go` 指令更方便地直接进行跳转，不过这一段文件头因为可以兼容两种启动方式就保留了下来。
3. CPU 配置：VIsionFive 2 所使用的 JH7110 处理器有四个 64 位 RISC-V CPU（支持 rv64gc，编号为1-4）和一个 32 位 RISC-V CPU（支持rv32imfc，编号为0），因此 U-Boot 不会在 0 号核上启动，ArceOS 也无法在它上面运行。然而 ArceOS 许多设计都假定 cpuid 从 0 开始，我们把 cpu id 从原始的 1-4 映射到 0-3 来解决这个问题。
4. 存储设备驱动：[Simple SD/MMC Driver](https://github.com/Starry-OS/simple-sdmmc)
