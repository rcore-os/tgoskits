# 当前源码唤醒优化阶段检查点

## 1. 只读路径审计

### 1.1 唤醒事务

`resume877-status.json` 记录对 `b292a098bb` 的只读审计。当前同核 futex 的唤醒请求在用户态返回路径消费，同核通知不发送 IPI。两次 Fair 虚拟时间更新之间有 `enqueue_task` 改变运行队列状态，不能凭表面重复删除其中一次。本轮未找到能证明语义等价且足以消除约 5.5 us 差距的源码改动，因此未改生产代码，也没有新增 full20。

### 1.2 上下文桥接

`resume878-status.json` 将现有四 crate PGO profile 的热点计数与候选 ELF 机器码核对。`ContextOps::enter` 的热路径已经由 fat LTO 展开为 DAIF、`SP_EL0` 和抢占深度操作，没有待消除的间接回调。计数不是独占耗时；本轮拒绝仅因热点计数高就重写桥接层，未重新构建或上板。

## 2. 广覆盖 PGO 预检

### 2.1 训练镜像

`resume879` 使用相同 `b292a098bb` 源码、关闭 cpufreq 的十项 feature、相同训练负载与顶层 binary 临时 exporter，探索在四 crate 以外增加目标依赖的 PGO 覆盖。`wrapper.py` 和 `wrapper.jsonl` 保存首次训练的实际 rustc 选择；其中 `someboot`、`somehal`、`ax_sync`、`starry_kernel` 均被插桩。`build-profgen.log` 证明训练构建完成，`export-profile-layout.json` 从 ELF 重建出 36790 条记录和 1762168 字节计数段。训练 ELF SHA256 为 `be6e0538df16fb98fcd2c53954dd5112caf29d9db4aa40af44f629e1f0af9998`，bin SHA256 为 `fd14a91c0ee38e74d32bcefae95fa7f2a93012d1dd2340bacdb2e2d602e2ccac`；镜像本体只保存在本地实验目录，未纳入 PR。

### 2.2 启动异常与结论边界

`run1/serial.log` 保留第一次上板的同步异常：ESR `0x96000035`、FAR `0x040267f8`、物理 PC `0x02c3059c`。对应训练 ELF 虚拟 PC `0xffffffff80c3059c` 位于 `someboot::arch::elx::switch_to_elx`，指令为对 profile 计数器执行 `ldxr x9, [x8]`。这表明首次广覆盖训练镜像在早期启动阶段就访问了尚不可用的计数器地址；没有进入 benchmark，不能作为性能候选。

`resume879` 仍在进行，下一次训练需在 generate/use 两侧同时排除早期启动和平台 crate，再核对 profile 身份及无插桩候选。这里没有新的 p50、p99 或 p99.9 结论。已提交的 `resume876` 两次有效候选 full20 仍仅 10/20 项达到冻结 Linux RT 的 90%，最差 OTHER `thread_futex_same_cpu` 为 56.861%。PR 继续 Draft；三次有效候选、全部同源码尾延迟回退小于 3%、生产构建复现及新 head CI 均未完成。
