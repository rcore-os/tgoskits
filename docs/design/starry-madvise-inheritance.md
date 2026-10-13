# madvise fork 继承策略

## 问题与范围

匿名私有映射需要在 fork 后保留地址和权限、但不把父进程内容交给子进程。
目前 `MADV_WIPEONFORK` 和 `MADV_KEEPONFORK` 被 syscall 入口拒绝。
本改动仅补齐这两项建议，不引入新 MM、swap、userfaultfd 或体系结构后端。

## 语义依据

以 Linux v6.12 的 [madvise_behavior](https://github.com/torvalds/linux/blob/v6.12/mm/madvise.c)
和 [dup_mmap](https://github.com/torvalds/linux/blob/v6.12/kernel/fork.c) 为基准：
WIPEONFORK 仅允许匿名私有映射，KEEPONFORK 清除继承策略；子进程保留策略，
但不复制被擦除区间的页。父进程内容不变，CLONE_VM 不进入地址空间复制。
跨空洞的请求保留已更新的映射并返回 ENOMEM；文件和共享映射拒绝 WIPEONFORK。
Starry 尚不提供 MAP_DROPPABLE，故本改动不引入其特殊规则。

## 方案与所有权

沿 `sys_madvise → AddrSpace::advise_vma_policy → VmaAdvicePolicy` 发布策略。
复用不可变 VMA 根、拆分合并和 mremap 元数据传播，而不另设区间登记表。
fork 在未发布的子地址空间中创建独立的匿名 COW 后端，不复制该区间的
PTE、MappingSlot 或父页所有者，也不对父区间施加 fork 写保护。
子空间准备失败沿既有 unpublished-clone 清理路径回收。

已物化大页跨越 WIPEONFORK 边界时，先用现有 split deposit 和 mutation receipt
在同一事务内拆为基本页。否则相邻正常继承区间可能复制包含被擦除内容的大叶。
未发布失败恢复旧 VMA 根和大页；已发布但 TLB 未确认时保留发布状态和既有
隔离机制。无大页拆分时仍使用无 TLB 目标的元数据提交。

不实现会继续拒绝该用户行为；整套移植另一个 MM 或维护平行策略表会引入
不必要的所有权冲突；仅把返回码改为成功则不能保证子进程隔离。因此选择扩展
现有策略和 fork 边界。接口与 syscall 编号不变，无持久格式或兼容层。
本改动属于 MM 高风险功能，合入前仍需领域维护者审查，不能以测试代替。

## 验证与交付边界

`qemu/system/madvise-inheritance` 通过直接 syscall、fork 和再次 fork 观察
清零、父子隔离、KEEPONFORK、权限拆分和 mremap 传播，以及空洞部分进展和
不支持的映射类型。源码内内核测试另验证真实大页拆分与叶所有权。
测试不使用 x86_64 硬编码 syscall 号；页表和 TLB 继续由现有架构后端执行。
回滚为撤销本提交，新建进程回到原有不支持行为，无磁盘状态迁移。
