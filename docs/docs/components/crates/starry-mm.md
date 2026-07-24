# `starry-mm`

> 路径：`memory/starry-mm`

StarryOS 的薄 `no_std` Linux VM 策略层。它不复制具体 VFS 或页表 backend；当前公共边界包括：

- RSS、COW charge、VSS 与进程内存统计；
- `RLIMIT_AS` admission；
- Always overcommit（mode 1）与 `Committed_AS` 原子记账；
- 全局 committed-memory charge 与每地址空间资源获取即初始化账本；
- 页表驻留查询和 VMA 权限通过纯数据/capability 输入，不依赖 `ax-hal`。
- `VmFile`、`PageSource` 和 clean-page evictor capability；
- shared page 的分配、借用 retainer 与最后引用释放；
- 仅在 `NoMemory` 后执行一次有界 clean-page 回收和一次重试。

Starry kernel 保留 `AddrSpace`、具体 `PageTableCursor` backend、VFS/page-cache/memfd adapter、syscall 参数、errno 和进程接线；策略数据与计数不在 kernel 重复维护。
