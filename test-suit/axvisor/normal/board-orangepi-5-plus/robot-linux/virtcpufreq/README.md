# Linux virt-cpufreq 客户机资产

这个目录提供带 `qemu,virtual-cpufreq` backport 的 Linux 客户机配置模板。它与生产的
`orangepi-5-plus-6.1.99` 分开，要求 `/guest/linux/orangepi-5-plus-6.1.99-virtcpufreq`
已经安装 `CONFIG_CPUFREQ_VIRT`、`CONFIG_CPU_FREQ_GOV_PERFORMANCE` 以及匹配的 FDT
驱动。没有该内核时使用上一级 `perf30` host-performance profile。

## 1. 资产和设备声明

`linux-smp1-emmc-virtcpufreq.toml` 固定 Linux 到 A76 big0 的宿主 CPU4，并声明
`virtual-cpufreq` 的单 vCPU bank。`domain = 1` 是当前 RK3588 `ax-runtime` 的 big0
domain ordinal，性能表是归一化值，不是物理频率；VM 准备阶段会用宿主已验证 OPP 表
覆盖模板中的 `performance_table`，而运行时 thermal/DSU 上限由宿主 worker 继续约束。
迁移板卡前要重新核对实际 domain 顺序。

```toml
[[devices.virtual]]
id = "guest-cpufreq"
model = "virtual-cpufreq"
domain = 1
vcpu_count = 1
performance_table = [256, 512, 768, 1024]
```

启动后必须观察 `scaling_driver=virt-cpufreq` 和
`scaling_governor=performance`。缺少任一项时只说明客户机前端未启用，不能把宿主
高性能 profile 伪装成客户机调频。

## 2. 构建前检查

在板卡或 `/guest` 资产准备好前不要把该目录加入默认 robot smoke。需要检查以下
SHA256 与内核配置，再用 `build-aarch64-unknown-none-softfloat.toml` 作为显式构建入口：

- patched Linux kernel 的 commit 和 SHA256；
- `initramfs.cpio`、rootfs、RKNN runtime、模型和 `aka-rk3588-virtual` SHA256；
- AxVisor commit 以及 host CPUFreq domain/OPP 记录。

QEMU 或静态配置解析只能证明 FDT 节点和 MMIO ABI，不能证明 RK3588 OPP、RKNPU、DMC
或真实摄像头 FPS。
