# LoongArch64 LVZ: GINTC HWI Passthrough 导致 Guest UART 控制台卡死

## 现象

在 Axvisor (LoongArch64 QEMU-LVZ) 上以 passthrough 模式运行 Linux guest 时，
guest 控制台在执行若干命令后会**完全冻结**——不再响应任何输入输出，但 guest 内核本身并未崩溃。

**复现条件**：
- VM 配置：`passthrough_devices = [["/"]]`，`interrupt_mode = "passthrough"`
- 在 guest 内启动高频 shell 循环（如 `while true; do echo "..."; usleep 500000; done`）
- 同时手动输入命令
- 通常在执行数十条命令后触发冻结

## 根因

`gintc_set_hwi_passthrough(0xff)` 在**每次 VM entry** 时被调用
（调用链：`vcpu_run()` → `vm.run_vcpu()` → `vcpu.bind()` → `enable_guest_mode()`），
该函数写 GINTC 寄存器时，其内部逻辑会清除 **HWIS（HWI Status）** 中的 pending 位，
导致在此间隙到达的 UART 中断被静默丢弃。

### GINTC 寄存器结构 (CSR 0x52)

| 位域 | 名称 | 含义 |
|------|------|------|
| [7:0] | HWIS | HWI 中断 pending 状态（只读/写 1 清除） |
| [15:8] | HWIP | HWI passthrough 使能掩码 |
| [23:16] | HWIC | 写 1 清除对应 HWIS 位 |

`gintc_set_hwi_passthrough()` 的**原始实现**同时写入了 HWIC=0xFF：

```rust
// 原始代码（有 bug）
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
    gintc &= !GINTC_HWIC_MASK;   // ← 准备写入 HWIC
    gintc |= (mask << GINTC_HWIP_SHIFT) & GINTC_HWIP_MASK;
    gintc |= (mask << GINTC_HWIC_SHIFT) & GINTC_HWIC_MASK; // ← HWIC=0xFF，清除所有 HWIS！
    write_gintc(gintc);
}
```

写入 HWIC 位会清除对应的 HWIS pending 位。当 HWIC=0xFF 时，所有 8 条 HWI 线的 pending 状态被一次性清除。

### 触发时序

```
  Guest 运行中          Host 处理 VM exit           Guest 重新进入
  ──────────────      ─────────────────────      ─────────────────
  1. Timer 中断 ──→    2. VM exit (timer)
                       3. 处理 timer exit
                          (此时 guest IE=0)
                       4. UART 中断到达
                          → LVZ 拦截，记录在
                            GINTC HWIS 中
                            (pending)
                       5. bind() → enable_guest_mode()
                          → gintc_set_hwi_passthrough(0xff)
                          → 写 GINTC，HWIC=0xFF
                          → HWIS pending 位被清除！
                                                  6. Guest 恢复运行
                                                     但 UART 中断已丢失
                                                     Shell 阻塞在 UART I/O
                                                     → 进入 idle 循环
```

步骤 5 是关键：每次 VM entry 都重置 GINTC，清除了步骤 4 中 pending 的 UART 中断。

### 为什么高频操作更容易触发

Timer VM exit 大约每 ~500μs 发生一次。UART 中断频率与 shell 输出成正比。
命令越频繁，UART 中断越密集，在 timer VM exit 的处理窗口中
命中"guest IE=0 且 UART 中断到达"的竞态窗口的概率越大。
一旦某个 UART 中断被丢弃，guest 的串口驱动就会卡在等待中断唤醒的状态，
后续的 UART 中断也可能因为同样的竞态被反复丢弃。

## 证据

### 1. 心跳调试：vcpu 循环持续运行但 guest 卡在 idle

在 vcpu loop 中加入每 500 次 VM exit 打印 guest 状态的心跳：

```
DBG heartbeat VM[1] VCpu[0] exit#14500 sepc=0x9000000000cbdd8c crmd=0xb4
    ectl=0x71c1d estat=0x800 era=0x9000000000cbdd8c
DBG heartbeat VM[1] VCpu[0] exit#15000 sepc=0x9000000000cbdd8c crmd=0xb4
    ectl=0x71c1d estat=0x800 era=0x9000000000cbdd8c
...
（冻结后心跳仍在输出，sepc/era 固定不变）
```

- `era=0x9000000000cbdd8c` = guest idle 循环地址（WFI/idle）
- `crmd=0xb4` = guest 中断已开启 (IE=1)
- `estat` 只有 timer 位 (0x800) 和 0 之间切换，**从未出现 HWI 位**
- 心跳持续输出 → vcpu 循环正常，问题在 guest 侧

### 2. GINTC 寄存器值确认 HWIS=0

修正 CSR 编号后读取 GINTC (CSR 0x52)：

```
DBG #500  gintc=0x0000ff00 est=0x800 ...  (正常时)
DBG #4000 gintc=0x0000ff00 est=0x0   ...  (卡住时)
```

- `gintc=0x0000ff00`：HWIP=0xFF（passthrough 全开），**HWIS=0x00**（无 pending HWI）
- 零 HWI VM exit — 从未出现非 timer 的 IRQ exit 日志

这说明 UART 中断在 EIOINTC/PCH-PIC 层面到达了，但在 GINTC 层面被丢弃。

### 3. 定期 HWI 注入可恢复 guest

在 vcpu loop 中每 50 次 exit 注入一次 HWI2（UART 中断线）后：

```
[37] 00:00:36 ----------------------------------------
ls
bin  etc  linuxrc  root  sys
[38] 00:00:37 ----------------------------------------
pwd
/
[39] 00:00:38 ----------------------------------------
（命令正常响应，不再卡死）
```

通过 `pulse_hwi(2)` 向 guest 注入模拟的 UART 中断后，guest 恢复正常。
这证实了 **guest 的中断处理逻辑本身没问题，问题在于中断信号在 GINTC 层面丢失**。

### 4. 禁用大页后问题消失（二次确认）

该 bug 还与大页支持代码交互：大页 PTE 中 `LEVEL` 字段（bits[14:13]）
与 `PHYS_ADDR_MASK`（bits[47:12]）重叠，导致 `lddir`/`ldpte` 硬件页表遍历
时物理地址被破坏。大页映射启用后 TLB refill 更频繁，放大了 GINTC 竞态窗口，
使卡死更快出现。但即使禁用大页，仅靠高频命令仍可触发，只是概率更低。

## 修复

### 修复 1：GINTC passthrough 只配置一次

将 `gintc_set_hwi_passthrough(0xff)` 从 `enable_guest_mode()`
（每次 VM entry 调用）移到 `init_hv()`（仅 vcpu setup 时调用一次）。

**文件**：`components/loongarch_vcpu/src/vcpu.rs`

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

**文件**：`components/loongarch_vcpu/src/registers.rs`

同时修复 `gintc_set_hwi_passthrough()` 本身，不再写入 HWIC 位：

```rust
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
    // 不再写 HWIC：写 HWIC 会清除 HWIS pending 位
    gintc |= (mask << GINTC_HWIP_SHIFT) & GINTC_HWIP_MASK;
    write_gintc(gintc);
}
```

### 修复 2：大页 PTE 不存储 LEVEL 到物理地址位域

**文件**：`components/page_table_multiarch/page_table_entry/src/arch/loongarch64.rs`
**文件**：`components/axaddrspace/src/npt/arch/loongarch64.rs`

LoongArch 大页 PTE 只设置 `GH` 位标识大页，不存储 LEVEL 到内存 PTE 中。
`lddir` 硬件遍历页表时根据当前遍历层级自动识别大页，无需软件在 PTE 中记录 level。

```rust
// 大页 PTE 只设 GH，不写 LEVEL
fn new_page_with_level(paddr: PhysAddr, flags: MappingFlags, level: usize) -> Self {
    Self::new_page(paddr, flags, level > 0)  // level > 0 时设 GH 位
}
```

## 原理：LoongArch LVZ 中断路径

```
  外设 (UART) → PCH-PIC → EIOINTC → CPU 中断控制器 → CPU
                                    ↕
                               GINTC (LVZ)
                                    ↓
                     HWIP 使能 → passthrough 直达 guest
                     HWIS 记录 pending 状态
                     HWIC 写 1 清除 pending
```

在 passthrough 模式下（HWIP=0xFF），外部硬件中断不引起 VM exit，
直接注入 guest。但如果 timer 中断导致 VM exit（`gcfg_set_toti(false)`），
则在 host 处理期间到达的 HWI 会被 LVZ 暂存到 GINTC HWIS 中。
问题在于 host 在恢复 guest 前重写 GINTC 时把 HWIS 清掉了。

## 经验教训

1. **不要在每次 VM entry 时重写中断控制器配置**。静态配置（如 passthrough mask）
   只需初始化一次，重复写入会带来意外的副作用。

2. **GINTC 的 HWIC 是 write-to-clear 语义**。写 HWIC 位会清除对应的 HWIS pending 位，
   这是 LoongArch LVZ 架构的设计，但容易被忽视。

3. **调试方法**：通过心跳日志区分"vcpu 循环停止"和"vcpu 循环运行但 guest 卡死"，
   是定位虚拟化 bug 的关键第一步。前者指向 host/hypervisor 问题，后者指向 guest 侧问题。

4. **多个 bug 可以交互放大**：GINTC 丢失中断本身是一个低概率竞态，
   但大页映射引入的 TLB refill 异常增加了 VM exit 频率和内存映射错误，
   使卡死更容易复现。
