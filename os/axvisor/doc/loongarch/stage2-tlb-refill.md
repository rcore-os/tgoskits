# LoongArch Stage-2 TLB Refill

本文记录 LoongArch LVZ 下 AxVisor 处理 guest stage-2 缺页和 TLB refill 的关键路径。

## 背景

LoongArch guest 运行时，host 会把 stage-2 页表根写入 `CSR_PGDL/CSR_PGDH`，并把
`CSR_TLBRENTRY` 指向 AxVisor 提供的 guest TLB refill 入口。

正常路径是：

```text
guest 访问地址
  -> LVZ/stage-2 TLB miss
  -> _guest_tlb_refill_vector 使用 lddir/ldpte 查 stage-2 页表
  -> 查到映射后 tlbfill
  -> ertn 回 guest
```

这条路径在汇编里完成，不需要每次 miss 都回到 Rust。

## 缺页路径

如果 `_guest_tlb_refill_vector` 通过 `lddir/ldpte` 查不到 stage-2 映射，会进入
VM exit。Rust 侧根据 `host_tlbrera` 判断这是 TLB refill 触发的 exit：

```text
_guest_tlb_refill_vector
  -> stage-2 PTE miss
  -> vmexit_trampoline
  -> handle_exception_sync()
  -> AxVCpuExitReason::NestedPageFault
  -> AxVM::handle_nested_page_fault()
  -> address_space.handle_page_fault()
```

也就是说，LoongArch 当前没有在 Rust 侧手工写入某条 TLB entry。真正的补映射动作
仍然发生在通用 `address_space.handle_page_fault()` 中；补完后，guest 再次进入时
由 TLB refill 汇编重新 page walk 并 `tlbfill`。

## Guest Fault 与 Stage-2 Fault

LoongArch Linux 自己也会产生 guest 虚拟地址缺页。AxVisor 需要区分两类情况：

- guest 自己页表缺页：注入回 guest 的 TLB refill 或普通异常；
- stage-2 映射缺失：返回 `NestedPageFault`，由 AxVisor 补二阶段映射。

当前判断主要依赖：

- guest 是否开启分页；
- fault 地址是否是 guest direct-map 地址；
- fault 地址是否落在已知 guest RAM/MMIO 范围；
- 是否来自 host TLB refill 入口。

这样可以避免把 Linux guest 自己应该处理的页表异常误当作 stage-2 缺页。

## 大页注意点

LoongArch stage-2 PTE 的物理地址位域不能混入软件 level 标记。当前实现只用
`GH` 位表示 huge page：

```text
is_huge == true -> set GH
physical address -> only kept in PHYS_ADDR_MASK
```

不要把 page-table level 写入 PTE 物理地址位域，否则 `lddir/ldpte` 硬件遍历时
会读到被污染的物理地址，进而导致异常或反复缺页。

## 与其他架构区别

- x86 EPT、ARM Stage-2、RISC-V G-stage 通常直接依赖硬件 walker。
- LoongArch 当前需要显式安装 guest TLB refill 入口，并在该入口中执行
  `lddir/ldpte/tlbfill`。
- stage-2 miss 仍走 AxVisor 的通用 nested page fault 补映射逻辑。

## 后续工作

1. 继续验证 READ/WRITE/EXECUTE fault 的权限判断。
2. 覆盖更多 guest direct-map VA 到 GPA 的转换场景。
3. 确认映射权限变化或解除映射时的 TLB 失效策略。
4. 用 Linux boot、rootfs I/O、用户态执行、设备 MMIO 分别验证。
