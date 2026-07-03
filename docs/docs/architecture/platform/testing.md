---
sidebar_position: 6
sidebar_label: "验证与调试"
---

# 验证与调试

平台层改动会影响启动、内存、IRQ、timer、console 和设备发现。验证应从最小编译检查开始，再逐步到 QEMU 或板卡 bring-up。

## 最小检查

平台模板或平台选择变更后，先运行：

```bash
cargo fmt --check
cargo metadata --locked --no-deps --format-version 1
cargo test -p axbuild --lib
cargo xtask clippy --package axbuild
```

`axplat-custom` 是不可发布模板，不是 workspace package，也不是 `ax-hal` 的内置依赖。复制并改名为真实平台后，应在根 workspace 和本地 `ax-hal` 增加对应 package / optional dependency / feature，然后再运行：

```bash
cargo check -p axplat-myplat --features irq,smp
AX_PLATFORM_CRATE=axplat_myplat cargo check -p ax-hal --features axplat-myplat,irq,smp
```

如果修改了具体平台 crate，还应对该 crate 运行 targeted clippy：

```bash
cargo xtask clippy --package axplat-myplat
cargo xtask clippy --package ax-hal
```

## QEMU 验证

动态平台路径优先使用 `cargo xtask`：

```bash
cargo xtask arceos test qemu --arch aarch64
cargo xtask starry test qemu --arch riscv64
cargo xtask axvisor test qemu --arch x86_64 --test-group normal
```

自定义平台只有在补齐启动入口、链接脚本、console、timer、内存和 IRQ 后，才应宣称可启动 QEMU。模板 `axplat-custom` 只演示接口形状，不参与 workspace 编译，也不代表能运行。

## 常见失败信号

| 现象 | 优先检查 |
| --- | --- |
| 链接重复 `__*If_*` 符号 | 是否同时链接了两个 `ax-plat` 实现 crate |
| 没有任何串口输出 | 入口符号、早期 console、地址转换、重定位窗口 |
| 清 BSS 后崩溃 | linker script、段地址、boot stack、保留内存范围 |
| 开 timer 后卡住 | timer IRQ 编号、ACK/EOI、one-shot 重编程顺序 |
| 设备找不到 | FDT/ACPI/PCI probe 是否注册，Static probe 是否执行 |
| IRQ handler 不触发 | IRQ source resolver、domain、enable、affinity、controller EOI |

## 调试建议

- 先确认最后一个可靠输出点：固件入口、退出 UEFI、开启 MMU、进入 `ax_plat::call_main()`、`init_early()`、`init_later()`。
- 早期启动问题优先用物理地址标记或最小串口输出，不要依赖完整日志系统。
- 检查最终链接镜像的符号和段布局，确认运行时地址而不只是编译期地址。
- QEMU 可加 `-d int,cpu_reset,guest_errors` 或 debug stub，但临时调试参数不要留在提交中。
