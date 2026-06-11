# LoongArch64 LVZ: GINTC HWI Passthrough 导致 Guest UART 控制台卡死

本文记录 LoongArch LVZ passthrough HWI 的一个关键问题：不能在每次 VM entry
时重写 GINTC passthrough 配置，否则可能清掉 guest pending HWI，导致 Linux guest
串口输入输出卡死。

## 现象

在 LoongArch64 QEMU-LVZ 上运行 Linux guest 时，guest shell 曾出现过随机卡死：

- guest 内核没有崩溃；
- vCPU 循环还在运行；
- 控制台不再响应输入输出；
- 高频串口输出或连续输入更容易触发。

## 根因

GINTC 的 HWI 相关位里，`HWIC` 是 write-one-clear 语义。写入 `HWIC` 会清除
对应的 `HWIS` pending 位。

如果 `gintc_set_hwi_passthrough(0xff)` 在每次 VM entry 都执行，并且写入了
`HWIC=0xff`，就会出现这个竞态：

```text
guest 运行
  -> timer 导致 VM exit
  -> host 处理 exit
  -> UART HWI 在此期间到达，暂存在 GINTC HWIS
  -> host 准备重新进入 guest
  -> 重写 GINTC，并写入 HWIC
  -> HWIS pending 位被清掉
  -> guest 丢失 UART 中断
```

guest 串口驱动依赖 UART 中断推进收发。中断被清掉后，guest 可能停在 idle 或等待 I/O
完成的位置，看起来就是 shell 卡死。

## GINTC 位语义

| 位域 | 名称 | 含义 |
|------|------|------|
| [7:0] | HWIS | HWI 中断 pending 状态（只读/写 1 清除） |
| [15:8] | HWIP | HWI passthrough 使能掩码 |
| [23:16] | HWIC | 写 1 清除对应 HWIS 位 |

问题代码的核心是把 passthrough mask 同时写到了 `HWIP` 和 `HWIC`：

```rust
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
    gintc &= !GINTC_HWIC_MASK;
    gintc |= (mask << GINTC_HWIP_SHIFT) & GINTC_HWIP_MASK;
    gintc |= (mask << GINTC_HWIC_SHIFT) & GINTC_HWIC_MASK;
    write_gintc(gintc);
}
```

## 修复

修复分两部分。

第一，GINTC passthrough mask 只在 vCPU 初始化时配置一次，不放在每次 VM entry
都会执行的路径里：

**文件**：`virtualization/loongarch_vcpu/src/vcpu.rs`

```rust
// init_hv() — 只调用一次
fn init_hv(&mut self, config: LoongArchVCpuSetupConfig) {
    self.init_vm_context(config);
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        crate::registers::gintc_set_hwi_passthrough(0xff);
    }
}

// enable_guest_mode() — 不再调用 gintc_set_hwi_passthrough
unsafe fn enable_guest_mode(&self) {
    // ... 其他 GID/TGID/PGM/GCFG 配置 ...
    // gintc_set_hwi_passthrough 已移除
    prmd::set_pie(true);
}
```

第二，`gintc_set_hwi_passthrough()` 只更新 `HWIP`，不写 `HWIC`：

```rust
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
    // 不再写 HWIC：写 HWIC 会清除 HWIS pending 位
    gintc |= (mask << GINTC_HWIP_SHIFT) & GINTC_HWIP_MASK;
    write_gintc(gintc);
}
```

## 中断路径

```
  外设 (UART) → PCH-PIC → EIOINTC → CPU 中断控制器 → CPU
                                    ↕
                               GINTC (LVZ)
                                    ↓
                     HWIP 使能 → passthrough 直达 guest
                     HWIS 记录 pending 状态
                     HWIC 写 1 清除 pending
```

在 passthrough 模式下，外部硬件中断可以直接进入 guest。但如果 host 正在处理
timer exit 或其他 VM exit，这段时间到达的 HWI 可能会先挂在 GINTC 的 `HWIS` 中。
因此 VM entry 前的寄存器恢复路径必须避免清除 `HWIS`。

## 注意事项

1. 静态中断控制器配置不要放在高频 VM entry 路径中反复写。
2. 写 `HWIC` 会清 pending HWI，不能把它当成普通配置位。
3. 如果 guest shell 卡在 idle 但 vCPU loop 仍在运行，要优先检查 pending HWI 是否被丢弃。

## 相关文件

- `virtualization/loongarch_vcpu/src/vcpu.rs`
- `virtualization/loongarch_vcpu/src/registers.rs`
