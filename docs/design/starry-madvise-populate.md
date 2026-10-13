# 显式 madvise 页面填充

## 问题与语义

`MADV_POPULATE_READ/WRITE` 让调用者在实际访问前预触发缺页并取得错误，
而不是像 MAP_POPULATE 一样隐藏失败。现有入口拒绝这两项，但 MM 已有
可复用的真实缺页事务；本提交不增加页所有权模型、页表 API 或分配器。

依据 Linux v6.12 的 [madvise_populate](https://github.com/torvalds/linux/blob/v6.12/mm/madvise.c)
和 [faultin_page_range](https://github.com/torvalds/linux/blob/v6.12/mm/gup.c)：
READ 要求可读，WRITE 要求可写且预先解除 COW；空洞返回 ENOMEM，
权限或设备映射不适用返回 EINVAL，后备文件 EOF 等故障返回 EFAULT，
不把这些故障转换为调用者的 SIGBUS。保留已经完成的前缀，遇到第一个
失败即停止，不跳过空洞继续处理后缀。零长度和地址对齐沿 syscall 入口检查。

## 架构与替代方案

`sys_madvise → MmPin::populate_advised_range → plan_page_fault →
resolve_page_fault_plan → prepare/apply/TLB acknowledgement`。
每个基本页的类型/权限检查和缺页计划在同一次 MM 锁持有期间完成。
权限准入读取 `VmaEntry::reported_rights` 中用户请求的权限，而不是为页表扩展的
硬件权限；因此仅 PROT_WRITE 的映射不能接受 POPULATE_READ。
锁外执行分配和文件 I/O，锁内 apply 复核已有 VMA epoch/PTE preimage。
冲突后重新取得计划并复核权限，沿既有取消路径释放未发布资源；
已发布的 TLB 义务沿既有 receipt 完成，不引入另一套页登记表。
调用者提供致命信号检查，在推进和重试时返回 EINTR。

选择复用真实缺页事务而非直接调用持锁的 bulk populate：后者需要在宽锁下
执行文件 I/O，而且整段回滚不符合显式填充保留前缀的接口。
保持不实现或返回成功均不能满足用户预触发缺页的需求；导入其他 MM 的
填充分配器则重复所有权与并发边界。此次不改变普通硬件缺页行为，只提取
其既有 plan 执行阶段供显式填充复用。

同一 COW、文件和共享映射后端跨现有架构复用；新代码没有体系结构分支。
页表编码、缓存同步和 TLB 操作仍属于既有后端。每页有界准备，整体成本
随请求页面数增长；不声明相对于 Linux 或原有 MAP_POPULATE 的性能优势。
无公共 syscall 结构、持久格式或配置迁移。撤销本提交恢复原先不支持行为。

## 必要验证与限制

系统测试 `qemu/system/madvise-populate` 优先复用 Linux v6.12 的
`madv_populate.c`，保留权限、空洞及匿名填充的 16 项核心断言。因 Starry
尚无 pagemap，匿名驻留通过 mincore 观察；soft-dirty 子测试不在此适配范围，
不宣称原版完整自测通过。来源、许可与差异见
[测试适配说明](../../test-suit/starryos/qemu/system/madvise-populate/README.md)。
补充程序只检查上游自测没有覆盖的文件与部分进展语义。
内核测试直接确认 READ 不破坏共享 COW，WRITE 在用户写入前已经建立
独立可写页，取消不发布页修改。沿项目入口运行，宿主 Linux 使用同一组测试。

这属于 MM/用户 ABI 高风险功能，合入仍须领域维护者审查。未实现的
HWPOISON、secretmem 等底层能力不由本提交补齐；不得把其不存在的路径
或仅有构建结果描述为运行时支持。
