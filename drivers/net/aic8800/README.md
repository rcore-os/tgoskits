# aic8800

`aic8800` 是完全 `no_std`、OS 无关的 AIC8800 Driver Core。核心对象
`AicDevice` 由单一 owner 持有，只通过 `&mut self`、显式单调时间、IRQ
快照、SDIO 完成、控制请求和 TX 数据推进。

核心不会创建线程或任务，不注册 IRQ，不读取设备树，不持有 OS 锁，也不
调用 sleep/yield。每次 `advance` 只返回一个可观察动作：提交或中止 SDIO
事务、等待 IRQ、等待绝对截止时间、发布完成/收包/发送回收事件，或进入
空闲状态。

SDIO 命令编码、CCCR/FBR/CIS、Function 生命周期和 CMD52/CMD53 均由
`sdmmc-protocol::sdio::SdioCard` 负责。RDIF 网络适配位于可选的 `rdif`
模块，仅由 `rdif` feature 编译；默认构建仍只有 Driver Core。构建脚本只在
构建机下载并校验固定哈希固件，不是目标驱动的网络或运行时依赖。

源码按领域目录组织：

```text
src/
  lib.rs
  device/
    mod.rs
    owner.rs
    model.rs
    progress.rs
    control.rs
    data_plane.rs
    link.rs
    mailbox.rs
    request.rs
    startup/
      mod.rs
      dc.rs
      firmware.rs
      vendor.rs
  firmware/
    mod.rs
    dc_config.rs
    dc_lmac_rf.rs
    dc_rf.rs
  lmac.rs
  profile.rs
  protocol.rs
  registers.rs
  rdif/
    mod.rs
    device/
      mod.rs
      endpoints/
        mod.rs
        control.rs
        device.rs
        irq.rs
        startup.rs
    owner/
      mod.rs
      operation.rs
      output.rs
      progress.rs
  rx.rs
  tx.rs
  wpa2.rs
```

无需 QEMU 的私有状态机测试放在对应源文件末尾；`tests/std.rs` 只通过公开
API 验证 crate 契约。真实 SDIO/Wi-Fi、FDT 和硬中断链路必须使用 axtest
或 SG2002 实板验证。
