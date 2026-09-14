# 用户可执行页的缓存同步

## 1. 可执行 PTE 的发布

### 1.1 数据可见不代表指令可见

Starry 的 ELF 使用惰性文件/COW 映射。文件读取、COW 拷贝和预填充先通过内核直接映射写入页框，再把页框发布为用户可执行 PTE。页表锁、release/acquire、TLBI 和异常返回不能替代 AArch64 数据缓存到 PoU 的清理与指令缓存失效。

Linux 基准是 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。arm64 的 [`__set_ptes_anysz`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/include/asm/pgtable.h) 先调用 `__sync_cache_and_tags`，由 [`__sync_icache_dcache`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/mm/flush.c) 同步可执行 folio，再写 PTE。这条缓存协议也适用于 PREEMPT_RT；它不是调度锁的替代品。

### 1.2 页所有者承担同步

[`PageObject::prepare_executable_mapping`](../../os/StarryOS/kernel/src/mm/aspace/objects.rs) 借用现有 `FrameLease`，验证待映射物理范围属于该页，缺页/预填充事务保留拥有型页框或文件缓存 pin，mprotect 的 MappingSlot 与 MM 变更锁排除驱逐，直到缓存维护完成。借用型 FrameLease 本身不承担外部页缓存的生命周期证明。普通 RAM 的可执行映射执行 `clean_dcache_range_to_pou`，随后执行 `flush_icache_all`；非执行映射和 DEVICE/UNCACHED 映射不进入该路径。不增加页面引用计数或缓存完成标志，因而没有新的失效状态需要与文件写入协调。

[`AddrSpace::apply_prepared_page_fault`](../../os/StarryOS/kernel/src/mm/aspace/mod.rs) 在 PTE 前像复核通过后、安装或替换 PTE 前同步。同步依据最终 PTE 的 EXECUTE 权限，不能只依据本次缺页是否属于取指；用户可能先读可执行文件页，再执行它。COW、文件和共享内存的预填充与克隆发布也在写 PTE 前同步；mprotect 使用已捕获的叶子前像和现有 MappingSlot 页所有者，在授予执行权限前同步。取消或安装失败可以留下无害的缓存维护结果，不发布未完成的 PTE。

[`AArch64::flush_icache_all`](../../components/axcpu/src/arch/aarch64/asm.rs) 使用 `ic ialluis; dsb ish; isb`。这与 Linux arm64 的 Inner Shareable 指令缓存失效范围一致；其他 CPU 返回用户态时的异常返回提供 context synchronization。它不使用 IPI 回调，因此不在页表临界区等待需要取得该锁的远端任务。RISC-V 保留其原有的每次返回用户态 `fence.i`；本次没有将 VisionFive2 的停滞归因于 AArch64 的缓存机制。

## 2. 缺陷证据与边界

### 2.1 实机回归

[`syscall-test-exec-cache`](../../test-suit/starryos/qemu/system/syscall-test-exec-cache/src/main.c) 建立 memfd 的共享可执行映射，执行返回 42 的指令，解除映射，通过 pwrite 写入返回 99 的指令并验证文件读回内容，然后在原虚拟地址重新映射并执行。第二次执行前不由用户程序清缓存：内容由内核写入，新的可执行文件映射必须观察已经完成的文件写入。这不同于要求用户程序自行同步的任意 JIT 自修改代码。

同一 OrangePi-5-Plus-1 实机、同一程序获得以下结果。修复只增加缓存维护，没有改变文件页身份、内容、映射选择或测试判据。

| 内核 | 结果 | 证据 |
| --- | --- | --- |
| `857f05f59c`，未修复 | `expected=99 actual=42`；`STARRY_EXEC_CACHE_FAILED errno=0` | 会话 `21987e27-2ad2-4e17-852a-9e0a09da4c08` |
| 本次缓存修复 | `expected=99 actual=99`；`STARRY_EXEC_CACHE_OK errno=0` | 会话 `43e6431f-b5c3-425d-8086-2399519f6a31` |

板卡用例通过会话文件下发静态程序，不修改 Linux 根文件系统。现有 OrangePi CI 命令 `cargo xtask starry test board --board orangepi-5-plus` 自动发现 [`exec-cache`](../../test-suit/starryos/board-orangepi-5-plus/exec-cache/board-orangepi-5-plus.toml)。QEMU 的 system 聚合用例发现同一 C 源码，验证实际 mmap/缺页/执行路径；QEMU 通过不能单独证明物理缓存维护正确。

### 2.2 原始停滞与未证明事项

原版内核在 OrangePi-5-Plus-3 已复现进入用户态后 300 秒超时，网络 DHCP/ARP 仍运行；本次新增回归第一次尝试也在执行程序之前复现同类停滞，不能算缓存回归的红例。缓存修复后，固定 -3 的原始 native-network-smoke 通过，包含网络测试成功标记。

以上证据证明可执行页缓存缺陷及其修复，但尚不能证明此前所有启动停滞均由它造成。VisionFive2 使用另一体系结构，其最新未含本修复的 CI 已通过；DMA 完成、文件系统和调度停滞需要各自的证据，不能仅凭日志 NUL、一次通过或统一增加屏障来归因。未把本地非 RT Linux 构建当作 PREEMPT_RT 性能基线。

### 2.3 系统调用兼容性对照

本表只评价可执行页发布时的缓存行为，不评价这些系统调用的全部 ABI。编号依次为 AArch64/RISC-V64/LoongArch64/x86_64；`—` 表示该体系结构没有独立入口。除明确的 AArch64 共享文件重新取指回归外，尚未获得的完整路径验证标记为无法确认。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| mmap(PROT_EXEC, MAP_SHARED)/222/222/222/9 | [8cd9520d arm64 set_ptes](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/include/asm/pgtable.h) | 新的可执行文件映射观察已完成的文件写入 | sys_mmap → 文件缺页事务 → PageObject 同步 → PTE 安装；每 MM 与文件页所有者 | 无法确认 | AArch64 实机同一回归红绿通过；其余架构执行验证待完成 |
| mmap(PROT_EXEC, MAP_PRIVATE)/222/222/222/9 | [8cd9520d arm64 cache sync](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/mm/flush.c) | 私有文件页/COW 拷贝须在可执行 PTE 发布前同步 | sys_mmap → CowBackend → fault/populate → PageObject 同步；每 MM 页所有者 | 无法确认 | 已检查缺页与预填充调用点；完整 COW 可执行拷贝回归待完成 |
| mprotect/226/226/226/10 | [8cd9520d mprotect](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/mprotect.c) | 授予执行权限前执行体系结构 PTE 缓存同步；不承诺任意用户 JIT 写入免清缓存 | sys_mprotect → protect_with_reported_flags → 保留 MappingSlot 页 → 同步 → PTE 权限提交 | 无法确认 | 新路径已静态核对；既有 W^X 回归待新提交验证 |
| execve/221/221/221/59 | [8cd9520d binfmt_elf](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | ELF 文件页缺页后可取到新装载指令 | sys_execve → load_user_app/map_elf → CowBackend 缺页同步；进程 MM 替换 | 无法确认 | OrangePi-3 原始网络用例通过并包含真实程序装载；完整 exec 矩阵待 CI |
| execveat/281/281/281/322 | [8cd9520d exec](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | 路径/FD 解析后，新映像同样遵守可执行页同步 | sys_execveat → 公共装载链 → CowBackend 缺页同步 | 无法确认 | 入口参数处理未修改；完整 FD/flags 路径待 CI |
| clone/220/220/220/56 | [8cd9520d fork](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 复制 MM 时保持子进程可执行页内容可见；CLONE_VM 保持共享 | sys_clone → clone 事务 → clone_map/后续 COW → PTE 前同步 | 无法确认 | 新同步不修改 CLONE_VM 或发布事务；完整 clone 矩阵待 CI |
| clone3/435/435/435/435 | [8cd9520d clone3](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | clone_args 校验后遵守相同 MM 复制缓存规则 | sys_clone3 → 公共 clone 事务 → 页后端 | 无法确认 | 结构体参数与错误顺序未改；完整入口验证待 CI |
| fork/—/—/—/57 | [8cd9520d fork](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 独立 MM 的子进程映射保持可执行内容 | sys_fork → 公共 clone 事务 → clone_map | 无法确认 | x86_64 独立入口验证待 CI |
