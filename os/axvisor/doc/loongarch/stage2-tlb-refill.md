# LoongArch Stage-2 TLB Refill

本文只记录 LoongArch stage-2 映射和 TLB refill 的特殊处理。

## 当前实现

通用 nested page fault 路径会先补二阶段页表映射。大多数架构在页表更新后，可以依赖硬件下一次访问重新 page walk。

LoongArch 当前额外实现了架构 hook：

```rust
self.fill_stage2_tlb(gpa, hpa, access_flags);
Ok(true)
```

也就是在 nested page fault 已经解析出 `gpa -> hpa` 后，主动填充 LoongArch stage-2 TLB/refill 相关项。

## 为什么需要

当前 LoongArch LVZ/stage2 路径不能完全依赖“更新二阶段页表后硬件自动重新 page walk”这个假设。某些缺页路径上，如果只更新 `address_space` 里的二阶段页表，guest 再次进入后仍可能继续触发 TLB refill 或转换异常。

因此 LoongArch 需要在 VM exit 处理后主动补一条 stage-2 TLB entry，让 guest 能继续访问。

## 与其他架构区别

- x86 EPT：通常由硬件 EPT walker 根据更新后的 EPT 继续翻译。
- ARM Stage-2：通常由硬件 stage-2 walker 继续翻译。
- RISC-V G-stage：通常由硬件 G-stage walker 继续翻译。
- LoongArch：当前实现需要在部分路径上手动填充 stage-2 TLB/refill 项。

## 后续工作

1. 确认 READ/WRITE/EXECUTE 权限都能正确填充。
2. 确认 huge page level 软件标记位不会污染物理地址。
3. 确认 guest direct-map VA 到 GPA 的转换覆盖 Linux 常见地址区间。
4. 确认映射权限变化或解除映射时是否需要失效旧 TLB 项。
5. 用 Linux boot、initramfs 访问、用户态执行、设备 MMIO 访问分别验证。
