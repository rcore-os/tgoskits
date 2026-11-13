# APLIC 延期完成测试

本用例通过 `somehal::irq` 和 `ax_plat::irq::riscv64_hv` 的生产接口，验证客户机取得物理中断完成责任后，APLIC 的屏蔽与退休约束。它运行真实 ArceOS 调度器和双核 QEMU APLIC/IMSIC，不创建状态机替身。

## 1. 来源生命周期

`src/aplic.rs` 独占 QEMU virt 未接线的来源 42，以边沿触发配置到 CPU1。IRQ sink 只记录接收核和计数，把完成责任保留给固定在 CPU0 的测试任务。

### 1.1 完成责任转移

CPU0 在关闭本地中断的范围内，向独占的未接线来源 43 和 44 注入 MSI，经生产 `begin_irq()` 与 `defer_riscv_external_completion()` 取得真实令牌。用例检查直接丢弃完成回滚、槽冲突返回原令牌且不释放槽内来源、取出并显式完成、重复取出为空及槽复用。轮询只用于等待真实 MSI 到达，有三秒上限，不进入生产中断路径。移除令牌析构中的完成动作后，同一用例在“dropping a claim did not complete rollback”处失败。

### 1.2 延期与禁用

收到中断后检查实际 SETIE 位已清零；延期期间先禁用再启用，来源必须继续保持屏蔽。完成后才重新开放，第二次完成返回 false。下一轮先禁用再完成，来源仍保持屏蔽。

### 1.3 退休与迟到完成

第三轮在完成前调用 `deactivate_guest_external_source()` 退休来源。迟到完成可以消费既有责任，但不得再次打开 SETIE；重新启用和重新激活均须返回 `Busy`。全部断言完成后才输出 `APLIC_DEFERRED_STATE_OK`。

## 2. 执行入口

构建、发现和 QEMU 执行共用项目运行器；RISC-V CPU 合约 CI 同时选择本 drivers 用例，panic 和超时都会使外层命令失败。

### 2.1 定向运行

在仓库根目录执行下面的入口，运行器从 `suite.toml` 选择 `riscv-aia` 批次的构建与 QEMU 配置。`src/aplic.rs::run()` 完成所有断言后返回，由统一薄入口输出当前批次与选择对应的 `DRIVERS_BATCH_OK`；成员中的 `APLIC_DEFERRED_STATE_OK` 只保留为诊断标记。

```bash
cargo xtask arceos test qemu --arch riscv64 --test-group drivers --test-case aplic
```

### 2.2 证明边界

此用例验证边沿注入的真实中断链路及完成交错，不证明持续电平重采样、设备 DMA 停稳或 K3 实板时序。客户机物理磁盘装配由 `test-suit/axvisor/normal/qemu-aplic-passthrough` 另行验证。删除生产实现中延期使能的保护后，本用例应在“enable bypassed a deferred completion”处确定性失败。
