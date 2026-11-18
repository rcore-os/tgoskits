# APLIC 延期完成测试

本用例通过 `somehal::irq` 和 `ax_plat::irq::riscv64_hv` 的生产接口，验证客户机取得物理中断完成责任后，APLIC 的屏蔽与退休约束。它运行真实 ArceOS 调度器和双核 QEMU APLIC/IMSIC，不创建状态机替身。

## 1. 来源生命周期

`src/aplic.rs` 独占 QEMU virt 未接线的来源 42，以边沿触发配置到 CPU1。IRQ sink 只记录接收核和计数，把完成责任保留给固定在 CPU0 的测试任务。

### 1.1 完成责任转移

CPU0 在关闭本地中断的范围内，向独占的未接线来源 43 和 44 注入 MSI，经生产 `begin_irq()` 与 `defer_riscv_external_completion()` 取得真实令牌。用例检查直接丢弃完成回滚、槽冲突返回原令牌且不释放槽内来源、取出并显式完成、重复取出为空及槽复用。轮询只用于等待真实 MSI 到达，有三秒上限，不进入生产中断路径。移除令牌析构中的完成动作后，同一用例在“dropping a claim did not complete rollback”处失败。

### 1.2 延期与宿主屏蔽

宿主不能禁用客户机持有的来源。收到中断后检查实际 SETIE 位已清零，延期期间的宿主启用请求也必须返回 `Busy`。完成后重新开放，第二次完成返回 false。

单独屏蔽空闲来源后，客户机仍可申请该来源。宿主已登记但尚未启用的共享来源要等最后一个动作释放后才能分配。共享登记失败也必须归还本次预留。

### 1.3 退休与迟到完成

下一次完成前调用 `deactivate_guest_external_source()` 退休来源。迟到完成可以消费既有责任，但不得再次打开 SETIE。重新启用和重新激活均须返回 `Busy`。

### 1.4 首次开放后的目标固定

来源 45 通过普通宿主控制接口配置。首次启用前可以在 CPU0 与 CPU1 间选择目标；向 CPU0 发出 MSI 后，即使屏蔽来源，改投 CPU1 也必须返回 `Busy`，等价的 `Any` 与 CPU0 设置仍可成功。原 MSI 必须由 CPU0 正常领取；领取完成后仍不得迁移，也不能通过客户机激活接口改投 CPU1。该检查保护 `AplicSourceState::ever_enabled` 的持久约束，恢复旧实现时在“masked source migrated with an old MSI outstanding”处失败。

### 1.5 批量申请失败

先通过 `activate_guest_external_sources()` 同时申请空闲的来源 43 和宿主的来源 45。申请须返回 `Busy`，并且 43 仍处于屏蔽状态。随后申请 43 和 44，两路都须成功。逐个启用再回滚会退休 43，导致第二次申请失败。

## 2. 执行入口

构建、发现和 QEMU 执行共用项目运行器；RISC-V CPU 合约 CI 同时选择本 drivers 用例，panic 和超时都会使外层命令失败。

### 2.1 定向运行

在仓库根目录执行下面的入口，运行器从 `suite.toml` 选择 `riscv-aia` 批次的构建与 QEMU 配置。`src/aplic.rs::run()` 完成所有断言后返回，由统一薄入口输出当前批次与选择对应的 `DRIVERS_BATCH_OK`；成员中的 `APLIC_DEFERRED_STATE_OK` 只保留为诊断标记。

```bash
cargo xtask arceos test qemu --arch riscv64 --test-group drivers --test-case aplic
```

### 2.2 证明边界

此用例验证边沿注入的真实中断链路及完成交错。客户机物理磁盘 IRQ 与持续电平重采样由 `test-suit/axvisor/normal/qemu-aplic-passthrough` 验证。设备 DMA 停稳和 K3 实板时序需要独立证据。
