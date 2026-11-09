# Loongson-3A6000-LL ArceOS Hello

该用例在 Loongson-3A6000-LL 物理桌面机上验证 native UEFI axloader 直接启动
ArceOS Hello。板卡固件从 USB 启动 native EFI launcher/loader；xtask 通过
ostool-server 连接 loader 提供的固件网络 HTTP 接口并上传 ArceOS ELF。
axloader 保留 Boot Services，由 ArceOS 的 `efi_pe_entry` 完成自身重定位和
`ExitBootServices`；不启动 Axvisor、虚拟机或宿主 shell。

```bash
cargo xtask arceos test board --board loongson-3a6000-ll --test-case http-boot
```

构建配置使用单 CPU（`max_cpu_num = 1`），并以
`RDRIVE_ACPI_LOAD_AML=0` 只消费 ACPI 表，不加载 AML。该最小用例不启用
initramfs、文件系统、网络设备或 PCI 功能。

板卡配置向目标传入
`earlycon=ns16550,mmio,0x1fe001e0`，并要求启动输出精确匹配以下两行：

```text
HOST_CMDLINE: earlycon=ns16550,mmio,0x1fe001e0
Hello, world!
```

`ready_to_handoff`、HTTP 上传成功或 loader 网络状态本身不构成通过证据。

2026-11-09 实机验证通过：恢复上游 `acpi 6.1.1` 后，以上命令输出 ArceOS
启动日志、完整 `HOST_CMDLINE` 与 `Hello, world!`，最终报告
`ok: http-boot/loongson-3a6000-ll`。本轮耗时约 218 秒，主要等待自动串口身份匹配。
